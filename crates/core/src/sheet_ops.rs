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
    wb.sheets.insert(idx, sheet);
    wb.dirty = true;
    true
}

/// Rename an existing sheet. Invalid/duplicate names and unchanged names return
/// false without modifying the workbook.
pub fn rename_sheet(wb: &mut Workbook, idx: usize, name: &str) -> bool {
    let name = name.trim();
    if idx >= wb.sheets.len() || !valid_name(wb, Some(idx), name)
        || wb.sheets[idx].name == name
    {
        return false;
    }
    let old = wb.sheets[idx].name.clone();
    wb.sheets[idx].name = name.to_owned();
    rewrite_qualifiers(wb, &old, Some(name));
    for (sheet, ..) in &mut wb.pivot_shifts {
        if sheet.eq_ignore_ascii_case(&old) { *sheet = name.to_owned(); }
    }
    wb.dirty = true;
    true
}

/// Delete a sheet, preserving at least one sheet. Deleted-sheet references
/// become explicit errors, so later reuse of its name cannot retarget them.
pub fn delete_sheet(wb: &mut Workbook, idx: usize) -> bool {
    if wb.sheets.len() <= 1 || idx >= wb.sheets.len() { return false; }
    let old = wb.sheets.remove(idx).name;
    let before = wb.names.len();
    wb.names.retain(|(_, local, _)| *local != Some(idx));
    wb.names_dirty |= wb.names.len() != before;
    for (_, local, _) in &mut wb.names {
        if let Some(i) = local.as_mut() {
            if *i > idx { *i -= 1; wb.names_dirty = true; }
        }
    }
    rewrite_qualifiers(wb, &old, None);
    wb.pivot_shifts.retain(|(sheet, ..)| !sheet.eq_ignore_ascii_case(&old));
    wb.pivots_pending.retain(|(sheet, ..)| *sheet != idx);
    for (sheet, ..) in &mut wb.pivots_pending {
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
    wb.dirty = true;
    true
}

fn rewrite_text(text: &mut String, old: &str, new: Option<&str>) -> bool {
    let rewritten = rewrite_sheet(text, old, new);
    let changed = *text != rewritten;
    *text = rewritten;
    changed
}

fn rewrite_qualifiers(wb: &mut Workbook, old: &str, new: Option<&str>) {
    for sheet in &mut wb.sheets {
        let cells: Vec<_> = sheet.iter().filter(|(_, c)| c.formula.is_some()).map(|(k, _)| k).collect();
        for key in cells {
            let cell = sheet.get_mut(key.row, key.col).unwrap();
            let formula = cell.formula.as_mut().unwrap();
            let rewritten = rewrite_sheet(formula, old, new).into_boxed_str();
            let changed = *formula != rewritten;
            *formula = rewritten;
            sheet.edited |= changed;
        }
        for block in &mut sheet.cf.blocks {
            for rule in &mut block.rules {
                match &mut rule.kind {
                    crate::cf::CfKind::Expr { formula, .. } => {
                        sheet.cf_dirty |= rewrite_text(formula, old, new);
                    }
                    crate::cf::CfKind::CellIs { a, b, .. } => {
                        sheet.cf_dirty |= rewrite_text(a, old, new);
                        sheet.cf_dirty |= rewrite_text(b, old, new);
                    }
                    _ => {}
                }
            }
        }
        for dv in &mut sheet.dv {
            sheet.dv_dirty |= rewrite_text(&mut dv.f1, old, new);
            sheet.dv_dirty |= rewrite_text(&mut dv.f2, old, new);
        }
        for chart in &mut sheet.charts {
            for series in &mut chart.series {
                for text in [&mut series.name_ref, &mut series.cat_ref, &mut series.val_ref] {
                    let rewritten = rewrite_sheet(text, old, new);
                    if *text != rewritten {
                        chart.ref_fixes.push((text.clone(), rewritten.clone()));
                        *text = rewritten;
                        sheet.charts_dirty = true;
                    }
                }
            }
        }
    }
    for (_, _, text) in &mut wb.names {
        wb.names_dirty |= rewrite_text(text, old, new);
    }
    for source in &mut wb.sources {
        if let crate::external::Kind::Internal { sheet, .. } = &mut source.kind {
            if sheet.eq_ignore_ascii_case(old) {
                // Invalid as a sheet name: a later new sheet cannot revive the
                // deleted source and silently refresh from unrelated data.
                *sheet = new.unwrap_or("#REF!").to_owned();
            }
        }
    }
}
