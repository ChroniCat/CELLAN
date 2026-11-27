//! References entered outside undo history can otherwise silently change their
//! referent when an older structural snapshot restores different coordinates.
use crate::{
    CellRef, Workbook,
    refs::{Axis, AxisMove, Shift},
    transform::CoordTransform,
};
use std::{collections::HashSet, sync::Arc};

#[derive(Clone, Debug, Hash, Eq, PartialEq)]
enum Location {
    Cell(u64, CellRef),
    Name(Option<usize>, String),
    Validation(u64, usize, usize),
    Condition(u64, usize, usize, usize),
    Chart(u64, usize, usize, usize),
    ChartSource(u64, usize),
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
    PivotModel {
        id: u64,
        target_sheet: usize,
        anchor: (u32, u32),
        rect: (u32, u32, u32, u32),
        source: (u32, u32, u32, u32),
        field_cols: Box<[u32]>,
    },
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
                            Location::PivotModel { field_cols, .. } => {
                                field_cols.len() * size_of::<u32>()
                            }
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
    let tx = CoordTransform::from_shift_and_move(target, op, movement);
    if tx.is_noop() {
        return;
    }
    let target_name = &wb.sheets[target].name;
    let target_sid = wb.sheets[target].identity;
    let axis = tx.axis();
    let whole_axis_col = axis == Axis::Col && tx.band().is_none();
    let index_changes = |pos: u32| tx.map_index(pos) != Some(pos);
    let fast_filter_ok = !target_name.contains('\'')
        && target_name
            .chars()
            .all(|c| c.is_ascii() || (!c.is_uppercase() && !c.is_lowercase()));

    for (index, sheet) in wb.sheets.iter().enumerate() {
        if index == target {
            continue;
        }
        let own_name = sheet.name.as_str();
        let id = sheet.identity;
        let changes = |text: &str| {
            may_mention_foreign_sheet(text, target_name, fast_filter_ok)
                && tx.formula_may_be_affected(text)
                && tx.rewrite_formula(text, target_name, own_name) != text
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
                rule.kind.visit_formulas(|slot, text| {
                    if changes(text) {
                        emit((Location::Condition(id, i, j, slot), text.into()));
                        cf_changed = true;
                    }
                });
            }
        }
        let mut charts_changed = false;
        for (i, chart) in sheet.charts.iter().enumerate() {
            for (j, series) in chart.series.iter().enumerate() {
                for (k, text) in [
                    &series.name_ref,
                    &series.cat_ref,
                    &series.val_ref,
                    &series.size_ref,
                ]
                .into_iter()
                .enumerate()
                {
                    if !text.is_empty() && changes(text) {
                        emit((Location::Chart(id, i, j, k), text.as_str().into()));
                        charts_changed = true;
                    }
                }
            }
            if !chart.source_range.is_empty() && changes(&chart.source_range) {
                emit((Location::ChartSource(id, i), chart.source_range.as_str().into()));
                charts_changed = true;
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
            if same_source && tx.rect_changes(sp.rect) {
                sparklines_ref_target = true;
                emit((Location::Spark(id, sp.r, sp.c, sp.rect), "".into()));
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
        let own_name = scope
            .and_then(|i| wb.sheets.get(i))
            .map(|s| s.name.as_str())
            .unwrap_or("");
        if tx.rewrite_formula(text, target_name, own_name) != *text {
            emit((Location::Name(*scope, name.clone()), text.as_str().into()));
        }
    }
    for (index, spec, _, out, label) in &wb.pivots_pending {
        if *index == target
            && (tx.rect_changes(spec.source)
                || tx.rect_changes(*out)
                || (whole_axis_col
                    && [Some(spec.row_field), spec.col_field, Some(spec.value_field)]
                        .into_iter()
                        .flatten()
                        .any(index_changes)))
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
        if *sheet == target {
            let col_field_changes = whole_axis_col
                && (spec.rows.iter().any(|f| index_changes(f.col))
                    || spec.cols.iter().any(|f| index_changes(f.col))
                    || spec.values.iter().any(|f| {
                        index_changes(f.col)
                            || f.base.as_ref().is_some_and(|(bc, _)| index_changes(*bc))
                    })
                    || spec.filters.iter().any(|f| index_changes(f.col)));
            if tx.rect_changes(spec.source) || tx.rect_changes(*out) || col_field_changes {
                emit((Location::MultiPivot(i, spec.source, *out), "".into()));
            }
        }
    }
    for p in &wb.pivots {
        let is_src = p.source_sheet_id == Some(target_sid)
            || p.source_sheet.eq_ignore_ascii_case(target_name);
        let is_tgt = p.target_sheet == target;
        if !is_src && !is_tgt {
            continue;
        }
        let src_changes = is_src
            && (tx.rect_changes(p.spec.source)
                || (whole_axis_col
                    && (p.spec.rows.iter().any(|f| index_changes(f.col))
                        || p.spec.cols.iter().any(|f| index_changes(f.col))
                        || p.spec.values.iter().any(|f| {
                            index_changes(f.col)
                                || f.base.as_ref().is_some_and(|(bc, _)| index_changes(*bc))
                        })
                        || p.spec.filters.iter().any(|f| index_changes(f.col))
                        || p.options.slicers.iter().any(|s| index_changes(s.col)))));
        let tgt_changes = is_tgt
            && (tx.rect_changes(p.rect) || tx.map_cell(p.anchor.0, p.anchor.1) != Some(p.anchor));
        if src_changes || tgt_changes {
            let mut field_cols = Vec::new();
            if whole_axis_col && is_src {
                field_cols.extend(p.spec.rows.iter().map(|f| f.col));
                field_cols.extend(p.spec.cols.iter().map(|f| f.col));
                for v in &p.spec.values {
                    field_cols.push(v.col);
                    if let Some((bc, _)) = &v.base {
                        field_cols.push(*bc);
                    }
                }
                field_cols.extend(p.spec.filters.iter().map(|f| f.col));
                field_cols.extend(p.options.slicers.iter().map(|s| s.col));
            }
            emit((
                Location::PivotModel {
                    id: p.id,
                    target_sheet: p.target_sheet,
                    anchor: p.anchor,
                    rect: p.rect,
                    source: p.spec.source,
                    field_cols: field_cols.into_boxed_slice(),
                },
                p.source_sheet.as_str().into(),
            ));
        }
    }
    for source in &wb.sources {
        if matches!(source.kind, crate::external::Kind::Unbound(_)) {
            continue;
        }
        if source
            .target_sheet
            .as_ref()
            .is_none_or(|name| name.eq_ignore_ascii_case(target_name))
            && tx.rect_changes(source.destination_rect())
        {
            emit((
                Location::SourceDestination(source.at, source.rows, source.cols),
                source.name.as_str().into(),
            ));
        }
        if let crate::external::Kind::Internal { sheet, rect } = &source.kind
            && sheet.eq_ignore_ascii_case(target_name)
            && tx.rect_changes(*rect)
        {
            emit((Location::Source(*rect), source.name.as_str().into()));
        }
    }
}
