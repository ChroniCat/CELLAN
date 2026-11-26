//! References entered outside undo history can otherwise silently change their
//! referent when an older structural snapshot restores different coordinates.
use crate::{
    CellRef, Workbook,
    cf::{CfKind, CfvoKind},
    refs::{Axis, AxisMove, Shift},
};
use std::{collections::HashSet, sync::Arc};

#[derive(Clone, Debug, Hash, Eq, PartialEq)]
enum Location {
    Cell(u64, CellRef),
    Name(Option<usize>, String),
    Validation(u64, usize, usize),
    Condition(u64, usize, usize, usize),
    Chart(u64, usize, usize, usize),
    Hyperlink(u64, u32, u32),
    Spark(u64, u32, u32, (u32, u32, u32, u32)),
    Source((u32, u32, u32, u32)),
    SourceDestination((u32, u32), u32, u32),
    Pivot(
        (u32, u32, u32, u32),
        (u32, u32, u32, u32),
        u32,
        Option<u32>,
        u32,
    ),
    MultiPivot(usize, (u32, u32, u32, u32), (u32, u32, u32, u32)),
}

#[derive(Clone, Debug, Default)]
pub(super) struct Uses(HashSet<(Location, Arc<str>)>);

impl Uses {
    pub(super) fn covers(&self, wb: &Workbook, target: usize, op: &Shift, movement: Option<AxisMove>) -> bool {
        let mut covered = true;
        visit(wb, target, op, movement, None, |entry| covered &= self.0.contains(&entry));
        covered
    }

    pub(super) fn bytes(&self) -> usize {
        self.0.capacity() * size_of::<(Location, Arc<str>)>()
            + self
                .0
                .iter()
                .map(|(loc, text)| {
                    text.len()
                        + match loc {
                            Location::Name(_, name) => name.len(),
                            _ => 0,
                        }
                })
                .sum::<usize>()
    }
}

#[inline]
fn contains_ascii_ci(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    if haystack.len() < needle.len() {
        return false;
    }
    let first = needle[0];
    let first_lo = first.to_ascii_lowercase();
    let first_hi = first.to_ascii_uppercase();
    let end = haystack.len() - needle.len();
    let mut i = 0;
    while i <= end {
        let b = haystack[i];
        if (b == first_lo || b == first_hi)
            && haystack[i..i + needle.len()].eq_ignore_ascii_case(needle)
        {
            return true;
        }
        i += 1;
    }
    false
}

#[inline]
fn may_mention_foreign_sheet(text: &str, target_name: &str, fast_byte_filter_ok: bool) -> bool {
    if !text.contains('!') {
        return false;
    }
    if fast_byte_filter_ok {
        contains_ascii_ci(text.as_bytes(), target_name.as_bytes())
    } else {
        true
    }
}

pub(super) fn collect(wb: &Workbook, target: usize, op: &Shift, movement: Option<AxisMove>) -> Uses {
    let mut uses = Uses::default();
    visit(wb, target, op, movement, None, |entry| {
        uses.0.insert(entry);
    });
    uses
}

/// Single-pass foreign reference + `Uses` collector for `snapshot_structure`,
/// avoiding rewriting every foreign formula/CF/DV/chart twice.
pub(super) fn collect_with_foreign(
    wb: &Workbook,
    target: usize,
    op: &Shift,
    movement: Option<AxisMove>,
) -> (Uses, Vec<super::ForeignReferences>) {
    let mut uses = Uses::default();
    let mut foreign = Vec::new();
    visit(wb, target, op, movement, Some(&mut foreign), |entry| {
        uses.0.insert(entry);
    });
    (uses, foreign)
}

