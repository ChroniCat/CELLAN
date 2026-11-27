//! Unified coordinate transformation engine (`CoordTransform`), compile-time
//! exhaustive field-coverage guards for `Sheet` and `Workbook`, and the single
//! authoritative workbook reference/subsystem dispatcher.
//!
//! All four coordinate-mutating operations route through this module:
//! - Whole-axis structural insert/delete (`Structural` -> `AxisShift { band: None }`)
//! - Partial band cell shift (`CellShift` -> `AxisShift { band: Some((lo, hi)) }`)
//! - Contiguous axis block move (`move_rows_wb` / `move_cols_wb` -> `AxisPermute`)
//! - Batch sorted insertions (`insert_subtotal_rows` -> `MultiInsert`)

use crate::model::{Sheet, Workbook};
use crate::refs::{Axis, AxisMove, MappedShift, Shift, permute_index};
use crate::structural::{CellShift, CellShiftConflict, Structural};

/// Compile-time classification of every field on `Sheet` and `Workbook`.
///
/// Used exclusively by `verify_sheet_field_coverage` and
/// `verify_workbook_field_coverage` (which destructure every field with NO `..`
/// wildcard) so adding any new field to `Sheet` or `Workbook` without declaring
/// its coordinate policy fails compilation immediately.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldCoordPolicy {
    /// Holds grid coordinates or cell/range references and MUST participate in
    /// unified coordinate transformation (or explicit `CellShift` preflight guards).
    CoordBound,
    /// Derived cache, dirty flag, revision counter, or save journal maintained
    /// alongside mutations rather than independently coordinate-transformed.
    DerivedOrDirty,
    /// Identity, view setting, protection password, or workbook option carrying
    /// no grid coordinates.
    CoordFree,
}

#[inline]
fn assert_field_policy<T>(_field: &T, _policy: FieldCoordPolicy) {}

/// Compile-time exhaustive field-coverage guard for `crate::model::Sheet`.
/// Adding a field to `Sheet` without classifying it here is a hard compile error.
#[allow(dead_code)]
pub(crate) fn verify_sheet_field_coverage(sheet: &Sheet) {
    let Sheet {
        // ---- CoordFree: identity, view settings & non-coordinate flags ----
        name,
        identity,
        audit_sheet_idx,
        source_part,
        source_needs_dom_repair,
        loading,
        outline_summary_below,
        outline_summary_right,
        outline_symbols,
        tab_color,
        protection,
        hidden,
        very_hidden,
        zoom_scale,
        show_gridlines,
        show_headings,

        // ---- DerivedOrDirty: caches, revisions, dirty flags & save journals ----
        shared_lookup,
        saved_cells,
        source_cells,
        display_style_revision,
        coordinate_revision,
        cleared,
        pictures_dirty,
        edited,
        styles_dirty,
        style_edits,
        col_styles_dirty,
        row_styles_dirty,
        merges_dirty,
        cf_dirty,
        dv_dirty,
        notes_dirty,
        threads_dirty,
        freeze_dirty,
        layout_dirty,
        scenarios_dirty,
        ignored_errors_dirty,
        outline_pr_dirty,
        filter_dirty,
        page_dirty,
        charts_dirty,
        deleted_charts,
        tab_color_dirty,
        tables_dirty,
        protect_dirty,
        protected_ranges_dirty,
        rich_dirty,
        hyperlinks_dirty,
        sparklines_dirty,
        textboxes_dirty,
        hidden_dirty,
        view_dirty,

        // ---- CoordBound: coordinate-bearing storage, metadata & references ----
        cells,
        spilled,
        spill_shapes,
        arrays,
        tables,
        pictures,
        merges,
        cols,
        rows,
        styles,
        col_styles,
        row_styles,
        cf,
        dv,
        notes,
        threads,
        freeze,
        filter,
        hidden_rows,
        hidden_rows_user,
        hidden_cols,
        scenarios,
        scenario_settings,
        ignored_errors,
        row_levels,
        col_levels,
        page,
        charts,
        protected_ranges,
        rich,
        hyperlinks,
        sparklines,
        textboxes,
    } = sheet;

    // CoordFree
    assert_field_policy(name, FieldCoordPolicy::CoordFree);
    assert_field_policy(identity, FieldCoordPolicy::CoordFree);
    assert_field_policy(audit_sheet_idx, FieldCoordPolicy::CoordFree);
    assert_field_policy(source_part, FieldCoordPolicy::CoordFree);
    assert_field_policy(source_needs_dom_repair, FieldCoordPolicy::CoordFree);
    assert_field_policy(loading, FieldCoordPolicy::CoordFree);
    assert_field_policy(outline_summary_below, FieldCoordPolicy::CoordFree);
    assert_field_policy(outline_summary_right, FieldCoordPolicy::CoordFree);
    assert_field_policy(outline_symbols, FieldCoordPolicy::CoordFree);
    assert_field_policy(tab_color, FieldCoordPolicy::CoordFree);
    assert_field_policy(protection, FieldCoordPolicy::CoordFree);
    assert_field_policy(hidden, FieldCoordPolicy::CoordFree);
    assert_field_policy(very_hidden, FieldCoordPolicy::CoordFree);
    assert_field_policy(zoom_scale, FieldCoordPolicy::CoordFree);
    assert_field_policy(show_gridlines, FieldCoordPolicy::CoordFree);
    assert_field_policy(show_headings, FieldCoordPolicy::CoordFree);

    // DerivedOrDirty
    assert_field_policy(shared_lookup, FieldCoordPolicy::DerivedOrDirty);
    assert_field_policy(saved_cells, FieldCoordPolicy::DerivedOrDirty);
    assert_field_policy(source_cells, FieldCoordPolicy::DerivedOrDirty);
    assert_field_policy(display_style_revision, FieldCoordPolicy::DerivedOrDirty);
    assert_field_policy(coordinate_revision, FieldCoordPolicy::DerivedOrDirty);
    assert_field_policy(cleared, FieldCoordPolicy::DerivedOrDirty);
    assert_field_policy(pictures_dirty, FieldCoordPolicy::DerivedOrDirty);
    assert_field_policy(edited, FieldCoordPolicy::DerivedOrDirty);
    assert_field_policy(styles_dirty, FieldCoordPolicy::DerivedOrDirty);
    assert_field_policy(style_edits, FieldCoordPolicy::DerivedOrDirty);
    assert_field_policy(col_styles_dirty, FieldCoordPolicy::DerivedOrDirty);
    assert_field_policy(row_styles_dirty, FieldCoordPolicy::DerivedOrDirty);
    assert_field_policy(merges_dirty, FieldCoordPolicy::DerivedOrDirty);
    assert_field_policy(cf_dirty, FieldCoordPolicy::DerivedOrDirty);
    assert_field_policy(dv_dirty, FieldCoordPolicy::DerivedOrDirty);
    assert_field_policy(notes_dirty, FieldCoordPolicy::DerivedOrDirty);
    assert_field_policy(threads_dirty, FieldCoordPolicy::DerivedOrDirty);
    assert_field_policy(freeze_dirty, FieldCoordPolicy::DerivedOrDirty);
    assert_field_policy(layout_dirty, FieldCoordPolicy::DerivedOrDirty);
    assert_field_policy(scenarios_dirty, FieldCoordPolicy::DerivedOrDirty);
    assert_field_policy(ignored_errors_dirty, FieldCoordPolicy::DerivedOrDirty);
    assert_field_policy(outline_pr_dirty, FieldCoordPolicy::DerivedOrDirty);
    assert_field_policy(filter_dirty, FieldCoordPolicy::DerivedOrDirty);
    assert_field_policy(page_dirty, FieldCoordPolicy::DerivedOrDirty);
    assert_field_policy(charts_dirty, FieldCoordPolicy::DerivedOrDirty);
    assert_field_policy(deleted_charts, FieldCoordPolicy::DerivedOrDirty);
    assert_field_policy(tab_color_dirty, FieldCoordPolicy::DerivedOrDirty);
    assert_field_policy(tables_dirty, FieldCoordPolicy::DerivedOrDirty);
    assert_field_policy(protect_dirty, FieldCoordPolicy::DerivedOrDirty);
    assert_field_policy(protected_ranges_dirty, FieldCoordPolicy::DerivedOrDirty);
    assert_field_policy(rich_dirty, FieldCoordPolicy::DerivedOrDirty);
    assert_field_policy(hyperlinks_dirty, FieldCoordPolicy::DerivedOrDirty);
    assert_field_policy(sparklines_dirty, FieldCoordPolicy::DerivedOrDirty);
    assert_field_policy(textboxes_dirty, FieldCoordPolicy::DerivedOrDirty);
    assert_field_policy(hidden_dirty, FieldCoordPolicy::DerivedOrDirty);
    assert_field_policy(view_dirty, FieldCoordPolicy::DerivedOrDirty);

    // CoordBound
    assert_field_policy(cells, FieldCoordPolicy::CoordBound);
    assert_field_policy(spilled, FieldCoordPolicy::CoordBound);
    assert_field_policy(spill_shapes, FieldCoordPolicy::CoordBound);
    assert_field_policy(arrays, FieldCoordPolicy::CoordBound);
    assert_field_policy(tables, FieldCoordPolicy::CoordBound);
    assert_field_policy(pictures, FieldCoordPolicy::CoordBound);
    assert_field_policy(merges, FieldCoordPolicy::CoordBound);
    assert_field_policy(cols, FieldCoordPolicy::CoordBound);
    assert_field_policy(rows, FieldCoordPolicy::CoordBound);
    assert_field_policy(styles, FieldCoordPolicy::CoordBound);
    assert_field_policy(col_styles, FieldCoordPolicy::CoordBound);
    assert_field_policy(row_styles, FieldCoordPolicy::CoordBound);
    assert_field_policy(cf, FieldCoordPolicy::CoordBound);
    assert_field_policy(dv, FieldCoordPolicy::CoordBound);
    assert_field_policy(notes, FieldCoordPolicy::CoordBound);
    assert_field_policy(threads, FieldCoordPolicy::CoordBound);
    assert_field_policy(freeze, FieldCoordPolicy::CoordBound);
    assert_field_policy(filter, FieldCoordPolicy::CoordBound);
    assert_field_policy(hidden_rows, FieldCoordPolicy::CoordBound);
    assert_field_policy(hidden_rows_user, FieldCoordPolicy::CoordBound);
    assert_field_policy(hidden_cols, FieldCoordPolicy::CoordBound);
    assert_field_policy(scenarios, FieldCoordPolicy::CoordBound);
    assert_field_policy(scenario_settings, FieldCoordPolicy::CoordBound);
    assert_field_policy(ignored_errors, FieldCoordPolicy::CoordBound);
    assert_field_policy(row_levels, FieldCoordPolicy::CoordBound);
    assert_field_policy(col_levels, FieldCoordPolicy::CoordBound);
    assert_field_policy(page, FieldCoordPolicy::CoordBound);
    assert_field_policy(charts, FieldCoordPolicy::CoordBound);
    assert_field_policy(protected_ranges, FieldCoordPolicy::CoordBound);
    assert_field_policy(rich, FieldCoordPolicy::CoordBound);
    assert_field_policy(hyperlinks, FieldCoordPolicy::CoordBound);
    assert_field_policy(sparklines, FieldCoordPolicy::CoordBound);
    assert_field_policy(textboxes, FieldCoordPolicy::CoordBound);
}

