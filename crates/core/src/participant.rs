//! Unified `CoordParticipant` subsystem table & contract.
//!
//! Every `FieldCoordPolicy::CoordBound` field on `Sheet` and `Workbook` belongs
//! to one of the 16 [`CoordParticipant`] subsystem groups declared here.
//! Preflight checks, workbook-level reference rewrites, `StructureSnapshot`
//! metadata-presence / change detection, `CellShiftPatch` dry-run cloning /
//! diffing / matching / replay, and `CellShiftOverlay` capture / apply all
//! derive from this single authoritative specification.

use crate::model::{CellRef, Sheet, Workbook};
use crate::structural::{CellShift, CellShiftConflict};
use crate::transform::CoordTransform;

/// How a coordinate-bound subsystem behaves under a partial-band `CellShift`
/// (`CoordTransform::AxisShift { band: Some(..) }`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CellShiftMode {
    /// Transforms cleanly under partial-band `CellShift` and participates in
    /// `CellShiftPatch` / `CellShiftOverlay` / `StructureSnapshot` parity.
    Transform,
    /// Safely rejected by `CellShift` preflight whenever the object intersects
    /// the moving ray (whole-axis `Structural` / `AxisPermute` transforms it).
    RejectOnRayOverlap,
    /// Whole-axis layout/drawing state untouched by partial-band `CellShift`
    /// (transformed only by whole-axis `Structural` / `AxisPermute` / `MultiInsert`).
    AxisOnly,
}

/// Authoritative enumeration of every coordinate-bound subsystem group across
/// `Sheet` and `Workbook`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CoordParticipant {
    CellsAndStyles,
    CellNotesAndThreads,
    RichAndHyperlinks,
    Merges,
    ConditionalAndValidation,
    Charts,
    Sparklines,
    DefinedNames,
    ExternalSources,
    IgnoredErrors,
    Tables,
    Pivots,
    ArraysAndSpills,
    AutoFilter,
    ScenariosAndProtectedRanges,
    LayoutAndPage,
}

/// Context passed once per `CoordTransform::apply_workbook_references` dispatch.
pub(crate) struct WorkbookRefCtx {
    pub(crate) sheet_idx: usize,
    pub(crate) target_name: String,
    pub(crate) target_sid: u64,
    pub(crate) is_band: bool,
}

/// View over a `StructureSnapshot`'s before/after state for participant-driven
/// `cell_shift_changed_metadata` checks.
pub(crate) struct CellShiftMetaDiffView<'a> {
    pub(crate) target_before: &'a Sheet,
    pub(crate) target_after: &'a Sheet,
    pub(crate) names_before: &'a [(String, Option<usize>, String)],
    pub(crate) sources_before: &'a [crate::external::Source],
    pub(crate) wb_after: &'a Workbook,
    pub(crate) foreign_cf_changed: bool,
    pub(crate) foreign_dv_changed: bool,
    pub(crate) foreign_charts_changed: bool,
    pub(crate) foreign_sparklines_changed: bool,
}

#[inline]
pub(crate) fn norm_rect((r1, c1, r2, c2): (u32, u32, u32, u32)) -> (u32, u32, u32, u32) {
    (r1.min(r2), c1.min(c2), r1.max(r2), c1.max(c2))
}

#[inline]
pub(crate) fn rects_intersect(a: (u32, u32, u32, u32), b: (u32, u32, u32, u32)) -> bool {
    a.0 <= b.2 && b.0 <= a.2 && a.1 <= b.3 && b.1 <= a.3
}

#[inline]
pub(crate) fn point_in_rect((r, c): (u32, u32), rect: (u32, u32, u32, u32)) -> bool {
    r >= rect.0 && r <= rect.2 && c >= rect.1 && c <= rect.3
}

/// Compare two chart slices purely by identity/reference geometry (`part`,
/// optional `anchor`, `source_range`, and series refs), ignoring presentation
/// and cached render values.
pub(crate) fn chart_refs_match(
    current: &[crate::charts::Chart],
    expected: &[crate::charts::Chart],
    check_anchor: bool,
) -> bool {
    current.len() == expected.len()
        && current.iter().zip(expected).all(|(a, b)| {
            a.part == b.part
                && (!check_anchor || a.anchor == b.anchor)
                && a.source_range == b.source_range
                && a.series.len() == b.series.len()
                && a.series.iter().zip(&b.series).all(|(s, t)| {
                    s.name_ref == t.name_ref
                        && s.cat_ref == t.cat_ref
                        && s.val_ref == t.val_ref
                        && s.size_ref == t.size_ref
                })
        })
}

