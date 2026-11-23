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
    wb.sheets[idx].name = name.to_owned();
    rewrite_qualifiers(wb, &old, Some(name), None, journal);
    wb.dirty = true;
    true
}

/// Delete a sheet, preserving at least one sheet. Deleted-sheet references
/// become explicit errors, so later reuse of its name cannot retarget them.
pub fn delete_sheet(wb: &mut Workbook, idx: usize) -> bool {
    if wb.sheets.len() <= 1 || idx >= wb.sheets.len() { return false; }
    // 删除前保留完整标签顺序：三维区间的邻居必须按原区间求，
    // 不能用全局 prev/next（反向区间/唯一成员会取错）。
    let order: Vec<String> = wb.sheets.iter().map(|s| s.name.clone()).collect();
    let old = wb.sheets.remove(idx).name;
    let before = wb.names.len();
    wb.names.retain(|(_, local, _)| *local != Some(idx));
    wb.names_dirty |= wb.names.len() != before;
    for (_, local, _) in &mut wb.names {
        if let Some(i) = local.as_mut() {
            if *i > idx { *i -= 1; wb.names_dirty = true; }
        }
    }
    rewrite_qualifiers(wb, &old, None, Some(&order), None);
    wb.pivot_shifts.retain(|(sheet, ..)| !sheet.eq_ignore_ascii_case(&old));
    wb.pivots_pending.retain(|(sheet, ..)| *sheet != idx);
    for (sheet, ..) in &mut wb.pivots_pending {
        if *sheet > idx { *sheet -= 1; }
    }
    wb.pivots_pending2.retain(|(sheet, ..)| *sheet != idx);
    for (sheet, ..) in &mut wb.pivots_pending2 {
        if *sheet > idx { *sheet -= 1; }
    }
    wb.dirty = true;
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
    Source(usize),
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
            ReferenceField::Source(_) | ReferenceField::Pivot(_) if text.eq_ignore_ascii_case(old) =>
                new.unwrap_or("#REF!").to_owned(),
            ReferenceField::Source(_) | ReferenceField::Pivot(_) => return None,
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
        for (ci, chart) in sheet.charts.iter_mut().enumerate() {
            for (series_idx, series) in chart.series.iter_mut().enumerate() {
                for (fi, text) in [&mut series.name_ref, &mut series.cat_ref, &mut series.val_ref].into_iter().enumerate() {
                    if let Some(new) = update(ReferenceField::Chart(si, ci, series_idx, fi), text) {
                        chart.ref_fixes.push((std::mem::replace(text, new.clone()), new));
                        sheet.charts_dirty = true;
                    }
                }
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
            if let Some(new) = update(ReferenceField::Source(i), sheet) { *sheet = new; }
        }
    }
    for (i, (sheet, ..)) in wb.pivot_shifts.iter_mut().enumerate() {
        if let Some(new) = update(ReferenceField::Pivot(i), sheet) { *sheet = new; }
    }
}
