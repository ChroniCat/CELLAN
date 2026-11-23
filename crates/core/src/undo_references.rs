//! References entered outside undo history can otherwise silently change their
//! referent when an older structural snapshot restores different coordinates.
use crate::{
    CellRef, Workbook,
    cf::{CfKind, CfvoKind},
    refs::{Axis, Shift},
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
    pub(super) fn covers(&self, wb: &Workbook, target: usize, op: &Shift) -> bool {
        let mut covered = true;
        visit(wb, target, op, |entry| covered &= self.0.contains(&entry));
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

pub(super) fn collect(wb: &Workbook, target: usize, op: &Shift) -> Uses {
    let mut uses = Uses::default();
    visit(wb, target, op, |entry| {
        uses.0.insert(entry);
    });
    uses
}

fn visit(wb: &Workbook, target: usize, op: &Shift, mut emit: impl FnMut((Location, Arc<str>))) {
    let mut shift = op.clone();
    let target_name = &wb.sheets[target].name;
    shift.target_sheet.clone_from(target_name);
    for (index, sheet) in wb.sheets.iter().enumerate() {
        if index == target {
            continue;
        }
        shift.own_sheet.clone_from(&sheet.name);
        let id = sheet.identity;
        let changes = |text: &str| text.contains('!') && crate::refs::rewrite(text, &shift) != text;
        for (key, cell) in sheet.foreign_formula_cells(target_name) {
            if let Some(text) = cell.formula.as_ref().filter(|f| changes(f)) {
                emit((Location::Cell(id, key), text.clone()));
            }
        }
        let mut record = |loc, text: &str| {
            if changes(text) {
                emit((loc, text.into()));
            }
        };
        for (i, dv) in sheet.dv.iter().enumerate() {
            record(Location::Validation(id, i, 0), &dv.f1);
            record(Location::Validation(id, i, 1), &dv.f2);
        }
        for (i, block) in sheet.cf.blocks.iter().enumerate() {
            for (j, rule) in block.rules.iter().enumerate() {
                let mut add = |slot, text: &str| record(Location::Condition(id, i, j, slot), text);
                match &rule.kind {
                    CfKind::Expr { formula, .. } => add(0, formula),
                    CfKind::CellIs { a, b, .. } => {
                        add(0, a);
                        add(1, b);
                    }
                    CfKind::Scale { points } => {
                        for (k, (v, _)) in points.iter().enumerate() {
                            if v.kind == CfvoKind::Formula {
                                add(k, &v.formula);
                            }
                        }
                    }
                    CfKind::Bar { min, max, .. } => {
                        for (k, v) in [min, max].into_iter().enumerate() {
                            if v.kind == CfvoKind::Formula {
                                add(k, &v.formula);
                            }
                        }
                    }
                    CfKind::IconSet { thresholds, .. } => {
                        for (k, v) in thresholds.iter().enumerate() {
                            if v.kind == CfvoKind::Formula {
                                add(k, &v.formula);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        for (i, chart) in sheet.charts.iter().enumerate() {
            for (j, series) in chart.series.iter().enumerate() {
                for (k, text) in [&series.name_ref, &series.cat_ref, &series.val_ref]
                    .into_iter()
                    .enumerate()
                {
                    record(Location::Chart(id, i, j, k), text);
                }
            }
        }
        for (&(row, col), link) in sheet.hyperlinks.iter() {
            if !link.external {
                record(Location::Hyperlink(id, row, col), &link.location);
            }
        }
        for sp in &sheet.sparklines {
            if sp
                .source_sheet
                .as_deref()
                .is_some_and(|n| n.eq_ignore_ascii_case(target_name))
                && rect_moves(&shift, sp.rect)
            {
                emit((Location::Spark(id, sp.r, sp.c, sp.rect), "".into()));
            }
        }
    }
    for (name, scope, text) in &wb.names {
        shift.own_sheet = scope
            .and_then(|i| wb.sheets.get(i))
            .map(|s| s.name.clone())
            .unwrap_or_default();
        if crate::refs::rewrite(text, &shift) != *text {
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
        if let crate::external::Kind::Internal { sheet, rect } = &source.kind
            && sheet.eq_ignore_ascii_case(target_name)
            && rect_moves(&shift, *rect)
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
