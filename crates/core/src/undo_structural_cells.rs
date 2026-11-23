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
        let mut others = self.other.iter().peekable();
        let mut numbers = self.numbers.iter();
        let mut changes = Vec::new();
        for (i, (key, cell)) in store
            .iter()
            .filter(|(_, c)| c.formula.is_some())
            .enumerate()
        {
            if let Some(run) = others.peek().filter(|r| r.first <= i) {
                if cell.value != run.value {
                    changes.push((key, run.value.clone()));
                }
                if i + 1 == run.end {
                    others.next();
                }
            } else {
                let n = *numbers.next()?;
                if !matches!(cell.value,Value::Number(v) if v.to_bits() == n.to_bits()) {
                    changes.push((key, Value::Number(n)));
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

fn same_mapped(source: &Cell, desired: &Cell, shift: &Shift) -> bool {
    let equal = match (&source.value, &desired.value) {
        (Value::Number(a), Value::Number(b)) => a.to_bits() == b.to_bits(),
        (a, b) => a == b,
    };
    equal
        && match (source.formula.as_deref(), desired.formula.as_deref()) {
            (Some(a), Some(b)) => crate::refs::rewrite(a, shift) == b,
            (None, None) => true,
            _ => false,
        }
}

fn input_digest(sheet: &Sheet, legacy: &HashMap<CellRef, Cell>) -> [u8; 32] {
    let mut hash = Sha256::new();
    // Row-major traversal and tagged, length-prefixed fields make the digest
    // independent of storage layout and unambiguous for arbitrary user text.
    for (k, cell) in sheet.iter() {
        if sheet.spilled.contains(&k) || legacy.contains_key(&k) {
            continue;
        }
        hash.update(k.row.to_le_bytes());
        hash.update(k.col.to_le_bytes());
        if let Some(f) = cell.formula.as_deref() {
            hash.update([0]);
            hash.update((f.len() as u64).to_le_bytes());
            hash.update(f.as_bytes());
        } else {
            match &cell.value {
                Value::Number(n) => {
                    hash.update([1]);
                    hash.update(n.to_bits().to_le_bytes());
                }
                Value::Text(t) => {
                    hash.update([2]);
                    hash.update((t.len() as u64).to_le_bytes());
                    hash.update(t.as_bytes());
                }
                Value::Bool(b) => hash.update([3, *b as u8]),
                Value::Error(e) => hash.update([4, *e as u8]),
                Value::Empty => hash.update([5]),
            }
        }
    }
    hash.finalize().into()
}

impl CellHistory {
    pub(super) fn between(source: &Sheet, desired: &Sheet, shift: &Shift) -> Self {
        let forward = on_sheet(shift, source);
        let reverse = Shift {
            delete: !shift.delete,
            ..forward.clone()
        };
        let mut exceptions = Vec::new();
        let mut formula_values = FormulaValues::default();
        let mut formula_index = 0;
        // A deleted row has no inverse source. A formula that was rewritten to
        // #REF! cannot recover its text. Record these, including formula caches.
        for (key, cell) in desired.iter() {
            if cell.formula.is_some() {
                formula_values.push(formula_index, &cell.value);
                formula_index += 1;
            }
            let prior = map(key, &reverse)
                .filter(|k| !source.spilled.contains(k))
                .and_then(|k| source.get(k.row, k.col));
            if desired.spilled.contains(&key)
                || !prior.is_some_and(|v| same_mapped(v, cell, &forward))
            {
                exceptions.push((key, cell.clone()));
            }
        }
        for (key, _) in source.iter() {
            if !source.spilled.contains(&key)
                && let Some(dest) = map(key, &forward)
                && !desired.contains(dest.row, dest.col)
            {
                exceptions.push((dest, Cell::default()));
            }
        }
        let mut legacy = HashMap::new();
        for &(anchor, rect) in &desired.arrays {
            if !desired.spill_shapes.contains_key(&anchor) {
                legacy.extend(
                    desired
                        .iter_rect(rect)
                        .filter(|(k, c)| {
                            *k != anchor && c.formula.is_none() && !desired.spilled.contains(k)
                        })
                        .map(|(k, c)| (k, c.clone())),
                );
            }
        }
        Self {
            version: desired.cell_version().clone(),
            hash: input_digest(desired, &legacy),
            legacy,
            exceptions,
            formula_values,
        }
    }

    pub(super) fn matches(&self, live: &Sheet, expected: &Sheet) -> bool {
        if *live.cell_version() == self.version && live.spilled == expected.spilled {
            return true;
        }
        input_digest(live, &self.legacy) == self.hash
            && self.legacy.iter().all(|(k, old)| {
                live.spilled.contains(k) || super::same_input(live.get(k.row, k.col), Some(old))
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
        let changes: Vec<_> = store
            .formula_cells()
            .filter_map(|(k, cell)| {
                let text = cell.formula.as_deref()?;
                let restored = crate::refs::rewrite(text, &shift);
                (restored != text).then(|| (k, restored.into()))
            })
            .collect();
        for (k, f) in changes {
            store.get_mut(k.row, k.col).unwrap().formula = Some(f);
        }
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
        let history = CellHistory::between(wb.sheet(0), &before, &reverse);
        let guard = CellHistory::between(&before, wb.sheet(0), &op.reference_shift());
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
