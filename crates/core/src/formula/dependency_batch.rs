//! Evaluate broad dependency levels using the ordinary formula evaluator. A cell leaves
//! the serial queue only after every observed formula input has a scalar from this pass.
use super::{
    Calc, FValue, Workbook, compiled,
    deps::{self, CellKey, Rect},
    scalar_batch::Batch,
};
use crate::Value;
use std::{
    collections::{BTreeMap, HashSet},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

const MIN_FORMULAS: usize = 2_048;
const MIN_PER_WORKER: usize = 128;
const MAX_INPUTS: usize = 2_048;
const MAX_RANGE: i64 = 32_768;

struct FormulaIndex<'a> {
    keys: &'a [CellKey],
    columns: Option<BTreeMap<(usize, u32), Vec<(u32, usize)>>>,
}

impl FormulaIndex<'_> {
    fn point(&self, key: CellKey, out: &mut Vec<usize>) {
        if let Ok(slot) = self.keys.binary_search(&key) {
            out.push(slot);
        }
    }

    fn collect(&mut self, r: Rect, out: &mut Vec<usize>) -> bool {
        if r.r1 == r.r2 && r.c1 == r.c2 {
            if let (Ok(row), Ok(col)) = (u32::try_from(r.r1), u32::try_from(r.c1)) {
                self.point((r.sheet, row, col), out);
            }
            return out.len() <= MAX_INPUTS;
        }
        // Wide shared ranges are left to the serial Calc, which reuses aggregation indexes.
        if (r.r2 - r.r1 + 1).saturating_mul(r.c2 - r.c1 + 1) > MAX_RANGE {
            return false;
        }
        let (Ok(top), Ok(bottom), Ok(left), Ok(right)) = (
            u32::try_from(r.r1),
            u32::try_from(r.r2),
            u32::try_from(r.c1),
            u32::try_from(r.c2),
        ) else {
            return false;
        };
        let columns = self.columns.get_or_insert_with(|| {
            let mut columns = BTreeMap::<_, Vec<_>>::new();
            for (slot, &(sheet, row, col)) in self.keys.iter().enumerate() {
                columns.entry((sheet, col)).or_default().push((row, slot));
            }
            columns
        });
        for (_, rows) in columns.range((r.sheet, left)..=(r.sheet, right)) {
            let start = rows.partition_point(|(r, _)| *r < top);
            let end = rows.partition_point(|(r, _)| *r <= bottom);
            if out.len() + end - start > MAX_INPUTS {
                return false;
            }
            out.extend(rows[start..end].iter().map(|(_, slot)| *slot));
        }
        true
    }
}

fn uses_definition(node: &super::Node, names: &HashSet<String>) -> bool {
    use super::Node;
    match node {
        Node::Name(_) | Node::ScopedName { .. } => true,
        Node::Func(name, args) => {
            names.contains(name) || args.iter().any(|v| uses_definition(v, names))
        }
        Node::Implicit(v) | Node::Percent(v) | Node::Unary(_, v) => uses_definition(v, names),
        Node::Binary(_, a, b) => uses_definition(a, names) || uses_definition(b, names),
        _ => false,
    }
}

fn prerequisites(
    wb: &Workbook,
    key: CellKey,
    index: &mut FormulaIndex<'_>,
    names: &HashSet<String>,
    slots: &mut Vec<usize>,
) -> bool {
    let (si, row, col) = key;
    let Some(formula) = wb
        .sheet(si)
        .get(row, col)
        .and_then(|c| c.formula.as_deref())
    else {
        return false;
    };
    let src = formula.strip_prefix('=').unwrap_or(formula);
    if !wb.links.is_empty() && src.contains('[') {
        return false;
    }
    if let Some(simple) = compiled::scalar_at(src, row, col) {
        simple.refs(|r, c| index.point((si, r, c), slots));
    } else {
        // Named values and lambdas may resolve relative to the caller's lexical bindings.
        if !names.is_empty()
            && compiled::with_node(src, row, col, |node| uses_definition(node, names))
                .unwrap_or(true)
        {
            return false;
        }
        let Some(areas) = deps::parallel_inputs(wb, key, src) else {
            return false;
        };
        for area in areas {
            if !index.collect(area, slots) {
                return false;
            }
        }
    }
    slots.sort_unstable();
    slots.dedup();
    true
}