impl CoordParticipant {
    /// Canonical list of all 16 coordinate-bound subsystem groups.
    pub const ALL: [Self; 16] = [
        Self::CellsAndStyles,
        Self::CellNotesAndThreads,
        Self::RichAndHyperlinks,
        Self::Merges,
        Self::ConditionalAndValidation,
        Self::Charts,
        Self::Sparklines,
        Self::DefinedNames,
        Self::ExternalSources,
        Self::IgnoredErrors,
        Self::Tables,
        Self::Pivots,
        Self::ArraysAndSpills,
        Self::AutoFilter,
        Self::ScenariosAndProtectedRanges,
        Self::LayoutAndPage,
    ];

    /// Deterministic conflict-precedence order for `CellShift` object preflight.
    const CELL_SHIFT_PREFLIGHT_ORDER: [Self; 16] = [
        Self::CellsAndStyles,
        Self::Merges,
        Self::Tables,
        Self::Pivots,
        Self::ArraysAndSpills,
        Self::AutoFilter,
        Self::Charts,
        Self::Sparklines,
        Self::ScenariosAndProtectedRanges,
        Self::RichAndHyperlinks,
        Self::CellNotesAndThreads,
        Self::ConditionalAndValidation,
        Self::DefinedNames,
        Self::ExternalSources,
        Self::IgnoredErrors,
        Self::LayoutAndPage,
    ];

    /// How this subsystem behaves under partial-band `CellShift`.
    pub const fn cell_shift_mode(self) -> CellShiftMode {
        match self {
            Self::CellsAndStyles
            | Self::CellNotesAndThreads
            | Self::RichAndHyperlinks
            | Self::Merges
            | Self::ConditionalAndValidation
            | Self::Charts
            | Self::Sparklines
            | Self::DefinedNames
            | Self::ExternalSources
            | Self::IgnoredErrors => CellShiftMode::Transform,
            Self::Tables
            | Self::Pivots
            | Self::ArraysAndSpills
            | Self::AutoFilter
            | Self::ScenariosAndProtectedRanges => CellShiftMode::RejectOnRayOverlap,
            Self::LayoutAndPage => CellShiftMode::AxisOnly,
        }
    }

    /// Whether this subsystem requires `StructureSnapshot` promotion (beyond
    /// sparse `CellDelta`s) when mutated by a local `CellShift`.
    pub const fn tracks_cell_shift_metadata(self) -> bool {
        matches!(self.cell_shift_mode(), CellShiftMode::Transform)
            && !matches!(self, Self::CellsAndStyles)
    }

    /// Whether this subsystem participates in `CellShiftPatch` / `CellShiftOverlay`.
    pub const fn participates_in_cell_shift_patch(self) -> bool {
        matches!(self.cell_shift_mode(), CellShiftMode::Transform)
    }

    /// Whether this subsystem runs a workbook-wide reference pass in
    /// `CoordTransform::apply_workbook_references`.
    pub const fn participates_in_workbook_refs(self) -> bool {
        matches!(
            self,
            Self::CellsAndStyles
                | Self::RichAndHyperlinks
                | Self::ConditionalAndValidation
                | Self::Charts
                | Self::Sparklines
                | Self::DefinedNames
                | Self::ExternalSources
                | Self::Pivots
        )
    }

