//! Sheet identity changes must keep formula qualifiers and sheet-indexed
//! metadata in step. Callers invalidate their dependency graph and indexed undo
//! history after a successful operation, then recalculate the workbook.

use crate::{model::{Sheet, Workbook}, refs::rewrite_sheet};

fn valid_name(wb: &Workbook, except: Option<usize>, name: &str) -> bool {
    !name.is_empty() && name.chars().count() <= 31
        && !name.chars().any(|c| c.is_control() || "[]:*?/\\".contains(c))
        && !name.starts_with('\'') && !name.ends_with('\'')
        && !wb.sheets.iter().enumerate().any(|(i, s)| Some(i) != except && s.name.eq_ignore_ascii_case(name))
}

/// Insert a new sheet while keeping existing local names and pending pivot
/// attachments associated with their original sheets.
pub fn insert_sheet(wb: &mut Workbook, idx: usize, mut sheet: Sheet) -> bool {
    if idx > wb.sheets.len() || !valid_name(wb, None, &sheet.name) { return false; }
    if wb.sheets.iter().any(|existing| existing.identity == sheet.identity) {
        sheet.renew_identity();
    }
    for (_, local, _) in &mut wb.names {
        if let Some(i) = local.as_mut() {
            if *i >= idx { *i += 1; wb.names_dirty = true; }
        }
    }
    for (i, ..) in &mut wb.pivots_pending {
        if *i >= idx { *i += 1; }
    }
    for (i, ..) in &mut wb.pivots_pending2 {
        if *i >= idx { *i += 1; }
    }
    for (i, ..) in &mut wb.pivot_calc_fields {
        if *i >= idx { *i += 1; }
    }
    for p in &mut wb.pivots {
        if p.target_sheet >= idx {
            p.target_sheet += 1;
        }
    }
    wb.sheets.insert(idx, sheet);
    wb.dirty = true;
    true
}

/// Rename an existing sheet. Invalid/duplicate names and unchanged names return
/// false without modifying the workbook.
pub fn rename_sheet(wb: &mut Workbook, idx: usize, name: &str) -> bool {
    rename_impl(wb, idx, name, None)
}

pub(crate) fn rename_with_history(wb: &mut Workbook, idx: usize, name: &str) -> Option<RenameReferences> {
    let mut changes = RenameReferences::default();
    rename_impl(wb, idx, name, Some(&mut changes)).then_some(changes)
}

fn rename_impl(wb: &mut Workbook, idx: usize, name: &str, journal: Option<&mut RenameReferences>) -> bool {
    let name = name.trim();
    if idx >= wb.sheets.len() || !valid_name(wb, Some(idx), name)
        || wb.sheets[idx].name == name
    {
        return false;
    }
    let old = wb.sheets[idx].name.clone();
    let sid = wb.sheets[idx].identity;
    wb.sheets[idx].name = name.to_owned();
    for p in &mut wb.pivots {
        if p.source_sheet_id == Some(sid) || p.source_sheet.eq_ignore_ascii_case(&old) {
            p.source_sheet = name.to_owned();
            p.source_sheet_id = Some(sid);
            if p.imported_part.is_none() {
                p.modified = true;
            }
        }
    }
    rewrite_qualifiers(wb, &old, Some(name), None, journal);
    wb.dirty = true;
    true
}

/// Delete a sheet, preserving at least one sheet. Deleted-sheet references
/// become explicit errors, so later reuse of its name cannot retarget them.
pub fn delete_sheet(wb: &mut Workbook, idx: usize) -> bool {
    delete_with_history(wb, idx).is_some()
}

#[derive(Clone, Default)]
pub(crate) struct DeletedSheetPivots {
    pub pivots: Vec<crate::pivot_engine::PivotTableModel>,
    pub pivots_pending: Vec<(
        usize,
        crate::pivot::PivotSpec,
        crate::pivot::PivotResult,
        (u32, u32, u32, u32),
        String,
    )>,
    pub pivots_pending2: Vec<(
        usize,
        crate::pivot_multi::PivotSpec2,
        crate::pivot_multi::PivotGrid,
        (u32, u32, u32, u32),
    )>,
    pub pivot_shifts: Vec<(String, u8, u32, u32, bool)>,
    pub pivot_calc_fields: Vec<(usize, String, String)>,
}