fn visit(
    wb: &Workbook,
    target: usize,
    op: &Shift,
    movement: Option<AxisMove>,
    mut foreign_out: Option<&mut Vec<super::ForeignReferences>>,
    mut emit: impl FnMut((Location, Arc<str>)),
) {
    let mut shift = op.clone();
    let target_name = &wb.sheets[target].name;
    shift.target_sheet.clone_from(target_name);
    let rewrite = |text: &str, shift: &Shift| match movement {
        Some(mv) => mv.rewrite(text, target_name, &shift.own_sheet),
        None => crate::refs::rewrite(text, shift),
    };
    let fast_filter_ok = !target_name.contains('\'')
        && target_name
            .chars()
            .all(|c| c.is_ascii() || (!c.is_uppercase() && !c.is_lowercase()));
    for (index, sheet) in wb.sheets.iter().enumerate() {
        if index == target {
            continue;
        }
        shift.own_sheet.clone_from(&sheet.name);
        let id = sheet.identity;
        let changes = |text: &str| {
            may_mention_foreign_sheet(text, target_name, fast_filter_ok)
                && rewrite(text, &shift) != text
        };
        let capture_foreign = foreign_out.is_some();
        let mut foreign_cells = Vec::new();
        for (key, cell) in sheet.foreign_formula_cells(target_name) {
            if let Some(text) = cell.formula.as_ref().filter(|f| changes(f)) {
                emit((Location::Cell(id, key), text.clone()));
                if capture_foreign {
                    foreign_cells.push((key, cell.clone()));
                }
            }
        }
        let mut dv_changed = false;
        for (i, dv) in sheet.dv.iter().enumerate() {
            if changes(&dv.f1) {
                emit((Location::Validation(id, i, 0), dv.f1.as_str().into()));
                dv_changed = true;
            }
            if changes(&dv.f2) {
                emit((Location::Validation(id, i, 1), dv.f2.as_str().into()));
                dv_changed = true;
            }
        }
        let mut cf_changed = false;
        for (i, block) in sheet.cf.blocks.iter().enumerate() {
            for (j, rule) in block.rules.iter().enumerate() {
                let mut add = |slot, text: &str, capture_cf: bool| {
                    if changes(text) {
                        emit((Location::Condition(id, i, j, slot), text.into()));
                        if capture_cf {
                            cf_changed = true;
                        }
                    }
                };
                match &rule.kind {
                    CfKind::Expr { formula, .. } => add(0, formula, true),
                    CfKind::CellIs { a, b, .. } => {
                        add(0, a, true);
                        add(1, b, true);
                    }
                    CfKind::Scale { points } => {
                        for (k, (v, _)) in points.iter().enumerate() {
                            if v.kind == CfvoKind::Formula {
                                add(k, &v.formula, false);
                            }
                        }
                    }
                    CfKind::Bar { min, max, .. } => {
                        for (k, v) in [min, max].into_iter().enumerate() {
                            if v.kind == CfvoKind::Formula {
                                add(k, &v.formula, false);
                            }
                        }
                    }
                    CfKind::IconSet { thresholds, .. } => {
                        for (k, v) in thresholds.iter().enumerate() {
                            if v.kind == CfvoKind::Formula {
                                add(k, &v.formula, false);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        let mut charts_changed = false;
        for (i, chart) in sheet.charts.iter().enumerate() {
            for (j, series) in chart.series.iter().enumerate() {
                for (k, text) in [&series.name_ref, &series.cat_ref, &series.val_ref]
                    .into_iter()
                    .enumerate()
                {
                    if changes(text) {
                        emit((Location::Chart(id, i, j, k), text.as_str().into()));
                        charts_changed = true;
                    }
                }
            }
        }
        for (&(row, col), link) in sheet.hyperlinks.iter() {
            if !link.external && changes(&link.location) {
                emit((Location::Hyperlink(id, row, col), link.location.as_str().into()));
            }
        }
        let mut sparklines_ref_target = false;
        for sp in &sheet.sparklines {
            let same_source = sp
                .source_sheet
                .as_deref()
                .is_some_and(|n| n.eq_ignore_ascii_case(target_name));
            if same_source {
                if sp.source_sheet.as_deref() == Some(target_name.as_str()) {
                    sparklines_ref_target = true;
                }
                if rect_moves(&shift, sp.rect) {
                    emit((Location::Spark(id, sp.r, sp.c, sp.rect), "".into()));
                }
            }
        }
        if let Some(out) = foreign_out.as_deref_mut() {
            let cf = cf_changed.then(|| sheet.cf.clone());
            let dv = dv_changed.then(|| sheet.dv.clone());
            let charts = charts_changed.then(|| sheet.charts.clone());
            let sparklines = sparklines_ref_target.then(|| sheet.sparklines.clone());
            if !foreign_cells.is_empty()
                || cf.is_some()
                || dv.is_some()
                || charts.is_some()
                || sparklines.is_some()
            {
                out.push(super::ForeignReferences {
                    sheet: index,
                    identity: id,
                    cells: foreign_cells,
                    cf,
                    dv,
                    charts,
                    sparklines,
                });
            }
        }
    }
    for (name, scope, text) in &wb.names {
        shift.own_sheet = scope
            .and_then(|i| wb.sheets.get(i))
            .map(|s| s.name.clone())
            .unwrap_or_default();
        if rewrite(text, &shift) != *text {
            emit((Location::Name(*scope, name.clone()), text.as_str().into()));
        }
    }
    for (index, spec, _, out, label) in &wb.pivots_pending {
        if *index == target
            && (rect_moves(&shift, spec.source)
                || rect_moves(&shift, *out)
                || shift.axis == Axis::Col
                    && [Some(spec.row_field), spec.col_field, Some(spec.value_field)]
                        .into_iter()
                        .flatten()
                        .any(|col| shift.map(col) != Some(col)))
        {
            emit((
                Location::Pivot(
                    spec.source,
                    *out,
                    spec.row_field,
                    spec.col_field,
                    spec.value_field,
                ),
                label.as_str().into(),
            ));
        }
    }
    for (i, (sheet, spec, _, out)) in wb.pivots_pending2.iter().enumerate() {
        if *sheet == target && (rect_moves(&shift, spec.source) || rect_moves(&shift, *out)) {
            emit((Location::MultiPivot(i, spec.source, *out), "".into()));
        }
    }
    for source in &wb.sources {
        if matches!(source.kind, crate::external::Kind::Unbound(_)) { continue; }
        let moved = |rect| match movement {
            Some(mv) => {
                let (r1,c1,r2,c2) = rect;
                let span = if mv.axis == Axis::Row { (r1,r2) } else { (c1,c2) };
                mv.contiguous_range(span.0, span.1) != Some(span)
            }
            None if shift.row_filter.is_some() || shift.col_filter.is_some() => {
                let (r1, c1, r2, c2) = rect;
                let cells = Shift { n: 1, ..shift.clone() };
                shift.row_filter.is_none_or(|(lo,hi)| r1 <= hi && lo <= r2)
                    && shift.col_filter.is_none_or(|(lo,hi)| c1 <= hi && lo <= c2)
                    && rect_moves(&cells, rect)
            }
            None => rect_moves(&shift, rect),
        };
        if source.target_sheet.as_ref().is_none_or(|name| name.eq_ignore_ascii_case(target_name)) {
            if moved(source.destination_rect()) {
                emit((Location::SourceDestination(source.at, source.rows, source.cols), source.name.as_str().into()));
            }
        }
        if let crate::external::Kind::Internal { sheet, rect } = &source.kind
            && sheet.eq_ignore_ascii_case(target_name)
            && moved(*rect)
        {
            emit((Location::Source(*rect), source.name.as_str().into()));
        }
    }
}

fn rect_moves(shift: &Shift, rect: (u32, u32, u32, u32)) -> bool {
    let bounds = match shift.axis {
        Axis::Row => (rect.0, rect.2),
        Axis::Col => (rect.1, rect.3),
    };
    shift.map_range(bounds.0, bounds.1) != Some(bounds)
}
