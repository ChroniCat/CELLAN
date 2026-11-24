//! Retain the input guard and cells that the inverse axis operation cannot reconstruct.
use crate::{
    Cell, CellRef, Sheet, Value,
    refs::{Axis, Shift},
    store::{CellVersion, Store},
};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub(super) struct CellHistory {
    version: CellVersion,
    hash: [u8; 32],
    legacy: HashMap<CellRef, Cell>,
    exceptions: Vec<(CellRef, Cell)>,
    formula_values: FormulaValues,
}

/// Formulas can be recalculated between recording and undo (and the user can
/// switch to manual calculation before undo). Their cached result is part of the
/// snapshot even when the formula's text is perfectly invertible. Coordinates
/// are implicit in the restored sheet's row-major formula order.
#[derive(Clone, Debug, Default)]
struct FormulaValues {
    numbers: Vec<f64>,
    other: Vec<ValueRun>,
}

#[derive(Clone, Debug)]
struct ValueRun {
    first: usize,
    end: usize,
    value: Value,
}

impl FormulaValues {
    fn push(&mut self, position: usize, value: &Value) {
        if let Value::Number(n) = value {
            self.numbers.push(*n);
        } else if let Some(last) = self.other.last_mut()
            && last.end == position
            && last.value == *value
        {
            last.end += 1;
        } else {
            self.other.push(ValueRun {
                first: position,
                end: position + 1,
                value: value.clone(),
            });
        }
    }

    fn restore(&self, store: &mut Store) -> Option<()> {
        if self.numbers.is_empty() && self.other.is_empty() {
            return (!store.has_formulas()).then_some(());
        }
        let mut others = self.other.iter().peekable();
        let mut numbers = self.numbers.iter();
        let mut changes = Vec::new();
        let mut formula_idx = 0usize;
        for (pi, page) in store.iter_pages() {
            if !page.may_have_formula {
                continue;
            }
            let base = pi * crate::store::PAGE_ROWS;
            let mut mixed_cols: Vec<(u32, &crate::store::MixedSlab)> = Vec::new();
            let mut fmask = [0u64; 4];
            for (col, slab) in page.cols.iter_entries() {
                if let crate::store::ColSlab::Mixed(cs) = slab.as_ref()
                    && cs.has_formulas()
                {
                    let cm = cs.row_mask();
                    for w in 0..4 {
                        fmask[w] |= cm[w];
                    }
                    mixed_cols.push((col, cs));
                }
            }
            if mixed_cols.is_empty() {
                continue;
            }
            for w in 0..4usize {
                let mut bits = fmask[w];
                while bits != 0 {
                    let tz = bits.trailing_zeros() as usize;
                    bits &= bits - 1;
                    let ro = w * 64 + tz;
                    let row = base + ro as u32;
                    for &(col, cs) in &mixed_cols {
                        let Some(cell) = cs.get_cell(ro) else {
                            continue;
                        };
                        if cell.formula.is_none() {
                            continue;
                        }
                        let i = formula_idx;
                        formula_idx += 1;
                        let key = CellRef::new(row, col);
                        if let Some(run) = others.peek().filter(|r| r.first <= i) {
                            if cell.value != run.value {
                                changes.push((key, run.value.clone()));
                            }
                            if i + 1 == run.end {
                                others.next();
                            }
                        } else {
                            let n = *numbers.next()?;
                            if !matches!(cell.value, Value::Number(v) if v.to_bits() == n.to_bits()) {
                                changes.push((key, Value::Number(n)));
                            }
                        }
                    }
                }
            }
        }
        if numbers.next().is_some() || others.next().is_some() {
            return None;
        }
        for (k, value) in changes {
            store.set_formula_cache(k.row, k.col, value);
        }
        Some(())
    }

    fn bytes(&self) -> usize {
        self.numbers.capacity() * size_of::<f64>()
            + self.other.capacity() * size_of::<ValueRun>()
            + self
                .other
                .iter()
                .map(|run| match &run.value {
                    Value::Text(text) => text.len(),
                    _ => 0,
                })
                .sum::<usize>()
    }
}