pub(crate) struct DeletedSheetRecord {
    pub sheet: Sheet,
    pub local_names: Vec<(String, Option<usize>, String)>,
    pub references: RenameReferences,
    pub pivots: DeletedSheetPivots,
}

pub(crate) fn delete_with_history(wb: &mut Workbook, idx: usize) -> Option<DeletedSheetRecord> {
    if wb.sheets.len() <= 1 || idx >= wb.sheets.len() { return None; }
    // 删除前保留完整标签顺序：三维区间的邻居必须按原区间求，
    // 不能用全局 prev/next（反向区间/唯一成员会取错）。
    let order: Vec<String> = wb.sheets.iter().map(|s| s.name.clone()).collect();
    let removed_sheet = wb.sheets.remove(idx);
    let old = removed_sheet.name.clone();
    let mut local_names = Vec::new();
    let mut kept_names = Vec::with_capacity(wb.names.len());
    for entry in std::mem::take(&mut wb.names) {
        if entry.1 == Some(idx) {
            local_names.push(entry);
            wb.names_dirty = true;
        } else {
            kept_names.push(entry);
        }
    }
    wb.names = kept_names;
    for (_, local, _) in &mut wb.names {
        if let Some(i) = local.as_mut() {
            if *i > idx { *i -= 1; wb.names_dirty = true; }
        }
    }
    let mut references = RenameReferences::default();
    rewrite_qualifiers(wb, &old, None, Some(&order), Some(&mut references));

    let mut deleted_shifts = Vec::new();
    let mut kept_shifts = Vec::with_capacity(wb.pivot_shifts.len());
    for entry in std::mem::take(&mut wb.pivot_shifts) {
        if entry.0.eq_ignore_ascii_case(&old) {
            deleted_shifts.push(entry);
        } else {
            kept_shifts.push(entry);
        }
    }
    wb.pivot_shifts = kept_shifts;

    let mut deleted_pending = Vec::new();
    let mut kept_pending = Vec::with_capacity(wb.pivots_pending.len());
    for mut entry in std::mem::take(&mut wb.pivots_pending) {
        if entry.0 == idx {
            deleted_pending.push(entry);
        } else {
            if entry.0 > idx { entry.0 -= 1; }
            kept_pending.push(entry);
        }
    }
    wb.pivots_pending = kept_pending;

    let mut deleted_pending2 = Vec::new();
    let mut kept_pending2 = Vec::with_capacity(wb.pivots_pending2.len());
    for mut entry in std::mem::take(&mut wb.pivots_pending2) {
        if entry.0 == idx {
            deleted_pending2.push(entry);
        } else {
            if entry.0 > idx { entry.0 -= 1; }
            kept_pending2.push(entry);
        }
    }
    wb.pivots_pending2 = kept_pending2;

    let mut deleted_calc_fields = Vec::new();
    let mut kept_calc_fields = Vec::with_capacity(wb.pivot_calc_fields.len());
    for mut entry in std::mem::take(&mut wb.pivot_calc_fields) {
        if entry.0 == idx {
            deleted_calc_fields.push(entry);
        } else {
            if entry.0 > idx { entry.0 -= 1; }
            kept_calc_fields.push(entry);
        }
    }
    wb.pivot_calc_fields = kept_calc_fields;

    let mut deleted_pivots = Vec::new();
    let mut removed_parts = Vec::new();
    let mut kept_pivots = Vec::with_capacity(wb.pivots.len());
    for mut p in std::mem::take(&mut wb.pivots) {
        if p.target_sheet == idx {
            if let Some(part) = &p.imported_part {
                removed_parts.push(part.clone());
            }
            deleted_pivots.push(p);
        } else {
            if p.target_sheet > idx {
                p.target_sheet -= 1;
            }
            kept_pivots.push(p);
        }
    }
    wb.pivots = kept_pivots;
    wb.pivots_removed_parts.extend(removed_parts.iter().cloned());

    wb.dirty = true;
    Some(DeletedSheetRecord {
        sheet: removed_sheet,
        local_names,
        references,
        pivots: DeletedSheetPivots {
            pivots: deleted_pivots,
            pivots_pending: deleted_pending,
            pivots_pending2: deleted_pending2,
            pivot_shifts: deleted_shifts,
            pivot_calc_fields: deleted_calc_fields,
        },
    })
}