/// Two flat buffers keep the graph proportional to actual formula edges without
/// allocating a separate adjacency list for every cell of a filled column.
struct Edges {
    offsets: Vec<usize>,
    slots: Vec<usize>,
}
impl Edges {
    fn get(&self, slot: usize) -> &[usize] {
        &self.slots[self.offsets[slot]..self.offsets[slot + 1]]
    }
}

struct Plan {
    inputs: Edges,
    consumers: Edges,
    waiting: Vec<usize>,
    ready: Vec<usize>,
}

impl Plan {
    fn build(
        wb: &Workbook,
        keys: &[CellKey],
        values: &[Option<Value>],
        cancel: Option<&AtomicBool>,
    ) -> Option<Self> {
        let mut index = FormulaIndex {
            keys,
            columns: None,
        };
        let names = wb
            .names
            .iter()
            .map(|(name, _, _)| name.to_ascii_uppercase())
            .collect();
        let mut inputs = Edges {
            offsets: Vec::with_capacity(keys.len() + 1),
            slots: Vec::new(),
        };
        let mut counts = vec![0; keys.len() + 1];
        let mut waiting = vec![0; keys.len()];
        let mut ready = Vec::new();
        let edge_limit = (keys.len().saturating_mul(16)).clamp(8192, 1_048_576);
        let mut scratch = Vec::new();
        for (slot, &key) in keys.iter().enumerate() {
            if slot % 128 == 0 && cancelled(cancel) {
                return None;
            }
            inputs.offsets.push(inputs.slots.len());
            scratch.clear();
            if values[slot].is_some() || !prerequisites(wb, key, &mut index, &names, &mut scratch) {
                continue;
            }
            if inputs.slots.len() + scratch.len() > edge_limit {
                return None;
            }
            for &input in &scratch {
                if values[input].is_none() {
                    counts[input + 1] += 1;
                    waiting[slot] += 1;
                }
            }
            if waiting[slot] == 0 {
                ready.push(slot);
            }
            inputs.slots.extend_from_slice(&scratch);
        }
        inputs.offsets.push(inputs.slots.len());
        for i in 1..counts.len() {
            counts[i] += counts[i - 1];
        }
        let mut write = counts[..keys.len()].to_vec();
        let mut consumers = Edges {
            slots: vec![0; counts[keys.len()]],
            offsets: counts,
        };
        for slot in 0..keys.len() {
            if slot % 128 == 0 && cancelled(cancel) {
                return None;
            }
            for &input in inputs.get(slot) {
                if values[input].is_none() {
                    consumers.slots[write[input]] = slot;
                    write[input] += 1;
                }
            }
        }
        Some(Self {
            inputs,
            consumers,
            waiting,
            ready,
        })
    }
}

fn cancelled(cancel: Option<&AtomicBool>) -> bool {
    cancel.is_some_and(|flag| flag.load(Ordering::Relaxed))
}

fn run_chunk<'a>(
    wb: &'a Workbook,
    keys: &[CellKey],
    chunk: &[usize],
    inputs: &Edges,
    values: &[Option<Value>],
    scope: Option<&'a HashSet<CellKey>>,
    cancel: Option<&AtomicBool>,
) -> Vec<(usize, Value)> {
    let mut calc = Calc::new(wb);
    calc.scheduling = true;
    calc.scope = scope;
    let mut outputs = Vec::new();
    for &slot in chunk {
        if cancelled(cancel) {
            break;
        }
        for &input in inputs.get(slot) {
            calc.memo
                .entry(keys[input])
                .or_insert_with(|| super::to_fvalue(values[input].as_ref().expect("ready input")));
        }
        let (si, r, c) = keys[slot];
        let v = calc.cell_value_inner(si, r, c, true);
        if calc.requested.is_some() || matches!(v, FValue::Array(_) | FValue::Lambda(_)) {
            // A formula may resolve an additional address at runtime. Discard its
            // partial caches and leave it and its consumers in the serial queue.
            drop(calc);
            calc = Calc::new(wb);
            calc.scheduling = true;
            calc.scope = scope;
        } else {
            outputs.push((slot, super::from_fvalue(&v)));
        }
    }
    outputs
}

pub(super) fn evaluate(
    wb: &Workbook,
    keys: &[CellKey],
    initial: Option<Batch>,
    cancel: Option<&AtomicBool>,
    done: Option<&AtomicUsize>,
) -> Option<Batch> {
    let workers = std::thread::available_parallelism()
        .map_or(1, usize::from)
        .min(8);
    evaluate_with_workers(wb, keys, initial, None, cancel, done, workers)
}