    /// Per-participant preflight validation for any [`CoordTransform`].
    pub fn preflight(self, wb: &Workbook, tx: &CoordTransform) -> Result<(), CellShiftConflict> {
        let sheet_idx = tx.target_sheet();
        let Some(sheet) = wb.sheets.get(sheet_idx) else {
            return Err(CellShiftConflict::InvalidTarget);
        };
        let Some(op) = tx.as_cell_shift() else {
            return Ok(());
        };
        let Some(ray) = op.moving_ray() else {
            return Err(CellShiftConflict::InvalidTarget);
        };
        let target_name = sheet.name.as_str();
        let target_sid = sheet.identity;

        match self {
            Self::CellsAndStyles => {
                let n = op.count();
                if op.insert && crate::cells::insert_would_overflow(sheet, op.rect, op.direction, n)
                {
                    return Err(CellShiftConflict::Overflow);
                }
                Ok(())
            }
            Self::Merges => preflight_merges_for_shift(sheet, &op),
            Self::Tables => {
                if sheet
                    .tables
                    .iter()
                    .any(|t| rects_intersect(norm_rect(t.range), ray))
                {
                    Err(CellShiftConflict::Table)
                } else {
                    Ok(())
                }
            }
            Self::Pivots => {
                let pivot_hit = wb.pivots.iter().any(|p| {
                    let is_src = p.source_sheet_id == Some(target_sid)
                        || p.source_sheet.eq_ignore_ascii_case(target_name);
                    (is_src && rects_intersect(norm_rect(p.spec.source), ray))
                        || (p.target_sheet == sheet_idx
                            && (rects_intersect(norm_rect(p.rect), ray)
                                || point_in_rect(p.anchor, ray)))
                }) || wb.pivots_pending.iter().any(|(idx, spec, _, out, _)| {
                    *idx == sheet_idx
                        && (rects_intersect(norm_rect(spec.source), ray)
                            || rects_intersect(norm_rect(*out), ray))
                }) || wb.pivots_pending2.iter().any(|(idx, spec, _, out)| {
                    *idx == sheet_idx
                        && (rects_intersect(norm_rect(spec.source), ray)
                            || rects_intersect(norm_rect(*out), ray))
                });
                if pivot_hit {
                    Err(CellShiftConflict::Pivot)
                } else {
                    Ok(())
                }
            }
            Self::ArraysAndSpills => {
                let array_or_spill_hit = sheet.arrays.iter().any(|&(anchor, rect)| {
                    point_in_rect((anchor.row, anchor.col), ray)
                        || rects_intersect(norm_rect(rect), ray)
                }) || sheet.spill_shapes.iter().any(|(&anchor, &(h, w))| {
                    let spill_rect = (
                        anchor.row,
                        anchor.col,
                        anchor.row.saturating_add(h.saturating_sub(1)),
                        anchor.col.saturating_add(w.saturating_sub(1)),
                    );
                    rects_intersect(spill_rect, ray)
                }) || sheet.spilled.iter().any(|k| point_in_rect((k.row, k.col), ray))
                    || (!sheet.spilled.is_empty()
                        && sheet.spill_shapes.is_empty()
                        && sheet.formula_cells().any(|(k, _)| {
                            point_in_rect((k.row, k.col), ray)
                                && (k
                                    .row
                                    .checked_add(1)
                                    .is_some_and(|r| sheet.spilled.contains(&CellRef::new(r, k.col)))
                                    || k.col.checked_add(1).is_some_and(|c| {
                                        sheet.spilled.contains(&CellRef::new(k.row, c))
                                    }))
                        }));
                if array_or_spill_hit {
                    Err(CellShiftConflict::ArrayOrSpill)
                } else {
                    Ok(())
                }
            }
            Self::AutoFilter => {
                if sheet
                    .filter
                    .as_ref()
                    .is_some_and(|f| rects_intersect(norm_rect(f.range), ray))
                {
                    Err(CellShiftConflict::AutoFilter)
                } else {
                    Ok(())
                }
            }
            // Charts: partial-band `CellShift` safely rewrites series references &
            // `source_range` across all sheets via `Chart::rewrite_with_transform`
            // without moving floating anchors.
            Self::Charts => Ok(()),
            Self::Sparklines => preflight_sparklines_for_shift(wb, sheet_idx, &op, tx, ray),
            Self::ScenariosAndProtectedRanges => {
                if sheet
                    .protected_ranges
                    .iter()
                    .any(|pr| pr.sqref.iter().any(|&sq| rects_intersect(norm_rect(sq), ray)))
                {
                    return Err(CellShiftConflict::ProtectedRange);
                }
                let scenario_hit = sheet
                    .scenarios
                    .iter()
                    .any(|sc| sc.cells.iter().any(|&(pos, _)| point_in_rect(pos, ray)))
                    || sheet.scenario_settings.sqref.as_deref().is_some_and(|sq| {
                        crate::scenarios::sqref_rects(sq)
                            .into_iter()
                            .any(|r| rects_intersect(r, ray))
                    });
                if scenario_hit {
                    return Err(CellShiftConflict::Scenario);
                }
                Ok(())
            }
            Self::RichAndHyperlinks => preflight_hyperlinks_for_shift(sheet, &op, ray),
            Self::CellNotesAndThreads
            | Self::ConditionalAndValidation
            | Self::DefinedNames
            | Self::ExternalSources
            | Self::IgnoredErrors
            | Self::LayoutAndPage => Ok(()),
        }
    }