pub(crate) fn restore_deleted_sheet(
    wb: &mut Workbook,
    idx: usize,
    sheet: Sheet,
    local_names: &[(String, Option<usize>, String)],
    references: &RenameReferences,
    deleted_pivots: &DeletedSheetPivots,
) -> bool {
    if idx > wb.sheets.len() || !valid_name(wb, None, &sheet.name) {
        return false;
    }
    // First shift names/pivots back up and re-insert sheet
    if !insert_sheet(wb, idx, sheet) {
        return false;
    }
    // Restore original sheet-scoped names
    for (n, _, f) in local_names {
        wb.names.push((n.clone(), Some(idx), f.clone()));
        wb.names_dirty = true;
    }
    // Restore cross-sheet formula qualifiers that were rewritten to #REF!
    // Note: field sheet indices in `references` were recorded AFTER removing `idx`,
    // so any sheet index >= idx in `references` now points to `si + 1` in `wb`.
    let shift_field = |f: &ReferenceField| -> ReferenceField {
        match *f {
            ReferenceField::Formula(si, k) => ReferenceField::Formula(if si >= idx { si + 1 } else { si }, k),
            ReferenceField::Conditional(si, bi, ri, n) => ReferenceField::Conditional(if si >= idx { si + 1 } else { si }, bi, ri, n),
            ReferenceField::Validation(si, vi, fi) => ReferenceField::Validation(if si >= idx { si + 1 } else { si }, vi, fi),
            ReferenceField::Chart(si, ci, s, fi) => ReferenceField::Chart(if si >= idx { si + 1 } else { si }, ci, s, fi),
            ReferenceField::Name(i) => ReferenceField::Name(i),
            ReferenceField::SourceInput(i) => ReferenceField::SourceInput(i),
            ReferenceField::SourceTarget(i) => ReferenceField::SourceTarget(i),
            ReferenceField::SparkSource(si, i) => ReferenceField::SparkSource(if si >= idx { si + 1 } else { si }, i),
            ReferenceField::Pivot(i) => ReferenceField::Pivot(i),
        }
    };
    let remapped: std::collections::HashMap<ReferenceField, (String, String)> = references
        .0
        .iter()
        .map(|(k, v)| (shift_field(k), v.clone()))
        .collect();
    visit_references(wb, |field, text| {
        let (before, after) = remapped.get(&field)?;
        (text == after).then(|| before.clone())
    });

    // Restore pivots and pivot metadata that belonged to the deleted sheet
    for mut p in deleted_pivots.pivots.iter().cloned() {
        p.target_sheet = idx;
        if let Some(part) = &p.imported_part {
            if let Some(pos) = wb.pivots_removed_parts.iter().position(|r| r == part) {
                wb.pivots_removed_parts.remove(pos);
            } else {
                // If an intermediate save already stripped the imported part from disk,
                // mark the restored pivot modified so the next save regenerates its XML.
                p.modified = true;
            }
        }
        wb.pivots.push(p);
    }
    for mut entry in deleted_pivots.pivots_pending.iter().cloned() {
        entry.0 = idx;
        wb.pivots_pending.push(entry);
    }
    for mut entry in deleted_pivots.pivots_pending2.iter().cloned() {
        entry.0 = idx;
        wb.pivots_pending2.push(entry);
    }
    for mut entry in deleted_pivots.pivot_calc_fields.iter().cloned() {
        entry.0 = idx;
        wb.pivot_calc_fields.push(entry);
    }
    wb.pivot_shifts.extend(deleted_pivots.pivot_shifts.iter().cloned());

    wb.dirty = true;
    true
}

/// Move a sheet from `from` to `to` (arbitrary reorder, shifting sheets in between).
pub fn reorder_sheet(wb: &mut Workbook, from: usize, to: usize) -> bool {
    if from == to || from >= wb.sheets.len() || to >= wb.sheets.len() {
        return false;
    }
    let mut cur = from;
    while cur < to {
        if !move_sheet(wb, cur, cur + 1) {
            return false;
        }
        cur += 1;
    }
    while cur > to {
        if !move_sheet(wb, cur, cur - 1) {
            return false;
        }
        cur -= 1;
    }
    true
}