/// Compile-time exhaustive field-coverage guard for `crate::model::Workbook`.
/// Adding a field to `Workbook` without classifying it here is a hard compile error.
#[allow(dead_code)]
pub(crate) fn verify_workbook_field_coverage(wb: &Workbook) {
    let Workbook {
        // ---- CoordFree: package/path/calc/encryption/protection/audit ----
        load_incomplete,
        has_vba,
        links,
        pivot_cache_base,
        pivot_session,
        pivot_calc_fields,
        styles,
        path,
        base_package,
        decrypted,
        date_system,
        calc_manual,
        calc_iterate,
        calc_max_iterations,
        calc_max_change,
        encryption,
        source_encryption,
        protection,
        audit,

        // ---- DerivedOrDirty: dirty flags, recalc state & save journals ----
        names_dirty,
        pivots_removed_parts,
        calc_pending,
        calc_pending_sheets,
        recalc_cell_revs,
        protection_dirty,
        dirty,
        chart_edits,

        // ---- CoordBound: coordinate-bearing workbook registries & sheets ----
        names,
        sources,
        pivot_shifts,
        pivots_pending,
        pivots_pending2,
        pivots,
        sheets,
    } = wb;

    // CoordFree
    assert_field_policy(load_incomplete, FieldCoordPolicy::CoordFree);
    assert_field_policy(has_vba, FieldCoordPolicy::CoordFree);
    assert_field_policy(links, FieldCoordPolicy::CoordFree);
    assert_field_policy(pivot_cache_base, FieldCoordPolicy::CoordFree);
    assert_field_policy(pivot_session, FieldCoordPolicy::CoordFree);
    assert_field_policy(pivot_calc_fields, FieldCoordPolicy::CoordFree);
    assert_field_policy(styles, FieldCoordPolicy::CoordFree);
    assert_field_policy(path, FieldCoordPolicy::CoordFree);
    assert_field_policy(base_package, FieldCoordPolicy::CoordFree);
    assert_field_policy(decrypted, FieldCoordPolicy::CoordFree);
    assert_field_policy(date_system, FieldCoordPolicy::CoordFree);
    assert_field_policy(calc_manual, FieldCoordPolicy::CoordFree);
    assert_field_policy(calc_iterate, FieldCoordPolicy::CoordFree);
    assert_field_policy(calc_max_iterations, FieldCoordPolicy::CoordFree);
    assert_field_policy(calc_max_change, FieldCoordPolicy::CoordFree);
    assert_field_policy(encryption, FieldCoordPolicy::CoordFree);
    assert_field_policy(source_encryption, FieldCoordPolicy::CoordFree);
    assert_field_policy(protection, FieldCoordPolicy::CoordFree);
    assert_field_policy(audit, FieldCoordPolicy::CoordFree);

    // DerivedOrDirty
    assert_field_policy(names_dirty, FieldCoordPolicy::DerivedOrDirty);
    assert_field_policy(pivots_removed_parts, FieldCoordPolicy::DerivedOrDirty);
    assert_field_policy(calc_pending, FieldCoordPolicy::DerivedOrDirty);
    assert_field_policy(calc_pending_sheets, FieldCoordPolicy::DerivedOrDirty);
    assert_field_policy(recalc_cell_revs, FieldCoordPolicy::DerivedOrDirty);
    assert_field_policy(protection_dirty, FieldCoordPolicy::DerivedOrDirty);
    assert_field_policy(dirty, FieldCoordPolicy::DerivedOrDirty);
    assert_field_policy(chart_edits, FieldCoordPolicy::DerivedOrDirty);

    // CoordBound
    assert_field_policy(names, FieldCoordPolicy::CoordBound);
    assert_field_policy(sources, FieldCoordPolicy::CoordBound);
    assert_field_policy(pivot_shifts, FieldCoordPolicy::CoordBound);
    assert_field_policy(pivots_pending, FieldCoordPolicy::CoordBound);
    assert_field_policy(pivots_pending2, FieldCoordPolicy::CoordBound);
    assert_field_policy(pivots, FieldCoordPolicy::CoordBound);
    assert_field_policy(sheets, FieldCoordPolicy::CoordBound);
}