pub(super) fn dirty(
    wb: &Workbook,
    keys: &[CellKey],
    scope: &HashSet<CellKey>,
    cancel: Option<&AtomicBool>,
) -> Option<Batch> {
    let workers = std::thread::available_parallelism()
        .map_or(1, usize::from)
        .min(8);
    evaluate_with_workers(wb, keys, None, Some(scope), cancel, None, workers)
}

fn evaluate_with_workers(
    wb: &Workbook,
    keys: &[CellKey],
    initial: Option<Batch>,
    scope: Option<&HashSet<CellKey>>,
    cancel: Option<&AtomicBool>,
    done: Option<&AtomicUsize>,
    workers: usize,
) -> Option<Batch> {
    let pending = initial.as_ref().map_or(keys.len(), |b| b.pending.len());
    if pending < MIN_FORMULAS || workers < 2 || cancelled(cancel) {
        return initial;
    }
    let mut values: Vec<Option<Value>> = vec![None; keys.len()];
    if let Some(batch) = &initial {
        let mut unresolved = batch.pending.iter().copied().peekable();
        for (i, (_, _, _, value)) in batch.values.iter().enumerate() {
            if unresolved.next_if_eq(&i).is_none() {
                values[i] = Some(super::from_fvalue(value));
            }
        }
    }
    let Some(mut plan) = Plan::build(wb, keys, &values, cancel) else {
        return initial;
    };
    let mut completed = keys.len() - pending;
    let mut narrow_levels = 0;
    while !plan.ready.is_empty() && !cancelled(cancel) {
        let ready = std::mem::take(&mut plan.ready);
        let threads = (ready.len() / MIN_PER_WORKER).clamp(1, workers);
        if threads == 1 {
            narrow_levels += 1;
            // One long serial chain should keep the ordinary stack evaluator.
            if narrow_levels > 8 {
                break;
            }
        } else {
            narrow_levels = 0;
        }
        let results = if threads == 1 {
            vec![run_chunk(
                wb,
                keys,
                &ready,
                &plan.inputs,
                &values,
                scope,
                cancel,
            )]
        } else {
            std::thread::scope(|pool| {
                let jobs: Vec<_> = ready
                    .chunks(ready.len().div_ceil(threads))
                    .map(|chunk| {
                        pool.spawn(|| {
                            run_chunk(wb, keys, chunk, &plan.inputs, &values, scope, cancel)
                        })
                    })
                    .collect();
                jobs.into_iter()
                    .map(|job| job.join().expect("dependency level worker"))
                    .collect()
            })
        };
        for (slot, value) in results.into_iter().flatten() {
            completed += 1;
            values[slot] = Some(value);
            for &consumer in plan.consumers.get(slot) {
                plan.waiting[consumer] -= 1;
                if plan.waiting[consumer] == 0 {
                    plan.ready.push(consumer);
                }
            }
        }
        if let Some(done) = done {
            done.store(completed, Ordering::Relaxed);
        }
    }
    if completed == keys.len() - pending || cancelled(cancel) {
        return initial;
    }
    let mut pending = Vec::new();
    let values = keys
        .iter()
        .zip(values)
        .enumerate()
        .map(|(slot, (&(si, row, col), value))| {
            let v = value.map_or_else(
                || {
                    pending.push(slot);
                    FValue::Empty
                },
                |v| super::to_fvalue(&v),
            );
            (si, row, col, v)
        })
        .collect();
    Some(Batch { values, pending })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formula::{deps::Deps, recalc_all_force, recalc_dirty, set_formula};
    use crate::{Cell, Sheet};

    fn fixture() -> Workbook {
        let mut wb = Workbook::new_blank();
        wb.sheets.push(Sheet::new("Report"));
        for row in 0..760 {
            let n = row + 1;
            wb.sheet_mut(0).set(
                row,
                0,
                Cell {
                    value: Value::Number(n as f64),
                    formula: None,
                },
            );
            for (si, col, formula) in [
                (0, 1, format!("=A{n}*3")),
                (0, 2, format!("=SQRT(B{n}^2)+$H$1")),
                (1, 0, format!("=SUM(Sheet1!B{n}:C{n})")),
                (1, 1, format!("=IF(A{n}>0,TEXT(A{n},\"0\"),\"no\")")),
            ] {
                set_formula(wb.sheet_mut(si), row, col, &formula, FValue::Number(-999.0));
            }
        }
        set_formula(wb.sheet_mut(0), 0, 7, "=1+1", FValue::Number(999.));
        wb
    }

    #[test]
    fn evaluated_levels_match_serial_with_ranges_across_sheets_and_stale_inputs() {
        let mut wb = fixture();
        wb.names.push(("Rate".into(), None, "=5".into()));
        set_formula(wb.sheet_mut(0), 0, 9, "=Rate+1", FValue::Empty);
        set_formula(wb.sheet_mut(0), 1, 9, "=J1+2", FValue::Empty);
        for (row, formula) in [
            "=SEQUENCE(1,2,3)",
            "=INDIRECT(\"A1\")",
            "=NOW()",
            "=D4+1",
            "=IF(FALSE,D5,7)",
        ]
        .into_iter()
        .enumerate()
        {
            set_formula(wb.sheet_mut(0), row as u32, 3, formula, FValue::Empty);
        }
        let mut keys: Vec<_> = wb
            .sheets
            .iter()
            .enumerate()
            .flat_map(|(i, s)| s.formula_cells().map(move |(k, _)| (i, k.row, k.col)))
            .collect();
        keys.sort_unstable();
        let progress = AtomicUsize::new(0);
        let batch =
            evaluate_with_workers(&wb, &keys, None, None, None, Some(&progress), 4).unwrap();
        let pending: Vec<_> = batch.pending.iter().map(|&i| keys[i]).collect();
        assert_eq!(
            pending,
            [
                (0, 0, 3),
                (0, 0, 9),
                (0, 1, 3),
                (0, 1, 9),
                (0, 2, 3),
                (0, 3, 3),
                (0, 4, 3)
            ]
        );
        assert_eq!(progress.load(Ordering::Relaxed), keys.len() - pending.len());
        let mut serial = Calc::new(&wb);
        for (i, &(s, r, c)) in keys.iter().enumerate() {
            if batch.pending.binary_search(&i).is_err() {
                assert_eq!(
                    batch.values[i].3,
                    serial.cell_value(s, r, c),
                    "at {s}:{r},{c}"
                );
            }
        }
        drop(serial);
        recalc_all_force(&mut wb);
        assert_eq!(wb.sheet(1).value(759, 0), Value::Number(4562.));
        assert_eq!(
            wb.sheet(0).value(3, 3),
            Value::Error(super::super::ErrKind::Circular)
        );
        assert_eq!(wb.sheet(0).value(4, 3), Value::Number(7.));
        assert_eq!(wb.sheet(0).value(1, 9), Value::Number(8.));
    }

    #[test]
    fn dirty_levels_read_unaffected_formula_caches_and_cancel_before_publication() {
        let mut wb = fixture();
        recalc_all_force(&mut wb);
        let deps = Deps::build(&wb);
        let mut seeds = Vec::new();
        for row in 0..760 {
            wb.sheet_mut(0).set(
                row,
                0,
                Cell {
                    value: Value::Number(3.),
                    formula: None,
                },
            );
            seeds.push((0, row, 0));
        }
        // Cache outside the changed formulas' closure is deliberately different from its formula.
        wb.sheet_mut(0).set_formula_cache(0, 7, Value::Number(10.));
        let dirty = deps.edit_closure(&seeds);
        let mut keys: Vec<_> = dirty.iter().copied().collect();
        keys.sort_unstable();
        assert!(!dirty.contains(&(0, 0, 7)));
        let batch = evaluate_with_workers(&wb, &keys, None, Some(&dirty), None, None, 4).unwrap();
        assert!(batch.pending.is_empty());
        assert_eq!(
            batch.values[keys.binary_search(&(1, 759, 0)).unwrap()].3,
            FValue::Number(28.)
        );
        let flag = AtomicBool::new(true);
        assert!(
            evaluate_with_workers(&wb, &keys, None, Some(&dirty), Some(&flag), None, 4).is_none()
        );
        assert_eq!(wb.sheet(0).value(759, 1), Value::Number(2280.));
        recalc_dirty(&mut wb, &deps, &seeds);
        assert_eq!(wb.sheet(1).value(759, 0), Value::Number(28.));
    }
}