/// Swap two tab positions, matching the app's move-left/move-right operation.
pub fn move_sheet(wb: &mut Workbook, from: usize, to: usize) -> bool {
    if from == to || from >= wb.sheets.len() || to >= wb.sheets.len() { return false; }
    wb.sheets.swap(from, to);
    let remap = |i: usize| if i == from { to } else if i == to { from } else { i };
    for (_, local, _) in &mut wb.names {
        if let Some(i) = local.as_mut() {
            let mapped = remap(*i);
            wb.names_dirty |= *i != mapped;
            *i = mapped;
        }
    }
    for (sheet, ..) in &mut wb.pivots_pending { *sheet = remap(*sheet); }
    for (sheet, ..) in &mut wb.pivots_pending2 { *sheet = remap(*sheet); }
    for p in &mut wb.pivots { p.target_sheet = remap(p.target_sheet); }
    wb.dirty = true;
    true
}

#[derive(Hash, Eq, PartialEq)]
enum ReferenceField {
    Formula(usize, crate::CellRef),
    Conditional(usize, usize, usize, usize),
    Validation(usize, usize, usize),
    Chart(usize, usize, usize, usize),
    Name(usize),
    SourceInput(usize),
    SourceTarget(usize),
    SparkSource(usize, usize),
    Pivot(usize),
}

#[derive(Default)]
pub(crate) struct RenameReferences(std::collections::HashMap<ReferenceField, (String, String)>);

impl RenameReferences {
    pub(crate) fn payload_bytes(&self) -> usize {
        self.0.capacity() * std::mem::size_of::<(ReferenceField, String, String)>()
            + self.0.values().map(|(old, new)| old.capacity() + new.capacity()).sum::<usize>()
    }

    pub(crate) fn restore(&self, wb: &mut Workbook, idx: usize, name: &str, redo: bool) -> bool {
        if !valid_name(wb, Some(idx), name) { return false; }
        let mut matched = 0;
        visit_references(wb, |field, text| {
            if let Some((before, after)) = self.0.get(&field) {
                if text == if redo { before } else { after } {
                    matched += 1;
                }
            }
            None
        });
        if matched != self.0.len() { return false; }
        wb.sheets[idx].name = name.to_owned();
        visit_references(wb, |field, _| {
            let (before, after) = self.0.get(&field)?;
            Some(if redo { after } else { before }.clone())
        });
        wb.dirty = true;
        true
    }
}

fn rewrite_qualifiers(
    wb: &mut Workbook,
    old: &str,
    new: Option<&str>,
    three_d_order: Option<&[String]>,
    mut journal: Option<&mut RenameReferences>,
) {
    visit_references(wb, |field, text| {
        let rewritten = match field {
            ReferenceField::Pivot(_) if new.is_none() => return None, // delete_sheet removes these
            ReferenceField::SparkSource(..) if text.eq_ignore_ascii_case(old) => new.unwrap_or_default().to_owned(),
            ReferenceField::SourceInput(_) | ReferenceField::SourceTarget(_) | ReferenceField::Pivot(_) if text.eq_ignore_ascii_case(old) =>
                new.unwrap_or("#REF!").to_owned(),
            ReferenceField::SourceInput(_) | ReferenceField::SourceTarget(_) | ReferenceField::SparkSource(..) | ReferenceField::Pivot(_) => return None,
            _ => match three_d_order {
                Some(order) => rewrite_sheet(&crate::refs::retarget_3d_endpoint(text, old, order), old, new),
                None => rewrite_sheet(text, old, new),
            }
        };
        if text == rewritten { return None; }
        if let Some(journal) = journal.as_mut() {
            journal.0.insert(field, (text.to_owned(), rewritten.clone()));
        }
        Some(rewritten)
    });
}