/// Unified coordinate transformation geometry covering every workbook
/// coordinate-mutating operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CoordTransform {
    /// Whole-axis structural insert/delete (`band: None`) or partial-band cell
    /// shift (`band: Some((lo, hi))`).
    ///
    /// For `axis == Axis::Row`, `band` is the inclusive column range `(c1, c2)`.
    /// For `axis == Axis::Col`, `band` is the inclusive row range `(r1, r2)`.
    AxisShift {
        sheet_idx: usize,
        axis: Axis,
        at: u32,
        n: u32,
        delete: bool,
        band: Option<(u32, u32)>,
    },
    /// Contiguous block permutation moving `[from, from + n)` to final starting
    /// coordinate `at` along `axis`.
    AxisPermute {
        sheet_idx: usize,
        axis: Axis,
        from: u32,
        n: u32,
        at: u32,
    },
    /// Batch sorted non-decreasing single-row/col insertions along `axis`,
    /// where each entry is expressed in pre-insertion coordinates.
    MultiInsert {
        sheet_idx: usize,
        axis: Axis,
        positions: Vec<u32>,
    },
}

impl CoordTransform {
    /// Construct from a whole-axis `Structural` operation.
    pub fn from_structural(sheet_idx: usize, op: &Structural) -> Self {
        let (axis, at, n, delete) = match *op {
            Structural::InsertRows { at, n } => (Axis::Row, at, n, false),
            Structural::DeleteRows { at, n } => (Axis::Row, at, n, true),
            Structural::InsertCols { at, n } => (Axis::Col, at, n, false),
            Structural::DeleteCols { at, n } => (Axis::Col, at, n, true),
        };
        Self::AxisShift {
            sheet_idx,
            axis,
            at,
            n,
            delete,
            band: None,
        }
    }

    /// Construct from a partial-band `CellShift` operation.
    pub fn from_cell_shift(sheet_idx: usize, op: &CellShift) -> Self {
        let (r1, c1, r2, c2) = op.rect;
        let n = op.count();
        match op.direction {
            crate::cells::Shift::Vertical => Self::AxisShift {
                sheet_idx,
                axis: Axis::Row,
                at: r1,
                n,
                delete: !op.insert,
                band: Some((c1, c2)),
            },
            crate::cells::Shift::Horizontal => Self::AxisShift {
                sheet_idx,
                axis: Axis::Col,
                at: c1,
                n,
                delete: !op.insert,
                band: Some((r1, r2)),
            },
        }
    }

    /// Construct from a UI drop-line axis move (`move_rows_wb` / `move_cols_wb`).
    pub fn from_axis_move(
        sheet_idx: usize,
        axis: Axis,
        from: u32,
        n: u32,
        drop: u32,
    ) -> Option<Self> {
        let mv = AxisMove::from_drop(axis, from, n, drop)?;
        Some(Self::from_raw_axis_move(sheet_idx, mv))
    }

    /// Construct from a validated final-destination axis permutation `[from, from+n) -> [at, at+n)`.
    pub fn from_axis_permute(
        sheet_idx: usize,
        axis: Axis,
        from: u32,
        n: u32,
        at: u32,
    ) -> Option<Self> {
        let limit = axis_limit(axis);
        let from_end = from.checked_add(n)?;
        let at_end = at.checked_add(n)?;
        if n == 0 || from == at || from_end > limit || at_end > limit {
            return None;
        }
        Some(Self::AxisPermute {
            sheet_idx,
            axis,
            from,
            n,
            at,
        })
    }

    pub(crate) fn from_raw_axis_move(sheet_idx: usize, mv: AxisMove) -> Self {
        Self::AxisPermute {
            sheet_idx,
            axis: mv.axis,
            from: mv.from,
            n: mv.n,
            at: mv.at,
        }
    }

    /// Bridge for undo/reference scanning where `(Shift, Option<AxisMove>)` is stored.
    pub(crate) fn from_shift_and_move(
        sheet_idx: usize,
        shift: &Shift,
        movement: Option<AxisMove>,
    ) -> Self {
        if let Some(mv) = movement {
            return Self::from_raw_axis_move(sheet_idx, mv);
        }
        let band = match shift.axis {
            Axis::Row => shift.col_filter,
            Axis::Col => shift.row_filter,
        };
        Self::AxisShift {
            sheet_idx,
            axis: shift.axis,
            at: shift.at,
            n: shift.n,
            delete: shift.delete,
            band,
        }
    }

    /// Construct a batch insertion transform; `positions` are sorted ascending.
    pub fn from_multi_insert(sheet_idx: usize, axis: Axis, mut positions: Vec<u32>) -> Self {
        positions.sort_unstable();
        Self::MultiInsert {
            sheet_idx,
            axis,
            positions,
        }
    }

    #[inline]
    pub fn target_sheet(&self) -> usize {
        match *self {
            Self::AxisShift { sheet_idx, .. }
            | Self::AxisPermute { sheet_idx, .. }
            | Self::MultiInsert { sheet_idx, .. } => sheet_idx,
        }
    }

    #[inline]
    pub fn axis(&self) -> Axis {
        match *self {
            Self::AxisShift { axis, .. }
            | Self::AxisPermute { axis, .. }
            | Self::MultiInsert { axis, .. } => axis,
        }
    }

    #[inline]
    pub fn band(&self) -> Option<(u32, u32)> {
        match *self {
            Self::AxisShift { band, .. } => band,
            Self::AxisPermute { .. } | Self::MultiInsert { .. } => None,
        }
    }

    #[inline]
    pub fn is_noop(&self) -> bool {
        match self {
            Self::AxisShift { n, .. } => *n == 0,
            Self::AxisPermute { from, n, at, .. } => *n == 0 || *from == *at,
            Self::MultiInsert { positions, .. } => positions.is_empty(),
        }
    }

    #[inline]
    pub fn axis_limit(&self) -> u32 {
        axis_limit(self.axis())
    }

    /// Map a single 0-based coordinate along `self.axis()`, returning `None` if
    /// deleted or shifted past the grid limit (`MAX_ROWS` / `MAX_COLS`).
    pub fn map_index(&self, pos: u32) -> Option<u32> {
        let limit = self.axis_limit();
        if pos >= limit {
            return None;
        }
        match self {
            Self::AxisShift { at, n, delete, .. } => {
                if *n == 0 {
                    return Some(pos);
                }
                if *delete {
                    if pos < *at {
                        Some(pos)
                    } else if pos < at.saturating_add(*n) {
                        None
                    } else {
                        Some(pos - *n)
                    }
                } else if pos >= *at {
                    pos.checked_add(*n).filter(|&v| v < limit)
                } else {
                    Some(pos)
                }
            }
            Self::AxisPermute { from, n, at, .. } => {
                if *n == 0 || *from == *at {
                    return Some(pos);
                }
                let mapped = permute_index(pos, *from, *n, *at);
                (mapped < limit).then_some(mapped)
            }
            Self::MultiInsert { positions, .. } => {
                if positions.is_empty() {
                    return Some(pos);
                }
                let delta = positions.partition_point(|&at| at <= pos) as u32;
                pos.checked_add(delta).filter(|&v| v < limit)
            }
        }
    }