fn map(k: CellRef, shift: &Shift) -> Option<CellRef> {
    Some(match shift.axis {
        Axis::Row => CellRef::new(shift.map(k.row)?, k.col),
        Axis::Col => CellRef::new(k.row, shift.map(k.col)?),
    })
}

fn on_sheet(shift: &Shift, sheet: &Sheet) -> Shift {
    Shift {
        target_sheet: sheet.name.clone(),
        own_sheet: sheet.name.clone(),
        ..shift.clone()
    }
}

fn same_mapped_view(
    source: crate::store::CellView<'_>,
    desired: crate::store::CellView<'_>,
    shift: &Shift,
) -> bool {
    source.same_stored_value(&desired)
        && match (source.formula(), desired.formula()) {
            (Some(a), Some(b)) => crate::refs::rewrite(a, shift) == b,
            (None, None) => true,
            _ => false,
        }
}

type SlabDigestCache = HashMap<usize, Option<[u8; 32]>>;

#[inline]
fn flush_hash_buf(hash: &mut Sha256, buf: &mut [u8; 4096], pos: &mut usize) {
    if *pos > 0 {
        hash.update(&buf[..*pos]);
        *pos = 0;
    }
}

#[inline]
fn push_hash_bytes(hash: &mut Sha256, buf: &mut [u8; 4096], pos: &mut usize, bytes: &[u8]) {
    if bytes.len() >= buf.len() {
        flush_hash_buf(hash, buf, pos);
        hash.update(bytes);
        return;
    }
    if *pos + bytes.len() > buf.len() {
        flush_hash_buf(hash, buf, pos);
    }
    buf[*pos..*pos + bytes.len()].copy_from_slice(bytes);
    *pos += bytes.len();
}

fn hash_slab_value(hash: &mut Sha256, buf: &mut [u8; 4096], pos: &mut usize, r_off: u16, value: &Value) {
    push_hash_bytes(hash, buf, pos, &r_off.to_le_bytes());
    match value {
        Value::Number(n) => {
            push_hash_bytes(hash, buf, pos, &[1]);
            push_hash_bytes(hash, buf, pos, &n.to_bits().to_le_bytes());
        }
        Value::Text(t) => {
            push_hash_bytes(hash, buf, pos, &[2]);
            push_hash_bytes(hash, buf, pos, &(t.len() as u64).to_le_bytes());
            push_hash_bytes(hash, buf, pos, t.as_bytes());
        }
        Value::Bool(b) => push_hash_bytes(hash, buf, pos, &[3, *b as u8]),
        Value::Error(e) => push_hash_bytes(hash, buf, pos, &[4, *e as u8]),
        Value::Empty => push_hash_bytes(hash, buf, pos, &[5]),
    }
}

fn hash_slab_view(
    hash: &mut Sha256,
    buf: &mut [u8; 4096],
    pos: &mut usize,
    r_off: u16,
    view: crate::store::CellView<'_>,
) {
    match view {
        crate::store::CellView::Number(n) => {
            push_hash_bytes(hash, buf, pos, &r_off.to_le_bytes());
            push_hash_bytes(hash, buf, pos, &[1]);
            push_hash_bytes(hash, buf, pos, &n.to_bits().to_le_bytes());
        }
        crate::store::CellView::Value(v) => hash_slab_value(hash, buf, pos, r_off, v),
        crate::store::CellView::Cell(cell) => {
            if let Some(f) = cell.formula.as_deref() {
                push_hash_bytes(hash, buf, pos, &r_off.to_le_bytes());
                push_hash_bytes(hash, buf, pos, &[0]);
                push_hash_bytes(hash, buf, pos, &(f.len() as u64).to_le_bytes());
                push_hash_bytes(hash, buf, pos, f.as_bytes());
            } else {
                hash_slab_value(hash, buf, pos, r_off, &cell.value);
            }
        }
    }
}