    /// Run complete preflight across geometry bounds and every [`CoordParticipant`].
    pub fn preflight_all(wb: &Workbook, tx: &CoordTransform) -> Result<(), CellShiftConflict> {
        let sheet_idx = tx.target_sheet();
        if wb.sheets.get(sheet_idx).is_none() {
            return Err(CellShiftConflict::InvalidTarget);
        }
        let limit = tx.axis_limit();
        match tx {
            CoordTransform::AxisShift {
                band: Some(_), ..
            } => {
                let Some(op) = tx.as_cell_shift() else {
                    return Err(CellShiftConflict::InvalidTarget);
                };
                if op.count() == 0 {
                    return Err(CellShiftConflict::InvalidTarget);
                }
                for participant in Self::CELL_SHIFT_PREFLIGHT_ORDER {
                    participant.preflight(wb, tx)?;
                }
                Ok(())
            }
            CoordTransform::AxisShift {
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
            CoordTransform::AxisPermute { from, n, at, .. } => {
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
            CoordTransform::MultiInsert { positions, .. } => {
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

    /// Run object-level preflight (all non-overflow, non-merge participants) for `CellShift`.
    pub(crate) fn preflight_cell_shift_objects(
        wb: &Workbook,
        sheet_idx: usize,
        op: &CellShift,
    ) -> Result<(), CellShiftConflict> {
        if wb.sheets.get(sheet_idx).is_none() || op.count() == 0 {
            return Err(CellShiftConflict::InvalidTarget);
        }
        let tx = CoordTransform::from_cell_shift(sheet_idx, op);
        for participant in Self::CELL_SHIFT_PREFLIGHT_ORDER {
            if matches!(participant, Self::CellsAndStyles | Self::Merges) {
                continue;
            }
            participant.preflight(wb, &tx)?;
        }
        Ok(())
    }

    /// Run merge-only preflight for `CellShift`.
    pub(crate) fn preflight_cell_shift_merges(
        sheet: &Sheet,
        op: &CellShift,
    ) -> Result<(), CellShiftConflict> {
        preflight_merges_for_shift(sheet, op)
    }

    /// Apply this participant's workbook-level reference rewrite step.
    pub(crate) fn apply_workbook_refs(
        self,
        wb: &mut Workbook,
        tx: &CoordTransform,
        ctx: &WorkbookRefCtx,
    ) {
        match self {
            Self::ExternalSources => {
                crate::external::remap_sources(
                    wb,
                    ctx.sheet_idx,
                    tx.axis(),
                    tx.band(),
                    |lo, hi| tx.map_range(lo, hi),
                );
            }
            Self::RichAndHyperlinks => {
                if ctx.is_band {
                    let target = &mut wb.sheets[ctx.sheet_idx];
                    crate::transform::rewrite_target_hyperlink_locations(target, tx);
                }
            }
            Self::CellsAndStyles => {
                for sheet in &mut wb.sheets {
                    tx.rewrite_sheet_cell_formulas(sheet, &ctx.target_name);
                }
            }
            Self::ConditionalAndValidation => {
                for sheet in &mut wb.sheets {
                    tx.rewrite_sheet_cf_dv(sheet, &ctx.target_name);
                }
            }
            Self::Charts => {
                for sheet in &mut wb.sheets {
                    let own_name = sheet.name.clone();
                    for ch in sheet.charts.iter_mut() {
                        if ch.rewrite_with_transform(tx, &ctx.target_name, &own_name) {
                            sheet.charts_dirty = true;
                        }
                    }
                }
            }
            Self::Sparklines => {
                if ctx.is_band {
                    tx.remap_band_sparklines(wb, ctx.sheet_idx, &ctx.target_name);
                } else {
                    for sheet in &mut wb.sheets {
                        tx.remap_sheet_qualified_sparklines(sheet, &ctx.target_name);
                    }
                }
            }
            Self::DefinedNames => {
                for (_, scope, text) in &mut wb.names {
                    let own_name = scope
                        .and_then(|i| wb.sheets.get(i))
                        .map(|s| s.name.as_str())
                        .unwrap_or("");
                    let rewritten = tx.rewrite_formula(text, &ctx.target_name, own_name);
                    if rewritten != *text {
                        *text = rewritten;
                        wb.names_dirty = true;
                    }
                }
            }
            Self::Pivots => {
                if !ctx.is_band {
                    tx.record_pivot_shifts(wb, &ctx.target_name);
                    tx.remap_pivots(wb, ctx.sheet_idx, ctx.target_sid, &ctx.target_name);
                }
            }
            Self::CellNotesAndThreads
            | Self::Merges
            | Self::IgnoredErrors
            | Self::Tables
            | Self::ArraysAndSpills
            | Self::AutoFilter
            | Self::ScenariosAndProtectedRanges
            | Self::LayoutAndPage => {}
        }
    }

    /// Dispatch workbook-level reference updates across all participants.
    pub(crate) fn apply_all_workbook_refs(wb: &mut Workbook, tx: &CoordTransform) {
        if tx.is_noop() {
            return;
        }
        let sheet_idx = tx.target_sheet();
        let Some(target_name) = wb.sheets.get(sheet_idx).map(|s| s.name.clone()) else {
            return;
        };
        let target_sid = wb.sheets[sheet_idx].identity;
        let ctx = WorkbookRefCtx {
            sheet_idx,
            target_name,
            target_sid,
            is_band: tx.band().is_some(),
        };
        wb.bump_coord_epoch();
        for participant in Self::ALL {
            if participant.participates_in_workbook_refs() {
                participant.apply_workbook_refs(wb, tx, &ctx);
            }
        }
    }

    /// Whether this participant currently has metadata that a partial `CellShift`
    /// could mutate, requiring a `StructureSnapshot` before applying the shift.
    pub fn has_cell_shift_metadata(self, wb: &Workbook, target_sheet: usize) -> bool {
        let Some(target) = wb.sheets.get(target_sheet) else {
            return false;
        };
        match self {
            Self::CellNotesAndThreads => !target.notes.is_empty() || !target.threads.is_empty(),
            Self::RichAndHyperlinks => !target.rich.is_empty() || !target.hyperlinks.is_empty(),
            Self::Merges => !target.merges.is_empty(),
            Self::IgnoredErrors => !target.ignored_errors.is_empty(),
            Self::ConditionalAndValidation => wb
                .sheets
                .iter()
                .any(|sh| !sh.cf.blocks.is_empty() || !sh.dv.is_empty()),
            Self::Charts => wb.sheets.iter().any(|sh| !sh.charts.is_empty()),
            Self::Sparklines => wb.sheets.iter().any(|sh| !sh.sparklines.is_empty()),
            Self::DefinedNames => !wb.names.is_empty(),
            Self::ExternalSources => !wb.sources.is_empty(),
            Self::CellsAndStyles
            | Self::Tables
            | Self::Pivots
            | Self::ArraysAndSpills
            | Self::AutoFilter
            | Self::ScenariosAndProtectedRanges
            | Self::LayoutAndPage => false,
        }
    }

    /// True if any metadata-tracking participant has state on `wb`.
    pub fn any_cell_shift_metadata(wb: &Workbook, target_sheet: usize) -> bool {
        wb.sheets.get(target_sheet).is_some()
            && Self::ALL
                .iter()
                .any(|p| p.tracks_cell_shift_metadata() && p.has_cell_shift_metadata(wb, target_sheet))
    }

    /// Whether this participant's metadata actually changed during a `CellShift`.
    pub(crate) fn cell_shift_metadata_changed(self, view: &CellShiftMetaDiffView<'_>) -> bool {
        match self {
            Self::CellNotesAndThreads => {
                view.target_before.notes != view.target_after.notes
                    || view.target_before.threads != view.target_after.threads
            }
            Self::RichAndHyperlinks => {
                view.target_before.rich != view.target_after.rich
                    || view.target_before.hyperlinks != view.target_after.hyperlinks
            }
            Self::Merges => view.target_before.merges != view.target_after.merges,
            Self::IgnoredErrors => {
                view.target_before.ignored_errors != view.target_after.ignored_errors
            }
            Self::ConditionalAndValidation => {
                view.target_before.cf != view.target_after.cf
                    || view.target_before.dv != view.target_after.dv
                    || view.foreign_cf_changed
                    || view.foreign_dv_changed
            }
            Self::Charts => {
                !chart_refs_match(&view.target_before.charts, &view.target_after.charts, false)
                    || view.foreign_charts_changed
            }
            Self::Sparklines => {
                view.target_before.sparklines != view.target_after.sparklines
                    || view.foreign_sparklines_changed
            }
            Self::DefinedNames => view.names_before != view.wb_after.names.as_slice(),
            Self::ExternalSources => view.sources_before != view.wb_after.sources.as_slice(),
            Self::CellsAndStyles
            | Self::Tables
            | Self::Pivots
            | Self::ArraysAndSpills
            | Self::AutoFilter
            | Self::ScenariosAndProtectedRanges
            | Self::LayoutAndPage => false,
        }
    }

    /// True if any metadata-tracking participant changed during a `CellShift`.
    pub(crate) fn any_cell_shift_metadata_changed(view: &CellShiftMetaDiffView<'_>) -> bool {
        Self::ALL
            .iter()
            .any(|p| p.tracks_cell_shift_metadata() && p.cell_shift_metadata_changed(view))
    }

    /// Clone a single participant's sheet-level state into a `CellShiftPatch::prepare`
    /// dry-run sheet copy, keeping `prepare` strictly synchronized with `CoordParticipant::ALL`.
    pub(crate) fn clone_sheet_for_cell_shift_dry_run(
        self,
        src: &Sheet,
        dst: &mut Sheet,
        is_target: bool,
    ) {
        match self {
            // `search_snapshot()` already shares cells, styles, row_styles, col_styles.
            Self::CellsAndStyles => {}
            Self::CellNotesAndThreads => {
                if is_target {
                    dst.notes = src.notes.clone();
                    dst.threads = src.threads.clone();
                }
            }
            Self::RichAndHyperlinks => {
                if is_target {
                    dst.rich = src.rich.clone();
                    dst.hyperlinks = src.hyperlinks.clone();
                }
            }
            Self::Merges => {
                if is_target {
                    dst.merges = src.merges.clone();
                }
            }
            Self::ConditionalAndValidation => {
                dst.cf = src.cf.clone();
                dst.dv = src.dv.clone();
            }
            Self::Charts => {
                dst.charts = src.charts.clone();
            }
            Self::Sparklines => {
                dst.sparklines = src.sparklines.clone();
            }
            Self::IgnoredErrors => {
                if is_target {
                    dst.ignored_errors = src.ignored_errors.clone();
                }
            }
            Self::ArraysAndSpills => {
                if is_target {
                    dst.spilled = src.spilled.clone();
                }
            }
            Self::DefinedNames
            | Self::ExternalSources
            | Self::Tables
            | Self::Pivots
            | Self::AutoFilter
            | Self::ScenariosAndProtectedRanges
            | Self::LayoutAndPage => {}
        }
    }

    /// Build a lightweight CoW workbook clone containing every participant needed
    /// to dry-run `CellShift::apply` inside `CellShiftPatch::prepare`.
    pub(crate) fn clone_workbook_for_cell_shift_dry_run(
        wb: &Workbook,
        target_sheet: usize,
    ) -> Workbook {
        Workbook {
            styles: wb.styles.clone(),
            names: wb.names.clone(),
            sources: wb.sources.clone(),
            sheets: wb
                .sheets
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    let mut copy = s.search_snapshot();
                    let is_target = i == target_sheet;
                    for participant in Self::ALL {
                        participant.clone_sheet_for_cell_shift_dry_run(s, &mut copy, is_target);
                    }
                    copy
                })
                .collect(),
            ..Default::default()
        }
    }
}

fn preflight_merges_for_shift(sheet: &Sheet, op: &CellShift) -> Result<(), CellShiftConflict> {
    let Some(ray) = op.moving_ray() else {
        return Err(CellShiftConflict::InvalidTarget);
    };
    let (r1, c1, r2, c2) = op.rect;
    for &raw in sheet.merges.iter() {
        let m = norm_rect(raw);
        if !rects_intersect(m, ray) {
            continue;
        }
        let supported = match op.direction {
            crate::cells::Shift::Vertical => {
                m.1 >= c1
                    && m.3 <= c2
                    && if op.insert {
                        m.0 >= r1
                    } else {
                        (m.0 >= r1 && m.2 <= r2) || m.0 > r2
                    }
            }
            crate::cells::Shift::Horizontal => {
                m.0 >= r1
                    && m.2 <= r2
                    && if op.insert {
                        m.1 >= c1
                    } else {
                        (m.1 >= c1 && m.3 <= c2) || m.1 > c2
                    }
            }
        };
        if !supported {
            return Err(CellShiftConflict::PartialMerge);
        }
    }
    Ok(())
}

fn preflight_hyperlinks_for_shift(
    sheet: &Sheet,
    op: &CellShift,
    ray: (u32, u32, u32, u32),
) -> Result<(), CellShiftConflict> {
    let (r1, c1, r2, c2) = op.rect;
    for (&(hr, hc), link) in sheet.hyperlinks.iter() {
        if link.span == (0, 0) {
            continue;
        }
        let rect = (
            hr,
            hc,
            hr.saturating_add(link.span.0),
            hc.saturating_add(link.span.1),
        );
        if !rects_intersect(rect, ray) {
            continue;
        }
        let supported = match op.direction {
            crate::cells::Shift::Vertical => {
                rect.1 >= c1
                    && rect.3 <= c2
                    && if op.insert {
                        rect.0 >= r1
                    } else {
                        (rect.0 >= r1 && rect.2 <= r2) || rect.0 > r2
                    }
            }
            crate::cells::Shift::Horizontal => {
                rect.0 >= r1
                    && rect.2 <= r2
                    && if op.insert {
                        rect.1 >= c1
                    } else {
                        (rect.1 >= c1 && rect.3 <= c2) || rect.1 > c2
                    }
            }
        };
        if !supported {
            return Err(CellShiftConflict::PartialHyperlink);
        }
    }
    Ok(())
}

fn preflight_sparklines_for_shift(
    wb: &Workbook,
    sheet_idx: usize,
    op: &CellShift,
    tx: &CoordTransform,
    ray: (u32, u32, u32, u32),
) -> Result<(), CellShiftConflict> {
    let target_name = wb.sheets[sheet_idx].name.as_str();
    let band = tx.band();
    for (si, sh) in wb.sheets.iter().enumerate() {
        for sp in &sh.sparklines {
            // 1. Target-sheet anchor check: insert overflow fails with Overflow;
            //    delete of anchor drops the sparkline cleanly (skip data-rect check).
            if si == sheet_idx && point_in_rect((sp.r, sp.c), ray) {
                if op.insert {
                    if tx.map_cell(sp.r, sp.c).is_none() {
                        return Err(CellShiftConflict::Overflow);
                    }
                } else if tx.map_cell(sp.r, sp.c).is_none() {
                    continue;
                }
            }

            // 2. Data-rect check for same-sheet and cross-sheet sparklines targeting `sheet_idx`.
            let targets_sheet = (si == sheet_idx && sp.source_sheet.is_none())
                || sp
                    .source_sheet
                    .as_deref()
                    .is_some_and(|n| n.eq_ignore_ascii_case(target_name));
            if !targets_sheet {
                continue;
            }
            let rect = norm_rect(sp.rect);
            if !rects_intersect(rect, ray) {
                continue;
            }
            if tx.map_rect(rect).is_none() {
                // Distinguish clean grid overflow of a wholly-in-band rect on insert
                // from a partial band-straddle / split / emptied data range.
                let wholly_in_band = band.is_some_and(|(lo, hi)| match tx.axis() {
                    crate::refs::Axis::Row => rect.1 >= lo && rect.3 <= hi,
                    crate::refs::Axis::Col => rect.0 >= lo && rect.2 <= hi,
                });
                if op.insert && wholly_in_band {
                    return Err(CellShiftConflict::Overflow);
                }
                return Err(CellShiftConflict::Sparkline);
            }
        }
    }
    Ok(())
}