    /// Map an inclusive 1D coordinate span `[a, b]` along `self.axis()`.
    ///
    /// - `AxisShift`: shrinks on partial deletion, returns `None` if wholly deleted
    ///   or shifted past the grid limit.
    /// - `AxisPermute`: returns `Some((lo, hi))` iff the image of `[a, b]` remains
    ///   a single contiguous interval (`AxisMove::contiguous_range`).
    /// - `MultiInsert`: maps both endpoints and validates bounds `< limit`.
    pub fn map_range(&self, a: u32, b: u32) -> Option<(u32, u32)> {
        let limit = self.axis_limit();
        let (first, last) = if a <= b { (a, b) } else { (b, a) };
        if last >= limit {
            return None;
        }
        match self {
            Self::AxisShift {
                axis,
                at,
                n,
                delete,
                ..
            } => {
                if *n == 0 {
                    return Some((first, last));
                }
                let sh = Shift {
                    axis: *axis,
                    at: *at,
                    n: *n,
                    delete: *delete,
                    ..Default::default()
                };
                let (lo, hi) = sh.map_range(first, last)?;
                (hi < limit).then_some((lo, hi))
            }
            Self::AxisPermute {
                axis,
                from,
                n,
                at,
                ..
            } => {
                if *n == 0 || *from == *at {
                    return Some((first, last));
                }
                let mv = AxisMove {
                    axis: *axis,
                    from: *from,
                    n: *n,
                    at: *at,
                };
                let (lo, hi) = mv.contiguous_range(first, last)?;
                (hi < limit).then_some((lo, hi))
            }
            Self::MultiInsert { positions, .. } => {
                if positions.is_empty() {
                    return Some((first, last));
                }
                let lo = self.map_index(first)?;
                let hi = self.map_index(last)?;
                (hi < limit).then_some((lo, hi))
            }
        }
    }

    /// Map a 2D cell `(row, col)`, respecting `self.band()`.
    /// Cells outside `band` are returned unchanged.
    pub fn map_cell(&self, row: u32, col: u32) -> Option<(u32, u32)> {
        if row >= crate::sync::MAX_ROWS || col >= crate::sync::MAX_COLS {
            return None;
        }
        if let Some((lo, hi)) = self.band() {
            let cross = match self.axis() {
                Axis::Row => col,
                Axis::Col => row,
            };
            if cross < lo || cross > hi {
                return Some((row, col));
            }
        }
        match self.axis() {
            Axis::Row => self.map_index(row).map(|nr| (nr, col)),
            Axis::Col => self.map_index(col).map(|nc| (row, nc)),
        }
    }

    /// Map a 2D inclusive rectangle `(r1, c1, r2, c2)`, respecting `self.band()`.
    ///
    /// Returns `None` if `r1 > r2 || c1 > c2`, deleted, overflows the grid,
    /// partially crosses a moving band boundary, or is non-contiguously split
    /// by an `AxisPermute`.
    pub fn map_rect(&self, rect: (u32, u32, u32, u32)) -> Option<(u32, u32, u32, u32)> {
        let (r1, c1, r2, c2) = rect;
        if r1 > r2 || c1 > c2 || r2 >= crate::sync::MAX_ROWS || c2 >= crate::sync::MAX_COLS {
            return None;
        }
        let (a, b, cross_a, cross_b) = match self.axis() {
            Axis::Row => (r1, r2, c1, c2),
            Axis::Col => (c1, c2, r1, r2),
        };
        if let Some((band_lo, band_hi)) = self.band() {
            if band_hi < cross_a || band_lo > cross_b {
                return Some(rect);
            }
            let (lo, hi) = self.map_range(a, b)?;
            if (lo, hi) != (a, b) && (band_lo > cross_a || band_hi < cross_b) {
                return None;
            }
            return Some(match self.axis() {
                Axis::Row => (lo, c1, hi, c2),
                Axis::Col => (r1, lo, r2, hi),
            });
        }
        let (lo, hi) = self.map_range(a, b)?;
        Some(match self.axis() {
            Axis::Row => (lo, c1, hi, c2),
            Axis::Col => (r1, lo, r2, hi),
        })
    }

    /// Convenience predicate for reference tracking: returns `true` iff `rect`
    /// is moved, resized, deleted, or split by this transform.
    #[inline]
    pub fn rect_changes(&self, rect: (u32, u32, u32, u32)) -> bool {
        self.map_rect(rect) != Some(rect)
    }

    /// Conservative O(|formula|) pre-filter: returns `false` ONLY when it is
    /// guaranteed that `self.rewrite_formula(formula, ..)` will return `formula`
    /// verbatim. Preserves large-sheet CoW sharing on untouched pages.
    pub fn formula_may_be_affected(&self, formula: &str) -> bool {
        if self.is_noop() {
            return false;
        }
        let min_at = match self {
            Self::AxisShift { at, .. } => *at,
            Self::AxisPermute { from, at, .. } => (*from).min(*at),
            Self::MultiInsert { positions, .. } => match positions.first() {
                Some(&p) => p,
                None => return false,
            },
        };
        formula_may_be_affected_from(formula, self.axis(), min_at)
    }

    /// Rewrite A1/range/whole-row/whole-col references in `formula` targeting
    /// `target_sheet` from a formula living on `own_sheet`.
    pub fn rewrite_formula(&self, formula: &str, target_sheet: &str, own_sheet: &str) -> String {
        if self.is_noop() {
            return formula.to_string();
        }
        match self {
            Self::AxisShift {
                axis,
                at,
                n,
                delete,
                band,
                ..
            } => {
                let (row_filter, col_filter) = match *axis {
                    Axis::Row => (None, *band),
                    Axis::Col => (*band, None),
                };
                let sh = Shift {
                    axis: *axis,
                    at: *at,
                    n: *n,
                    delete: *delete,
                    target_sheet: target_sheet.to_string(),
                    own_sheet: own_sheet.to_string(),
                    translate: None,
                    row_filter,
                    col_filter,
                };
                crate::refs::rewrite(formula, &sh)
            }
            Self::AxisPermute {
                axis,
                from,
                n,
                at,
                ..
            } => AxisMove {
                axis: *axis,
                from: *from,
                n: *n,
                at: *at,
            }
            .rewrite(formula, target_sheet, own_sheet),
            Self::MultiInsert {
                axis, positions, ..
            } => {
                let Some(&first) = positions.first() else {
                    return formula.to_string();
                };
                let sh = Shift {
                    axis: *axis,
                    at: first,
                    n: positions.len() as u32,
                    delete: false,
                    target_sheet: target_sheet.to_string(),
                    own_sheet: own_sheet.to_string(),
                    ..Default::default()
                };
                let ms = MappedShift {
                    shift: &sh,
                    insertions: positions,
                };
                crate::refs::rewrite_mapped(formula, &ms)
            }
        }
    }

    /// Geometric inverse of this transform when representable as a single
    /// `CoordTransform`.
    ///
    /// - `AxisShift`: flips `delete`.
    /// - `AxisPermute`: swaps `from` and `at`.
    /// - `MultiInsert`: returns `Some` when empty or all positions are identical
    ///   (single contiguous delete), and `None` for non-contiguous multi-insert.
    pub fn inverse(&self) -> Option<Self> {
        match self {
            Self::AxisShift {
                sheet_idx,
                axis,
                at,
                n,
                delete,
                band,
            } => Some(Self::AxisShift {
                sheet_idx: *sheet_idx,
                axis: *axis,
                at: *at,
                n: *n,
                delete: !*delete,
                band: *band,
            }),
            Self::AxisPermute {
                sheet_idx,
                axis,
                from,
                n,
                at,
            } => Some(Self::AxisPermute {
                sheet_idx: *sheet_idx,
                axis: *axis,
                from: *at,
                n: *n,
                at: *from,
            }),
            Self::MultiInsert {
                sheet_idx,
                axis,
                positions,
            } => {
                let Some(&first) = positions.first() else {
                    return Some(self.clone());
                };
                if positions.iter().all(|&p| p == first) {
                    Some(Self::AxisShift {
                        sheet_idx: *sheet_idx,
                        axis: *axis,
                        at: first,
                        n: positions.len() as u32,
                        delete: true,
                        band: None,
                    })
                } else {
                    None
                }
            }
        }
    }