fn compute_slab_digest_unfiltered(slab: &crate::store::ColSlab) -> Option<[u8; 32]> {
    let mut hash = Sha256::new();
    let mut buf = [0u8; 4096];
    let mut pos = 0usize;
    let mut any = false;
    match slab {
        crate::store::ColSlab::Numbers(ns) => {
            for (r_off, &n) in ns.values.iter().enumerate() {
                if (ns.present[r_off / 64] & (1u64 << (r_off % 64))) != 0 {
                    any = true;
                    push_hash_bytes(&mut hash, &mut buf, &mut pos, &(r_off as u16).to_le_bytes());
                    push_hash_bytes(&mut hash, &mut buf, &mut pos, &[1]);
                    push_hash_bytes(&mut hash, &mut buf, &mut pos, &n.to_bits().to_le_bytes());
                }
            }
        }
        crate::store::ColSlab::Values(vs) => {
            vs.for_each_occupied(|r_off, v| {
                any = true;
                hash_slab_value(&mut hash, &mut buf, &mut pos, r_off as u16, v);
            });
        }
        crate::store::ColSlab::Mixed(cs) => {
            cs.for_each_occupied(|r_off, cell| {
                any = true;
                hash_slab_view(
                    &mut hash,
                    &mut buf,
                    &mut pos,
                    r_off as u16,
                    crate::store::CellView::Cell(cell),
                );
            });
        }
    }
    if !any {
        return None;
    }
    flush_hash_buf(&mut hash, &mut buf, &mut pos);
    Some(hash.finalize().into())
}

fn input_digest_with_cache(
    sheet: &Sheet,
    legacy: &HashMap<CellRef, Cell>,
    mut cache: Option<&mut SlabDigestCache>,
) -> [u8; 32] {
    let mut excluded_slabs = std::collections::HashSet::new();
    if !sheet.spilled.is_empty() || !legacy.is_empty() {
        for k in sheet.spilled.iter().chain(legacy.keys()) {
            excluded_slabs.insert((k.row / crate::store::PAGE_ROWS, k.col));
        }
    }
    let mut top = Sha256::new();
    let mut top_buf = [0u8; 4096];
    let mut top_pos = 0usize;
    for (pi, page) in sheet.cells().iter_pages() {
        let base = pi * crate::store::PAGE_ROWS;
        for (col, slab_arc) in page.cols.iter_entries() {
            let slab_digest = if excluded_slabs.contains(&(pi, col)) {
                let mut h = Sha256::new();
                let mut buf = [0u8; 4096];
                let mut pos = 0usize;
                let mut any = false;
                for r_off in 0..slab_arc.len() {
                    if let Some(view) = slab_arc.get_view(r_off as u32) {
                        let k = CellRef::new(base + r_off as u32, col);
                        if !sheet.spilled.contains(&k) && !legacy.contains_key(&k) {
                            any = true;
                            hash_slab_view(&mut h, &mut buf, &mut pos, r_off as u16, view);
                        }
                    }
                }
                if any {
                    flush_hash_buf(&mut h, &mut buf, &mut pos);
                    Some(h.finalize().into())
                } else {
                    None
                }
            } else if let Some(c) = cache.as_deref_mut() {
                let ptr = std::sync::Arc::as_ptr(slab_arc) as usize;
                *c.entry(ptr)
                    .or_insert_with(|| compute_slab_digest_unfiltered(slab_arc))
            } else {
                compute_slab_digest_unfiltered(slab_arc)
            };
            if let Some(digest) = slab_digest {
                push_hash_bytes(&mut top, &mut top_buf, &mut top_pos, &pi.to_le_bytes());
                push_hash_bytes(&mut top, &mut top_buf, &mut top_pos, &col.to_le_bytes());
                push_hash_bytes(&mut top, &mut top_buf, &mut top_pos, &digest);
            }
        }
    }
    flush_hash_buf(&mut top, &mut top_buf, &mut top_pos);
    top.finalize().into()
}

fn input_digest(sheet: &Sheet, legacy: &HashMap<CellRef, Cell>) -> [u8; 32] {
    input_digest_with_cache(sheet, legacy, None)
}

impl CellHistory {
    /// Record both directions together, sharing column-slab Merkle SHA-256 digests across
    /// unchanged/shifted `Arc<Vec<Cell>>` slabs and skipping untouched `Arc::ptr_eq` pages in `O(1)`.
    pub(super) fn pair(
        before: &Sheet,
        after: &Sheet,
        forward: &Shift,
        backward: &Shift,
    ) -> (Self, Self) {
        let mut slab_cache = SlabDigestCache::new();
        let (mut undo, redo_blanks) =
            Self::record_side(before, after, backward, forward, &mut slab_cache);
        let (mut redo, undo_blanks) =
            Self::record_side(after, before, forward, backward, &mut slab_cache);
        undo.exceptions
            .extend(undo_blanks.into_iter().map(|key| (key, Cell::default())));
        redo.exceptions
            .extend(redo_blanks.into_iter().map(|key| (key, Cell::default())));
        (undo, redo)
    }