// Visit the same reference-bearing fields for ordinary sheet operations and their undo journals.
// An inverse rename must leave pre-existing unresolved references to the new name alone.
fn visit_references(wb: &mut Workbook, mut update: impl FnMut(ReferenceField, &str) -> Option<String>) {
    for (si, sheet) in wb.sheets.iter_mut().enumerate() {
        let changes: Vec<_> = sheet.iter().filter_map(|(key, cell)| {
            update(ReferenceField::Formula(si, key), cell.formula.as_deref()?).map(|text| (key, text))
        }).collect();
        for (key, text) in changes {
            sheet.get_mut(key.row, key.col).unwrap().formula = Some(text.into());
            sheet.edited = true;
        }
        for (bi, block) in sheet.cf.blocks.iter_mut().enumerate() {
            for (ri, rule) in block.rules.iter_mut().enumerate() {
                let mut apply = |number, text: &mut String| {
                    if let Some(new) = update(ReferenceField::Conditional(si, bi, ri, number), text) {
                        *text = new;
                        sheet.cf_dirty = true;
                    }
                };
                match &mut rule.kind {
                    crate::cf::CfKind::Expr { formula, .. } => apply(0, formula),
                    crate::cf::CfKind::CellIs { a, b, .. } => { apply(0, a); apply(1, b); }
                    crate::cf::CfKind::Scale { points } => {
                        for (k, (v, _)) in points.iter_mut().enumerate() {
                            if v.kind == crate::cf::CfvoKind::Formula {
                                apply(k, &mut v.formula);
                            }
                        }
                    }
                    crate::cf::CfKind::Bar { min, max, .. } => {
                        if min.kind == crate::cf::CfvoKind::Formula {
                            apply(0, &mut min.formula);
                        }
                        if max.kind == crate::cf::CfvoKind::Formula {
                            apply(1, &mut max.formula);
                        }
                    }
                    crate::cf::CfKind::IconSet { thresholds, .. } => {
                        for (k, v) in thresholds.iter_mut().enumerate() {
                            if v.kind == crate::cf::CfvoKind::Formula {
                                apply(k, &mut v.formula);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        for (vi, dv) in sheet.dv.iter_mut().enumerate() {
            for (fi, text) in [&mut dv.f1, &mut dv.f2].into_iter().enumerate() {
                if let Some(new) = update(ReferenceField::Validation(si, vi, fi), text) {
                    *text = new;
                    sheet.dv_dirty = true;
                }
            }
        }
        for (i, spark) in sheet.sparklines.iter_mut().enumerate() {
            if let Some(source) = spark.source_sheet.as_mut()
                && let Some(new) = update(ReferenceField::SparkSource(si, i), source)
            {
                *source = new;
                sheet.sparklines_dirty = true;
            }
        }
        for (ci, chart) in sheet.charts.iter_mut().enumerate() {
            for (series_idx, series) in chart.series.iter_mut().enumerate() {
                for (fi, text) in [
                    &mut series.name_ref,
                    &mut series.cat_ref,
                    &mut series.val_ref,
                    &mut series.size_ref,
                ]
                .into_iter()
                .enumerate()
                {
                    if let Some(new) = update(ReferenceField::Chart(si, ci, series_idx, fi), text) {
                        chart.ref_fixes.push((std::mem::replace(text, new.clone()), new));
                        sheet.charts_dirty = true;
                    }
                }
            }
            if !chart.source_range.is_empty()
                && let Some(new) =
                    update(ReferenceField::Chart(si, ci, usize::MAX, 0), &chart.source_range)
            {
                chart.source_range = new;
                sheet.charts_dirty = true;
            }
        }
    }
    for (i, (_, _, text)) in wb.names.iter_mut().enumerate() {
        if let Some(new) = update(ReferenceField::Name(i), text) {
            *text = new;
            wb.names_dirty = true;
        }
    }
    for (i, source) in wb.sources.iter_mut().enumerate() {
        if let crate::external::Kind::Internal { sheet, .. } = &mut source.kind {
            if let Some(new) = update(ReferenceField::SourceInput(i), sheet) { *sheet = new; }
        }
        if let Some(target) = source.target_sheet.as_mut() {
            if let Some(new) = update(ReferenceField::SourceTarget(i), target) { *target = new; }
        }
    }
    for (i, (sheet, ..)) in wb.pivot_shifts.iter_mut().enumerate() {
        if let Some(new) = update(ReferenceField::Pivot(i), sheet) { *sheet = new; }
    }
}