    /// Reconstruct the corresponding `CellShift` for a partial-band `AxisShift`.
    pub fn as_cell_shift(&self) -> Option<CellShift> {
        let Self::AxisShift {
            axis,
            at,
            n,
            delete,
            band: Some((lo, hi)),
            ..
        } = *self
        else {
            return None;
        };
        if n == 0 || lo > hi {
            return None;
        }
        let end = at.checked_add(n - 1)?;
        let (rect, direction) = match axis {
            Axis::Row => ((at, lo, end, hi), crate::cells::Shift::Vertical),
            Axis::Col => ((lo, at, hi, end), crate::cells::Shift::Horizontal),
        };
        Some(CellShift::new(rect, direction, !delete))
    }

    /// Validate whether this transform can safely execute on `wb`.
    pub fn preflight(&self, wb: &Workbook) -> Result<(), CellShiftConflict> {
        let sheet_idx = self.target_sheet();
        if wb.sheets.get(sheet_idx).is_none() {
            return Err(CellShiftConflict::InvalidTarget);
        }
        let limit = self.axis_limit();
        match self {
            Self::AxisShift {
                band: Some(_), ..
            } => {
                let Some(cs) = self.as_cell_shift() else {
                    return Err(CellShiftConflict::InvalidTarget);
                };
                cs.preflight(wb, sheet_idx)
            }
            Self::AxisShift {
                at,
                n,
                delete,
                band: None,
                ..
            } => {
                if *at >= limit {
                    return Err(CellShiftConflict::InvalidTarget);
                }
                if !*delete && *n > limit - *at {
                    return Err(CellShiftConflict::Overflow);
                }
                Ok(())
            }
            Self::AxisPermute { from, n, at, .. } => {
                if *n == 0 || *from == *at {
                    return Err(CellShiftConflict::InvalidTarget);
                }
                let Some(from_end) = from.checked_add(*n) else {
                    return Err(CellShiftConflict::Overflow);
                };
                let Some(at_end) = at.checked_add(*n) else {
                    return Err(CellShiftConflict::Overflow);
                };
                if from_end > limit || at_end > limit {
                    return Err(CellShiftConflict::Overflow);
                }
                Ok(())
            }
            Self::MultiInsert { positions, .. } => {
                if positions.len() >= limit as usize {
                    return Err(CellShiftConflict::Overflow);
                }
                if positions.iter().any(|&p| p >= limit) {
                    return Err(CellShiftConflict::InvalidTarget);
                }
                Ok(())
            }
        }
    }

    /// Build a `Shift` + `&[u32]` pair for `AxisShift` / `MultiInsert` callers
    /// that interface with `refs::rewrite_mapped`.
    fn make_shift(&self, target_sheet: &str, own_sheet: &str) -> (Shift, &[u32]) {
        match self {
            Self::AxisShift {
                axis,
                at,
                n,
                delete,
                band,
                ..
            } => {
                let (row_filter, col_filter) = match *axis {
                    Axis::Row => (None, *band),
                    Axis::Col => (*band, None),
                };
                (
                    Shift {
                        axis: *axis,
                        at: *at,
                        n: *n,
                        delete: *delete,
                        target_sheet: target_sheet.to_string(),
                        own_sheet: own_sheet.to_string(),
                        translate: None,
                        row_filter,
                        col_filter,
                    },
                    &[],
                )
            }
            Self::MultiInsert {
                axis, positions, ..
            } => (
                Shift {
                    axis: *axis,
                    at: positions.first().copied().unwrap_or(0),
                    n: positions.len() as u32,
                    delete: false,
                    target_sheet: target_sheet.to_string(),
                    own_sheet: own_sheet.to_string(),
                    ..Default::default()
                },
                positions.as_slice(),
            ),
            Self::AxisPermute {
                axis,
                from,
                n,
                at,
                ..
            } => {
                let lo = (*from).min(*at);
                let hi = (*from + *n).max(*at + *n);
                (
                    Shift {
                        axis: *axis,
                        at: lo,
                        n: hi - lo,
                        delete: false,
                        target_sheet: target_sheet.to_string(),
                        own_sheet: own_sheet.to_string(),
                        ..Default::default()
                    },
                    &[],
                )
            }
        }
    }

    /// Rewrite all formulas, CF rules (including `Scale`/`Bar`/`IconSet` CFVO
    /// `Formula` thresholds), and DV formulas on `sheet` referencing `target_sheet`.
    pub(crate) fn rewrite_sheet_formulas_cf_dv(
        &self,
        sheet: &mut Sheet,
        target_sheet: &str,
    ) {
        if self.is_noop() {
            return;
        }
        let own_name = sheet.name.clone();
        let foreign_sheet =
            (!own_name.eq_ignore_ascii_case(target_sheet)).then_some(target_sheet);
        let changed = match self {
            Self::AxisPermute {
                axis,
                from,
                n,
                at,
                ..
            } => {
                let (axis, from, n, at) = (*axis, *from, *n, *at);
                let min_at = from.min(at);
                sheet
                    .cells_mut()
                    .rewrite_formulas_parallel(foreign_sheet, |old| {
                        if !formula_may_be_affected_from(old, axis, min_at) {
                            return None;
                        }
                        let new =
                            crate::refs::permute_axis(old, axis, from, n, at, target_sheet, &own_name);
                        (new != old).then(|| new.into())
                    })
            }
            Self::AxisShift { .. } | Self::MultiInsert { .. } => {
                let (shift, insertions) = self.make_shift(target_sheet, &own_name);
                let ms = MappedShift {
                    shift: &shift,
                    insertions,
                };
                let min_at = if insertions.is_empty() {
                    shift.at
                } else {
                    insertions[0]
                };
                let axis = shift.axis;
                sheet
                    .cells_mut()
                    .rewrite_formulas_parallel(foreign_sheet, |old| {
                        if !formula_may_be_affected_from(old, axis, min_at) {
                            return None;
                        }
                        let new = crate::refs::rewrite_mapped(old, &ms);
                        (new != old).then(|| new.into())
                    })
            }
        };
        sheet.edited |= changed;

        for blk in sheet.cf.blocks.iter_mut() {
            for rule in blk.rules.iter_mut() {
                if rule
                    .kind
                    .rewrite_formulas(|f| self.rewrite_formula(f, target_sheet, &own_name))
                {
                    sheet.cf_dirty = true;
                }
            }
        }
        for dv in sheet.dv.iter_mut() {
            let f1 = self.rewrite_formula(&dv.f1, target_sheet, &own_name);
            let f2 = self.rewrite_formula(&dv.f2, target_sheet, &own_name);
            if dv.f1 != f1 || dv.f2 != f2 {
                dv.f1 = f1;
                dv.f2 = f2;
                sheet.dv_dirty = true;
            }
        }
    }