    fn record_side(
        desired: &Sheet,
        other: &Sheet,
        toward_desired: &Shift,
        toward_other: &Shift,
        slab_cache: &mut SlabDigestCache,
    ) -> (Self, Vec<CellRef>) {
        let incoming = on_sheet(toward_desired, other);
        let inverse = Shift {
            delete: !incoming.delete,
            ..incoming.clone()
        };
        let mut legacy = HashMap::new();
        for &(anchor, rect) in &desired.arrays {
            if !desired.spill_shapes.contains_key(&anchor) {
                legacy.extend(
                    desired
                        .iter_rect_views(rect)
                        .filter(|(k, c)| {
                            *k != anchor && !c.has_formula() && !desired.spilled.contains(k)
                        })
                        .map(|(k, c)| (k, c.to_cell())),
                );
            }
        }
        let hash = input_digest_with_cache(desired, &legacy, Some(slab_cache));

        let mut spilled_pages = std::collections::HashSet::new();
        if !desired.spilled.is_empty() || !other.spilled.is_empty() {
            for k in desired.spilled.iter().chain(other.spilled.iter()) {
                spilled_pages.insert(k.row / crate::store::PAGE_ROWS);
            }
        }

        let rows_axis = incoming.axis == Axis::Row
            && toward_other.axis == Axis::Row
            && incoming.translate.is_none()
            && toward_other.translate.is_none()
            && incoming.col_filter.is_none()
            && toward_other.col_filter.is_none();
        let unchanged_row_bound = if rows_axis {
            incoming.at.min(toward_other.at)
        } else {
            0
        };

        let mut exceptions = Vec::new();
        let mut clear_other = Vec::new();
        let mut formula_values = FormulaValues::default();
        let mut formula_index = 0usize;

        for (pi, page) in desired.cells().iter_pages() {
            let base = pi * crate::store::PAGE_ROWS;
            // Fast O(1) skip for untouched prefix pages that share the exact `Arc<Page>` with `other`!
            if base + crate::store::PAGE_ROWS <= unchanged_row_bound
                && desired.cells().same_page(other.cells(), pi)
                && !spilled_pages.contains(&pi)
            {
                if !page.may_have_formula {
                    continue;
                }
                for w in 0..4usize {
                    let mut bits = page.row_mask[w];
                    while bits != 0 {
                        let tz = bits.trailing_zeros();
                        bits &= bits - 1;
                        let ro = w as u32 * 64 + tz;
                        for (col, slab) in page.cols.iter_entries() {
                            let crate::store::ColSlab::Mixed(cs) = slab.as_ref() else {
                                continue;
                            };
                            let Some(cell) = cs.get_cell(ro as usize) else {
                                continue;
                            };
                            if let Some(f_desired) = cell.formula.as_deref() {
                                formula_values.push(formula_index, &cell.value);
                                formula_index += 1;
                                // In a shared page, `prior == cell`; formula is invertible iff rewrite(f, incoming) == f.
                                if crate::refs::rewrite(f_desired, &incoming) != f_desired {
                                    exceptions.push((CellRef::new(base + ro, col), cell.clone()));
                                }
                            }
                        }
                    }
                }
                continue;
            }

            let other_cells = other.cells();
            let mut cached_other_page: Option<(u32, Option<&std::sync::Arc<crate::store::Page>>)> =
                None;
            let mut get_other = |r: u32, c: u32| -> Option<crate::store::CellView<'_>> {
                let opi = r / crate::store::PAGE_ROWS;
                let oro = r % crate::store::PAGE_ROWS;
                let opage = match cached_other_page {
                    Some((p, page_ref)) if p == opi => page_ref,
                    _ => {
                        let p_ref = other_cells.page(opi);
                        cached_other_page = Some((opi, p_ref));
                        p_ref
                    }
                }?;
                opage.get_view(oro, c)
            };

            for w in 0..4usize {
                let mut bits = page.row_mask[w];
                while bits != 0 {
                    let tz = bits.trailing_zeros();
                    bits &= bits - 1;
                    let ro = w as u32 * 64 + tz;
                    let row = base + ro;
                    for (col, slab) in page.cols.iter_entries() {
                        let Some(cell) = slab.get_view(ro) else {
                            continue;
                        };
                        let key = CellRef::new(row, col);
                        let spill =
                            !desired.spilled.is_empty() && desired.spilled.contains(&key);
                        if cell.has_formula() {
                            formula_values.push(formula_index, cell.value().as_ref());
                            formula_index += 1;
                        }
                        let origin = map(key, &inverse);
                        let prior = origin.and_then(|pos| get_other(pos.row, pos.col));
                        let invertible = origin
                            .is_some_and(|pos| other.spilled.is_empty() || !other.spilled.contains(&pos))
                            && prior.is_some_and(|c| same_mapped_view(c, cell, &incoming));
                        if spill || !invertible {
                            exceptions.push((key, cell.to_cell()));
                        }
                        if !spill && let Some(dest) = map(key, toward_other) {
                            let absent = if Some(dest) == origin {
                                prior.is_none()
                            } else {
                                get_other(dest.row, dest.col).is_none()
                            };
                            if absent {
                                clear_other.push(dest);
                            }
                        }
                    }
                }
            }
        }
        (
            Self {
                version: desired.cell_version().clone(),
                hash,
                legacy,
                exceptions,
                formula_values,
            },
            clear_other,
        )
    }

    pub(super) fn matches(&self, live: &Sheet, expected: &Sheet) -> bool {
        if *live.cell_version() == self.version && live.spilled == expected.spilled {
            return true;
        }
        input_digest(live, &self.legacy) == self.hash
            && self.legacy.iter().all(|(k, old)| {
                live.spilled.contains(k)
                    || super::same_input(live.get(k.row, k.col).as_deref(), Some(old))
            })
    }

    pub(super) fn restore(&self, current: &Sheet, shift: &Shift) -> Option<Store> {
        let shift = on_sheet(shift, current);
        let mut store = current.cells().clone();
        for k in &current.spilled {
            store.remove(k.row, k.col);
        }
        store.remap_axis(
            shift.axis == Axis::Row,
            shift.at,
            |v| shift.map(v),
            &mut Default::default(),
        );
        store.rewrite_formulas_parallel(None, |text| {
            let restored = crate::refs::rewrite(text, &shift);
            (restored != text).then(|| restored.into())
        });
        for (k, cell) in &self.exceptions {
            if cell.is_blank() {
                store.remove(k.row, k.col);
            } else {
                store.set(k.row, k.col, cell.clone());
            }
        }
        self.formula_values.restore(&mut store)?;
        Some(store)
    }

    pub(super) fn bytes(&self) -> usize {
        (self.exceptions.capacity() + self.legacy.capacity()) * size_of::<(CellRef, Cell)>()
            + self
                .exceptions
                .iter()
                .map(|(_, c)| super::cell_heap_bytes(c))
                .sum::<usize>()
            + self
                .legacy
                .values()
                .map(super::cell_heap_bytes)
                .sum::<usize>()
            + self.formula_values.bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Workbook,
        formula::{self, FValue},
        structural::{Structural, apply_structural_wb},
    };

    #[test]
    fn paired_history_restores_exceptions_and_holes_with_different_shift_boundaries() {
        let mut before = Sheet::new("Budget");
        for (r, c, n) in [(0, 0, 1.), (1, 1, 2.), (3, 0, 8.), (8, 2, 16.)] {
            before.set(
                r,
                c,
                Cell {
                    value: Value::Number(n),
                    formula: None,
                },
            );
        }
        let mut after = before.clone();
        after.insert_rows(2, 1);
        after.remove(0, 0);
        after.set(
            3,
            4,
            Cell {
                value: Value::Text("new".into()),
                formula: None,
            },
        );
        let forward = Structural::InsertRows { at: 2, n: 1 }.reference_shift();
        let reverse = Structural::DeleteRows { at: 1, n: 1 }.reference_shift();
        let (undo, redo) = CellHistory::pair(&before, &after, &forward, &reverse);
        let restored = undo.restore(&after, &reverse).unwrap();
        assert!(restored.iter().eq(before.iter()));
        let reapplied = redo.restore(&before, &forward).unwrap();
        assert!(reapplied.iter().eq(after.iter()));
        let mut changed = after.clone();
        changed.set(
            20,
            1,
            Cell {
                value: Value::Bool(true),
                formula: None,
            },
        );
        assert!(!redo.matches(&changed, &after));
        changed.remove(20, 1);
        assert!(redo.matches(&changed, &after));
    }

    #[test]
    fn inverse_axis_restores_cached_formula_state_after_later_calculation() {
        let mut wb = Workbook::new_blank();
        for (r, n) in [(0, 3.), (1, 20.), (2, 5.)] {
            wb.sheet_mut(0).set(
                r,
                0,
                Cell {
                    value: Value::Number(n),
                    formula: None,
                },
            );
        }
        formula::set_formula(wb.sheet_mut(0), 0, 1, "=INDIRECT(\"A2\")", FValue::Empty);
        formula::set_formula(wb.sheet_mut(0), 0, 2, "=B1*2", FValue::Empty);
        formula::recalc_all_force(&mut wb);
        let before = wb.sheet(0).clone();
        let op = Structural::DeleteRows { at: 1, n: 1 };
        let snapshot = crate::undo::snapshot_structure(&wb, 0, &op);
        apply_structural_wb(&mut wb, 0, &op);
        let staged = snapshot.capture_after(&wb);
        let mut stack = crate::undo::UndoStack::new();
        stack.push_structure("delete", snapshot, staged, (0, 0, 0));
        let reverse = Structural::InsertRows { at: 1, n: 1 }.reference_shift();
        let (history, guard) =
            CellHistory::pair(&before, wb.sheet(0), &op.reference_shift(), &reverse);
        let after = wb.sheet(0).clone();
        formula::recalc_all_force(&mut wb);
        assert_eq!(wb.sheet(0).value(0, 2), Value::Number(10.));
        assert!(guard.matches(wb.sheet(0), &after));
        let restored = history.restore(wb.sheet(0), &reverse).unwrap();
        assert_eq!(restored.get(0, 2), before.get(0, 2));
        assert_eq!(restored.get(0, 1), before.get(0, 1));
        let mut manual = wb.clone();
        manual.calc_manual = true;
        stack.undo_workbook(&mut manual, |_, _, _, _| true).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("manual-undo.xlsx");
        crate::xlsx::save_atomic(&path, &manual).unwrap();
        let reopened = crate::xlsx::load(&path).unwrap();
        assert_eq!(reopened.sheet(0).value(0, 2), Value::Number(40.));

        wb.sheet_mut(0).set(
            0,
            0,
            Cell {
                value: Value::Number(777.),
                formula: None,
            },
        );
        assert!(!guard.matches(wb.sheet(0), &after));
    }

    #[test]
    fn compact_formula_cache_stream_handles_mixed_types_and_rejects_mismatched_counts() {
        let mut snapshot = Store::default();
        let values = [
            Value::Bool(true),
            Value::Bool(true),
            Value::Number(-0.),
            Value::Text("note".into()),
            Value::Error(crate::formula::ErrKind::Ref),
            Value::Number(0.),
            Value::Empty,
            Value::Empty,
        ];
        let mut history = FormulaValues::default();
        for (i, v) in values.iter().enumerate() {
            history.push(i, v);
            snapshot.set(
                i as u32,
                2,
                Cell {
                    formula: Some("=1".into()),
                    value: Value::Number(45.),
                },
            );
        }
        history.restore(&mut snapshot).unwrap();
        for (i, v) in values.iter().enumerate() {
            let actual = &snapshot.get(i as u32, 2).unwrap().value;
            match (actual, v) {
                (Value::Number(a), Value::Number(b)) => assert_eq!(a.to_bits(), b.to_bits()),
                _ => assert_eq!(actual, v),
            }
        }
        snapshot.remove(0, 2);
        assert!(history.restore(&mut snapshot).is_none());
    }
}