    /// Update sparklines on `sheet` whose explicit `source_sheet` matches `target_sheet`
    /// (case-insensitive). Same-sheet sparklines with `source_sheet == None` are handled
    /// on the target sheet itself so nothing is double-mapped.
    pub(crate) fn remap_sheet_qualified_sparklines(
        &self,
        sheet: &mut Sheet,
        target_sheet: &str,
    ) {
        if self.is_noop() || sheet.sparklines.is_empty() {
            return;
        }
        for sp in sheet.sparklines.iter_mut() {
            if !sp
                .source_sheet
                .as_deref()
                .is_some_and(|n| n.eq_ignore_ascii_case(target_sheet))
            {
                continue;
            }
            match self.map_rect(sp.rect) {
                Some(next) => {
                    if next != sp.rect {
                        sp.rect = next;
                        sheet.sparklines_dirty = true;
                    }
                }
                None => match *self {
                    Self::AxisShift {
                        axis,
                        at,
                        delete: true,
                        ..
                    } => {
                        let (r1, c1, r2, c2) = sp.rect;
                        let clamped = match axis {
                            Axis::Row => (at, c1, at, c2),
                            Axis::Col => (r1, at, r2, at),
                        };
                        if sp.rect != clamped {
                            sp.rect = clamped;
                            sheet.sparklines_dirty = true;
                        }
                    }
                    Self::AxisPermute { .. } => {
                        sp.source_sheet = Some(String::new());
                        sp.rect = (0, 0, 0, 0);
                        sheet.sparklines_dirty = true;
                    }
                    _ => {}
                },
            }
        }
    }

    /// Unified workbook-level reference & subsystem dispatcher.
    ///
    /// Invoked after target-sheet cell/layout mutation by ALL four workbook entry
    /// points: `apply_structural_wb`, `move_rows_wb`/`move_cols_wb`,
    /// `CellShift::apply`/`apply_cell_shift_wb`, and `insert_subtotal_rows`.
    pub(crate) fn apply_workbook_references(&self, wb: &mut Workbook) {
        if self.is_noop() {
            return;
        }
        let sheet_idx = self.target_sheet();
        let Some(target_name) = wb.sheets.get(sheet_idx).map(|s| s.name.clone()) else {
            return;
        };
        let target_sid = wb.sheets[sheet_idx].identity;
        let is_band = self.band().is_some();

        // 1. External/internal data source bindings (`wb.sources`).
        crate::external::remap_sources(wb, sheet_idx, self.axis(), self.band(), |lo, hi| {
            self.map_range(lo, hi)
        });

        // 2. For band shifts (`CellShift`), rewrite target-sheet internal hyperlink `location`
        //    (whole-axis / permute / multi-insert rewrite target hyperlinks during sheet mutation).
        if is_band {
            let target = &mut wb.sheets[sheet_idx];
            rewrite_target_hyperlink_locations(target, self);
        }

        // 3. Per-sheet reference-bearing subsystems across ALL sheets (`wb.sheets`).
        for sheet in &mut wb.sheets {
            let own_name = sheet.name.clone();
            self.rewrite_sheet_formulas_cf_dv(sheet, &target_name);
            if !is_band {
                for ch in sheet.charts.iter_mut() {
                    if ch.rewrite_with_transform(self, &target_name, &own_name) {
                        sheet.charts_dirty = true;
                    }
                }
                self.remap_sheet_qualified_sparklines(sheet, &target_name);
            }
        }

        // 4. Workbook defined names (`wb.names`).
        for (_, scope, text) in &mut wb.names {
            let own_name = scope
                .and_then(|i| wb.sheets.get(i))
                .map(|s| s.name.as_str())
                .unwrap_or("");
            let rewritten = self.rewrite_formula(text, &target_name, own_name);
            if rewritten != *text {
                *text = rewritten;
                wb.names_dirty = true;
            }
        }

        // 5. Workbook pivot cache shift journal & pivot models (whole-axis / permute / multi-insert).
        //    For band shifts, `CellShift::preflight` already blocks overlapping pivots.
        if !is_band {
            self.record_pivot_shifts(wb, &target_name);
            self.remap_pivots(wb, sheet_idx, target_sid, &target_name);
        }
    }

    fn record_pivot_shifts(&self, wb: &mut Workbook, target_name: &str) {
        match self {
            Self::AxisShift {
                axis,
                at,
                n,
                delete,
                band: None,
                ..
            } => {
                wb.pivot_shifts.push((
                    target_name.to_string(),
                    u8::from(*axis == Axis::Col),
                    *at,
                    *n,
                    *delete,
                ));
            }
            Self::MultiInsert {
                axis, positions, ..
            } => {
                let axis_code = u8::from(*axis == Axis::Col);
                wb.pivot_shifts.extend(
                    positions
                        .iter()
                        .rev()
                        .map(|&p| (target_name.to_string(), axis_code, p, 1, false)),
                );
            }
            Self::AxisShift { band: Some(_), .. } | Self::AxisPermute { .. } => {}
        }
    }

    fn remap_pivots(
        &self,
        wb: &mut Workbook,
        sheet_idx: usize,
        target_sid: u64,
        target_name: &str,
    ) {
        let axis = self.axis();
        let is_permute = matches!(self, Self::AxisPermute { .. });

        wb.pivots_pending.retain_mut(|(idx, spec, _, rect, _)| {
            if *idx != sheet_idx {
                return true;
            }
            let mapped = (|| {
                let source = self.map_rect(spec.source)?;
                let output = self.map_rect(*rect)?;
                let (row_field, col_field, value_field) = if axis == Axis::Col {
                    let col_field = match spec.col_field {
                        Some(c) => Some(self.map_index(c)?),
                        None => None,
                    };
                    (
                        self.map_index(spec.row_field)?,
                        col_field,
                        self.map_index(spec.value_field)?,
                    )
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

        wb.pivots_pending2.retain_mut(|(idx, spec, _, rect)| {
            if *idx != sheet_idx {
                return true;
            }
            let Some(source) = self.map_rect(spec.source) else {
                return false;
            };
            let Some(output) = self.map_rect(*rect) else {
                return false;
            };
            spec.source = source;
            *rect = output;
            if axis == Axis::Col {
                spec.rows.retain_mut(|f| match self.map_index(f.col) {
                    Some(nc) => {
                        f.col = nc;
                        true
                    }
                    None => false,
                });
                spec.cols.retain_mut(|f| match self.map_index(f.col) {
                    Some(nc) => {
                        f.col = nc;
                        true
                    }
                    None => false,
                });
                spec.values.retain_mut(|f| match self.map_index(f.col) {
                    Some(nc) => {
                        f.col = nc;
                        if let Some((bc, item)) = f.base.take() {
                            f.base = self.map_index(bc).map(|nbc| (nbc, item));
                        }
                        true
                    }
                    None => false,
                });
                spec.filters.retain_mut(|f| match self.map_index(f.col) {
                    Some(nc) => {
                        f.col = nc;
                        true
                    }
                    None => false,
                });
                if spec.rows.is_empty() && spec.cols.is_empty() && spec.values.is_empty() {
                    return false;
                }
            }
            true
        });

        wb.pivots.retain_mut(|p| {
            let is_src = p.source_sheet_id == Some(target_sid)
                || p.source_sheet.eq_ignore_ascii_case(target_name);
            let mut changed = false;
            if is_src {
                let Some(new_src) = self.map_rect(p.spec.source) else {
                    if let Some(part) = p.imported_part.take() {
                        wb.pivots_removed_parts.push(part);
                    }
                    return false;
                };
                if p.spec.source != new_src {
                    p.spec.source = new_src;
                    changed = true;
                }
                if axis == Axis::Col {
                    let mut keep_rows = Vec::new();
                    let mut keep_rext = Vec::new();
                    for (i, mut f) in std::mem::take(&mut p.spec.rows).into_iter().enumerate() {
                        if let Some(nc) = self.map_index(f.col) {
                            changed |= nc != f.col;
                            f.col = nc;
                            keep_rows.push(f);
                            if let Some(ext) = p.options.row_ext.get(i).cloned() {
                                keep_rext.push(ext);
                            }
                        } else {
                            changed = true;
                        }
                    }
                    p.spec.rows = keep_rows;
                    p.options.row_ext = keep_rext;

                    let mut keep_cols = Vec::new();
                    let mut keep_cext = Vec::new();
                    for (i, mut f) in std::mem::take(&mut p.spec.cols).into_iter().enumerate() {
                        if let Some(nc) = self.map_index(f.col) {
                            changed |= nc != f.col;
                            f.col = nc;
                            keep_cols.push(f);
                            if let Some(ext) = p.options.col_ext.get(i).cloned() {
                                keep_cext.push(ext);
                            }
                        } else {
                            changed = true;
                        }
                    }
                    p.spec.cols = keep_cols;
                    p.options.col_ext = keep_cext;

                    let mut keep_vals = Vec::new();
                    let mut keep_vext = Vec::new();
                    for (i, mut f) in std::mem::take(&mut p.spec.values).into_iter().enumerate() {
                        if let Some(nc) = self.map_index(f.col) {
                            changed |= nc != f.col;
                            f.col = nc;
                            if let Some((bc, item)) = f.base.take() {
                                let mapped_base = self.map_index(bc).map(|nbc| (nbc, item));
                                changed |= mapped_base.as_ref().map(|(c, _)| *c) != Some(bc);
                                f.base = mapped_base;
                            }
                            keep_vals.push(f);
                            if let Some(ext) = p.options.val_ext.get(i).cloned() {
                                keep_vext.push(ext);
                            }
                        } else {
                            changed = true;
                        }
                    }
                    p.spec.values = keep_vals;
                    p.options.val_ext = keep_vext;

                    p.spec.filters.retain_mut(|f| match self.map_index(f.col) {
                        Some(nc) => {
                            changed |= nc != f.col;
                            f.col = nc;
                            true
                        }
                        None => {
                            changed = true;
                            false
                        }
                    });
                    p.options.slicers.retain_mut(|s| match self.map_index(s.col) {
                        Some(nc) => {
                            changed |= nc != s.col;
                            s.col = nc;
                            true
                        }
                        None => {
                            changed = true;
                            false
                        }
                    });
                }
                if p.imported_part.is_none() || (is_permute && changed) {
                    p.modified = true;
                }
            }
            if p.target_sheet == sheet_idx {
                let Some(new_out) = self.map_rect(p.rect) else {
                    if let Some(part) = p.imported_part.take() {
                        wb.pivots_removed_parts.push(part);
                    }
                    return false;
                };
                let out_changed = p.rect != new_out || p.anchor != (new_out.0, new_out.1);
                p.anchor = (new_out.0, new_out.1);
                p.rect = new_out;
                p.last_meta.anchor = p.anchor;
                p.last_meta.rect = new_out;
                if p.imported_part.is_none() || (is_permute && out_changed) {
                    p.modified = true;
                }
            }
            true
        });
    }
}

#[inline]
fn axis_limit(axis: Axis) -> u32 {
    match axis {
        Axis::Row => crate::sync::MAX_ROWS,
        Axis::Col => crate::sync::MAX_COLS,
    }
}

/// Partition an inclusive 1D range `[a, b]` by the breakpoints of permutation
/// `[from, from+n) -> [at, at+n)`, map every affine piece, and coalesce adjacent
/// output intervals. Preserves the EXACT set of cells for multi-range metadata
/// (`protected_ranges`, `cf`, `dv`, `hidden_rows`, `hidden_cols`, `scenario_settings`).
pub(crate) fn permute_axis_range_split(
    a: u32,
    b: u32,
    from: u32,
    n: u32,
    at: u32,
) -> Vec<(u32, u32)> {
    let (lo_in, hi_in) = if a <= b { (a, b) } else { (b, a) };
    if n == 0 || from == at {
        return vec![(lo_in, hi_in)];
    }
    let band_lo = from.min(at);
    let band_hi = from.saturating_add(n).max(at.saturating_add(n));
    if hi_in < band_lo || lo_in >= band_hi {
        return vec![(lo_in, hi_in)];
    }
    if lo_in <= band_lo && hi_in.saturating_add(1) >= band_hi {
        return vec![(lo_in, hi_in)];
    }
    let mut cuts = [band_lo, from, from.saturating_add(n), band_hi];
    cuts.sort_unstable();
    let mut cur = lo_in;
    let mut pieces = Vec::with_capacity(4);
    for cut in cuts {
        if cut > cur && cut <= hi_in {
            let s = permute_index(cur, from, n, at);
            let e = permute_index(cut - 1, from, n, at);
            pieces.push((s.min(e), s.max(e)));
            cur = cut;
        }
    }
    if cur <= hi_in {
        let s = permute_index(cur, from, n, at);
        let e = permute_index(hi_in, from, n, at);
        pieces.push((s.min(e), s.max(e)));
    }
    pieces.sort_unstable_by_key(|&(s, _)| s);
    let mut merged: Vec<(u32, u32)> = Vec::with_capacity(pieces.len());
    for (s, e) in pieces {
        if let Some(last) = merged.last_mut()
            && s <= last.1.saturating_add(1)
        {
            last.1 = last.1.max(e);
        } else {
            merged.push((s, e));
        }
    }
    merged
}

/// Conservative O(|formula|) pre-filter: returns `false` ONLY when no token
/// along `axis` can possibly be rewritten when coordinates `< min_at` stay fixed.
pub(crate) fn formula_may_be_affected_from(formula: &str, axis: Axis, min_at: u32) -> bool {
    let b = formula.as_bytes();
    match axis {
        Axis::Row => {
            let threshold = min_at as u64 + 1;
            let mut i = 0usize;
            let mut saw_colon = false;
            let mut prev_num: Option<u64> = None;
            while i < b.len() {
                let ch = b[i];
                if ch.is_ascii_lowercase() {
                    return true;
                }
                if ch == b':' {
                    saw_colon = true;
                    i += 1;
                    continue;
                }
                if ch.is_ascii_digit() {
                    let start = i;
                    let mut val = 0u64;
                    while i < b.len() && b[i].is_ascii_digit() {
                        val = val.saturating_mul(10).saturating_add((b[i] - b'0') as u64);
                        i += 1;
                    }
                    if i - start > 1 && b[start] == b'0' {
                        return true;
                    }
                    if val >= threshold {
                        return true;
                    }
                    if saw_colon {
                        if prev_num.is_some_and(|p| p > val) {
                            return true;
                        }
                        saw_colon = false;
                    }
                    prev_num = Some(val);
                    continue;
                }
                i += 1;
            }
            false
        }
        Axis::Col => {
            let mut i = 0usize;
            let mut saw_colon = false;
            let mut prev_col: Option<u32> = None;
            while i < b.len() {
                let ch = b[i];
                if ch.is_ascii_lowercase() {
                    return true;
                }
                if ch == b':' {
                    saw_colon = true;
                    i += 1;
                    continue;
                }
                if ch.is_ascii_digit() {
                    let start = i;
                    while i < b.len() && b[i].is_ascii_digit() {
                        i += 1;
                    }
                    if i - start > 1 && b[start] == b'0' {
                        return true;
                    }
                    continue;
                }
                if ch.is_ascii_uppercase() {
                    let start = i;
                    let mut col = 0u32;
                    while i < b.len() && b[i].is_ascii_uppercase() {
                        col = col
                            .saturating_mul(26)
                            .saturating_add((b[i] - b'A') as u32 + 1);
                        i += 1;
                    }
                    let len = i - start;
                    if len <= 3 && col > 0 && col <= 16_384 && b.get(i) != Some(&b'(') {
                        let c0 = col - 1;
                        if c0 >= min_at {
                            return true;
                        }
                        if saw_colon {
                            if prev_col.is_some_and(|p| p > c0) {
                                return true;
                            }
                            saw_colon = false;
                        }
                        prev_col = Some(c0);
                    }
                    continue;
                }
                i += 1;
            }
            false
        }
    }
}

enum InternalLinkTarget {
    Cell(u32, u32, bool, bool),
    Range(u32, u32, bool, bool, u32, u32, bool, bool),
}

fn parse_internal_link_location<'a>(
    loc: &'a str,
    own_sheet: &str,
) -> Option<(&'static str, Option<&'a str>, InternalLinkTarget)> {
    let trimmed = loc.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (hash_prefix, rest) = if let Some(r) = trimmed.strip_prefix('#') {
        ("#", r)
    } else {
        ("", trimmed)
    };
    let (sheet_prefix, range_part) = if let Some((sh, rg)) = rest.rsplit_once('!') {
        let unquoted = sh.trim().trim_matches('\'').replace("''", "'");
        if !unquoted.eq_ignore_ascii_case(own_sheet) {
            return None;
        }
        (Some(sh), rg.trim())
    } else {
        (None, rest.trim())
    };
    let parse_token = |tok: &str| -> Option<(u32, u32, bool, bool)> {
        let mut s = tok.trim();
        let col_abs = s.starts_with('$');
        if col_abs {
            s = &s[1..];
        }
        let col_end = s.bytes().take_while(|b| b.is_ascii_alphabetic()).count();
        if !(1..=3).contains(&col_end) {
            return None;
        }
        let col_str = &s[..col_end];
        let mut r_part = &s[col_end..];
        let row_abs = r_part.starts_with('$');
        if row_abs {
            r_part = &r_part[1..];
        }
        if r_part.is_empty() || !r_part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let (r, c) = crate::sparse::parse_ref(&format!("{col_str}{r_part}"))?;
        Some((r, c, col_abs, row_abs))
    };
    let target = if let Some((a, b)) = range_part.split_once(':') {
        let (r1, c1, ca1, ra1) = parse_token(a)?;
        let (r2, c2, ca2, ra2) = parse_token(b)?;
        InternalLinkTarget::Range(r1, c1, ca1, ra1, r2, c2, ca2, ra2)
    } else {
        let (r, c, col_abs, row_abs) = parse_token(range_part)?;
        InternalLinkTarget::Cell(r, c, col_abs, row_abs)
    };
    Some((hash_prefix, sheet_prefix, target))
}

/// Returns `true` if `loc` is an internal hyperlink location targeting `own_sheet`
/// whose referenced cell or range is shifted, shrunk, deleted, overflowed, or
/// crosses a moving band boundary under `transform`.
pub(crate) fn internal_link_location_affected(
    loc: &str,
    own_sheet: &str,
    transform: &CoordTransform,
) -> bool {
    if transform.is_noop() {
        return false;
    }
    let Some((_, _, target)) = parse_internal_link_location(loc, own_sheet) else {
        return false;
    };
    match target {
        InternalLinkTarget::Cell(r, c, _, _) => transform.map_cell(r, c) != Some((r, c)),
        InternalLinkTarget::Range(r1, c1, _, _, r2, c2, _, _) => {
            let rect = (r1.min(r2), c1.min(c2), r1.max(r2), c1.max(c2));
            transform.map_rect(rect) != Some(rect)
        }
    }
}

/// Rewrite an internal hyperlink `location` targeting `own_sheet` (such as
/// `Sheet1!B5`, `#Sheet1!$A$1:$C$3`, or unqualified `B5`) under `transform`.
pub(crate) fn rewrite_internal_link_location(
    loc: &str,
    own_sheet: &str,
    transform: &CoordTransform,
) -> Option<String> {
    if transform.is_noop() {
        return None;
    }
    let (hash_prefix, sheet_prefix, target) = parse_internal_link_location(loc, own_sheet)?;
    let fmt_token = |r: u32, c: u32, col_abs: bool, row_abs: bool| -> String {
        let mut out = String::new();
        if col_abs {
            out.push('$');
        }
        out.push_str(&crate::model::col_label(c));
        if row_abs {
            out.push('$');
        }
        out.push_str(&(r + 1).to_string());
        out
    };
    let mapped = match target {
        InternalLinkTarget::Range(r1, c1, ca1, ra1, r2, c2, ca2, ra2) => {
            let (nr1, nc1, nr2, nc2) =
                transform.map_rect((r1.min(r2), c1.min(c2), r1.max(r2), c1.max(c2)))?;
            format!(
                "{}:{}",
                fmt_token(nr1, nc1, ca1, ra1),
                fmt_token(nr2, nc2, ca2, ra2)
            )
        }
        InternalLinkTarget::Cell(r, c, col_abs, row_abs) => {
            let (nr, nc) = transform.map_cell(r, c)?;
            fmt_token(nr, nc, col_abs, row_abs)
        }
    };
    Some(match sheet_prefix {
        Some(sh) => format!("{hash_prefix}{sh}!{mapped}"),
        None => format!("{hash_prefix}{mapped}"),
    })
}

/// Rewrite all internal hyperlink `location` strings on `sheet` in place.
pub(crate) fn rewrite_target_hyperlink_locations(sheet: &mut Sheet, transform: &CoordTransform) {
    if sheet.hyperlinks.is_empty() || transform.is_noop() {
        return;
    }
    let own_name = sheet.name.clone();
    let updates: Vec<((u32, u32), String)> = sheet
        .hyperlinks
        .iter()
        .filter_map(|(&pos, link)| {
            if link.location.is_empty() {
                return None;
            }
            let next = rewrite_internal_link_location(&link.location, &own_name, transform)?;
            (next != link.location).then_some((pos, next))
        })
        .collect();
    if !updates.is_empty() {
        for (pos, next) in updates {
            if let Some(link) = sheet.hyperlinks.get_mut(&pos) {
                link.location = next;
            }
        }
        sheet.hyperlinks_dirty = true;
    }
}

/// Check whether a chart series reference or `source_range` references cells
/// intersecting `ray` on `target_name` (or would be rewritten by `transform`).
pub(crate) fn chart_ref_intersects_ray(
    ref_str: &str,
    own_is_target: bool,
    own_name: &str,
    target_name: &str,
    ray: (u32, u32, u32, u32),
    transform: &CoordTransform,
) -> bool {
    let trimmed = ref_str.trim();
    if trimmed.is_empty() {
        return false;
    }
    let inner = trimmed
        .strip_prefix('(')
        .and_then(|s| s.strip_suffix(')'))
        .unwrap_or(trimmed);
    // Split multi-area chart refs like `(Sheet1!$A$1:$A$3,Sheet1!$C$1:$C$3)` outside quotes.
    let mut start = 0usize;
    let mut in_quote = false;
    let bytes = inner.as_bytes();
    let mut i = 0usize;
    while i <= bytes.len() {
        let at_sep = if i == bytes.len() {
            true
        } else {
            if bytes[i] == b'\'' {
                in_quote = !in_quote;
            }
            !in_quote && bytes[i] == b','
        };
        if at_sep {
            let part = inner[start..i].trim();
            if let Some((sheet_opt, rect)) = crate::charts::parse_ref_rect(part) {
                let hits_sheet = match sheet_opt {
                    Some(name) => name.eq_ignore_ascii_case(target_name),
                    None => own_is_target,
                };
                if hits_sheet
                    && rect.0 <= ray.2
                    && ray.0 <= rect.2
                    && rect.1 <= ray.3
                    && ray.1 <= rect.3
                {
                    return true;
                }
            }
            start = i + 1;
        }
        i += 1;
    }
    transform.formula_may_be_affected(trimmed)
        && transform.rewrite_formula(trimmed, target_name, own_name) != trimmed
}
