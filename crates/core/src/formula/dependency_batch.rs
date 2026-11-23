//! Multi-core topological formula evaluator.
//!
//! Key capabilities:
//! - Zero `MAX_RANGE` limit: large literal ranges (`A:A`, `A1:Z100000`) add 0 edges and run
//!   at Level 0 across all cores; ranges containing `> 64` formulas use Virtual Range Nodes
//!   (`O(1)` edge per consumer instead of `O(N^2)` edges) + `SharedCalcCaches`.
//! - Static defined names (`Node::Name` / `Node::ScopedName`) run in parallel.
//! - Direct `FValue` transport (`Arc<str>`, `Arc<Array>`) without `Value <-> FValue` roundtrips.
//! - Dynamic atomic work-stealing across up to 32 cores without wiping `Calc` caches on array/lambda cells.

use super::{
    Calc, FValue, SharedCalcCaches, Workbook, compiled,
    deps::{self, CellKey, Rect},
    functions::EvalCtx,
    scalar_batch::Batch,
};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};

const MIN_FORMULAS: usize = 2_048;
const MIN_PER_WORKER: usize = 128;
const DIRECT_RANGE_FORMULA_CAP: usize = 64;
const WORK_CHUNK: usize = 64;

struct FormulaIndex<'a> {
    keys: &'a [CellKey],
    values: &'a [Option<FValue>],
    by_sheet: Option<HashMap<usize, BTreeMap<u32, Vec<(u32, usize)>>>>,
    virtual_map: HashMap<(usize, u32, u32, u32, u32), Option<usize>>,
    virtual_defs: Vec<Vec<usize>>,
}

impl<'a> FormulaIndex<'a> {
    fn new(keys: &'a [CellKey], values: &'a [Option<FValue>]) -> Self {
        Self {
            keys,
            values,
            by_sheet: None,
            virtual_map: HashMap::new(),
            virtual_defs: Vec::new(),
        }
    }

    fn ensure_columns(&mut self) -> &HashMap<usize, BTreeMap<u32, Vec<(u32, usize)>>> {
        let keys = self.keys;
        self.by_sheet.get_or_insert_with(|| {
            let mut sheets: HashMap<usize, BTreeMap<u32, Vec<(u32, usize)>>> = HashMap::new();
            for (slot, &(sheet, row, col)) in keys.iter().enumerate() {
                sheets
                    .entry(sheet)
                    .or_default()
                    .entry(col)
                    .or_default()
                    .push((row, slot));
            }
            sheets
        })
    }

    #[inline]
    fn point(&self, key: CellKey, out: &mut Vec<usize>) {
        if let Ok(slot) = self.keys.binary_search(&key)
            && self.values[slot].is_none()
        {
            out.push(slot);
        }
    }

    fn collect(&mut self, r: Rect, out: &mut Vec<usize>) -> bool {
        if r.r1 == r.r2 && r.c1 == r.c2 {
            if let (Ok(row), Ok(col)) = (u32::try_from(r.r1), u32::try_from(r.c1)) {
                self.point((r.sheet, row, col), out);
            }
            return true;
        }
        let (Ok(top), Ok(bottom), Ok(left), Ok(right)) = (
            u32::try_from(r.r1.max(0)),
            u32::try_from(r.r2.max(0)),
            u32::try_from(r.c1.max(0)),
            u32::try_from(r.c2.max(0)),
        ) else {
            return false;
        };
        if top > bottom || left > right {
            return true;
        }
        let vkey = (r.sheet, top, left, bottom, right);
        if let Some(&cached_virt) = self.virtual_map.get(&vkey) {
            if let Some(virt_slot) = cached_virt {
                out.push(virt_slot);
            }
            return true;
        }

        self.ensure_columns();
        let values = self.values;
        let Some(cols) = self.by_sheet.as_ref().unwrap().get(&r.sheet) else {
            // Zero formula cells on this sheet -> pure literal sheet!
            return true;
        };

        let mut small_buf: Vec<usize> = Vec::new();
        let mut overflowed = false;
        for (_, rows) in cols.range(left..=right) {
            let start = rows.partition_point(|(row, _)| *row < top);
            let end = rows.partition_point(|(row, _)| *row <= bottom);
            for &(_, slot) in &rows[start..end] {
                if values[slot].is_none() {
                    small_buf.push(slot);
                    if small_buf.len() > DIRECT_RANGE_FORMULA_CAP && !overflowed {
                        overflowed = true;
                    }
                }
            }
        }

        if small_buf.is_empty() {
            self.virtual_map.insert(vkey, None);
            return true;
        }
        if !overflowed {
            out.extend_from_slice(&small_buf);
            return true;
        }

        small_buf.sort_unstable();
        small_buf.dedup();
        let virt_slot = self.keys.len() + self.virtual_defs.len();
        self.virtual_defs.push(small_buf);
        self.virtual_map.insert(vkey, Some(virt_slot));
        out.push(virt_slot);
        true
    }
}

fn collect_lambda_names(wb: &Workbook) -> HashSet<String> {
    wb.names
        .iter()
        .filter(|(_, _, def)| {
            def.as_bytes()
                .windows(6)
                .any(|w| w.eq_ignore_ascii_case(b"LAMBDA"))
        })
        .map(|(name, _, _)| name.to_ascii_uppercase())
        .collect()
}

fn uses_named_lambda(node: &super::Node, lambda_names: &HashSet<String>) -> bool {
    use super::Node;
    match node {
        Node::Name(n) => lambda_names.contains(n),
        Node::ScopedName { name, .. } => lambda_names.contains(&name.to_ascii_uppercase()),
        Node::Func(name, args) => {
            lambda_names.contains(name) || args.iter().any(|v| uses_named_lambda(v, lambda_names))
        }
        Node::Implicit(v) | Node::Percent(v) | Node::Unary(_, v) => {
            uses_named_lambda(v, lambda_names)
        }
        Node::Binary(_, a, b) => {
            uses_named_lambda(a, lambda_names) || uses_named_lambda(b, lambda_names)
        }
        _ => false,
    }
}

fn prerequisites(
    wb: &Workbook,
    key: CellKey,
    index: &mut FormulaIndex<'_>,
    lambda_names: &HashSet<String>,
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
    if let Some(simple) = compiled::scalar_at(src, row, col) {
        simple.refs(|r, c| index.point((si, r, c), slots));
    } else {
        if !lambda_names.is_empty()
            && compiled::with_node(src, row, col, |node| uses_named_lambda(node, lambda_names))
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

struct Edges {
    offsets: Vec<usize>,
    slots: Vec<usize>,
}

impl Edges {
    #[inline]
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
        values: &[Option<FValue>],
        cancel: Option<&AtomicBool>,
    ) -> Option<Self> {
        let mut index = FormulaIndex::new(keys, values);
        let lambda_names = collect_lambda_names(wb);
        let mut inputs = Edges {
            offsets: Vec::with_capacity(keys.len() + 1),
            slots: Vec::new(),
        };
        let mut eligible = vec![false; keys.len()];
        let edge_limit = (keys.len().saturating_mul(24)).clamp(16_384, 8_388_608);
        let mut scratch = Vec::new();
        for (slot, &key) in keys.iter().enumerate() {
            if slot % 256 == 0 && cancelled(cancel) {
                return None;
            }
            inputs.offsets.push(inputs.slots.len());
            scratch.clear();
            if values[slot].is_some()
                || !prerequisites(wb, key, &mut index, &lambda_names, &mut scratch)
            {
                continue;
            }
            if inputs.slots.len() + scratch.len() > edge_limit {
                return None;
            }
            eligible[slot] = true;
            inputs.slots.extend_from_slice(&scratch);
        }
        inputs.offsets.push(inputs.slots.len());

        let n_real = keys.len();
        let virtual_defs = index.virtual_defs;
        let total_nodes = n_real + virtual_defs.len();
        let mut counts = vec![0usize; total_nodes + 1];
        let mut waiting = vec![0usize; total_nodes];
        let mut ready = Vec::new();

        for slot in 0..n_real {
            if !eligible[slot] {
                continue;
            }
            let deps_slice = inputs.get(slot);
            for &dep in deps_slice {
                counts[dep + 1] += 1;
                waiting[slot] += 1;
            }
            if waiting[slot] == 0 {
                ready.push(slot);
            }
        }
        for (v_idx, members) in virtual_defs.iter().enumerate() {
            let v_node = n_real + v_idx;
            for &real_dep in members {
                counts[real_dep + 1] += 1;
                waiting[v_node] += 1;
            }
        }

        for i in 1..counts.len() {
            counts[i] += counts[i - 1];
        }
        let mut write = counts[..total_nodes].to_vec();
        let mut consumers = Edges {
            slots: vec![0; counts[total_nodes]],
            offsets: counts,
        };

        for slot in 0..n_real {
            if slot % 256 == 0 && cancelled(cancel) {
                return None;
            }
            if !eligible[slot] {
                continue;
            }
            for &dep in inputs.get(slot) {
                consumers.slots[write[dep]] = slot;
                write[dep] += 1;
            }
        }
        for (v_idx, members) in virtual_defs.iter().enumerate() {
            let v_node = n_real + v_idx;
            for &real_dep in members {
                consumers.slots[write[real_dep]] = v_node;
                write[real_dep] += 1;
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

#[inline]
fn cancelled(cancel: Option<&AtomicBool>) -> bool {
    cancel.is_some_and(|flag| flag.load(Ordering::Relaxed))
}

fn eval_ready_slice<'a>(
    calc: &mut Calc<'a>,
    keys: &'a [CellKey],
    slice: &[usize],
    inputs: &Edges,
    values: &'a [Option<FValue>],
    cancel: Option<&AtomicBool>,
    out: &mut Vec<(usize, FValue)>,
) {
    let n_real = keys.len();
    for &slot in slice {
        if cancelled(cancel) {
            break;
        }
        for &input in inputs.get(slot) {
            if input < n_real {
                if let Some(ref fv) = values[input] {
                    calc.memo.entry(keys[input]).or_insert_with(|| fv.clone());
                }
            }
        }
        let (si, r, c) = keys[slot];
        let v = calc.cell_value_inner(si, r, c, true);
        if calc.requested.is_some() || matches!(v, FValue::Array(_) | FValue::Lambda(_)) {
            calc.reset_partial((si, r, c));
        } else {
            out.push((slot, v));
        }
    }
}

pub(super) fn evaluate<'a>(
    wb: &'a Workbook,
    keys: &[CellKey],
    initial: Option<Batch>,
    scope: Option<&'a HashSet<CellKey>>,
    shared: &'a SharedCalcCaches,
    ctx: EvalCtx,
    cancel: Option<&AtomicBool>,
    done: Option<&AtomicUsize>,
) -> Option<Batch> {
    let workers = std::thread::available_parallelism()
        .map_or(1, usize::from)
        .min(32);
    evaluate_with_workers(wb, keys, initial, scope, Some(shared), ctx, cancel, done, workers)
}

pub(super) fn dirty<'a>(
    wb: &'a Workbook,
    keys: &[CellKey],
    scope: &'a HashSet<CellKey>,
    shared: &'a SharedCalcCaches,
    ctx: EvalCtx,
    cancel: Option<&AtomicBool>,
) -> Option<Batch> {
    let workers = std::thread::available_parallelism()
        .map_or(1, usize::from)
        .min(32);
    evaluate_with_workers(wb, keys, None, Some(scope), Some(shared), ctx, cancel, None, workers)
}

fn evaluate_with_workers<'a>(
    wb: &'a Workbook,
    keys: &[CellKey],
    initial: Option<Batch>,
    scope: Option<&'a HashSet<CellKey>>,
    shared: Option<&'a SharedCalcCaches>,
    ctx: EvalCtx,
    cancel: Option<&AtomicBool>,
    done: Option<&AtomicUsize>,
    workers: usize,
) -> Option<Batch> {
    let pending = initial.as_ref().map_or(keys.len(), |b| b.pending.len());
    if pending < MIN_FORMULAS || workers < 2 || cancelled(cancel) {
        return initial;
    }
    let owned_shared;
    let shared_ref = match shared {
        Some(s) => s,
        None => {
            owned_shared = SharedCalcCaches::default();
            &owned_shared
        }
    };

    let mut values: Vec<Option<FValue>> = vec![None; keys.len()];
    if let Some(batch) = &initial {
        let mut unresolved = batch.pending.iter().copied().peekable();
        for (i, (_, _, _, value)) in batch.values.iter().enumerate() {
            if unresolved.next_if_eq(&i).is_none() {
                values[i] = Some(value.clone());
            }
        }
    }
    let Some(mut plan) = Plan::build(wb, keys, &values, cancel) else {
        return initial;
    };
    let n_real = keys.len();
    let mut completed = keys.len() - pending;
    let mut narrow_levels = 0;

    while !plan.ready.is_empty() && !cancelled(cancel) {
        let ready = std::mem::take(&mut plan.ready);
        let threads = (ready.len() / MIN_PER_WORKER).clamp(1, workers);
        if threads == 1 {
            narrow_levels += 1;
            if narrow_levels > 8 {
                break;
            }
        } else {
            narrow_levels = 0;
        }

        let results: Vec<Vec<(usize, FValue)>> = if threads == 1 {
            let mut calc = Calc::new_worker(wb, ctx, scope, Some(shared_ref));
            calc.scheduling = true;
            calc.parallel_values = Some((keys, &values));
            let mut out = Vec::with_capacity(ready.len());
            eval_ready_slice(&mut calc, keys, &ready, &plan.inputs, &values, cancel, &mut out);
            vec![out]
        } else {
            let next_idx = AtomicUsize::new(0);
            let ready_slice = ready.as_slice();
            let inputs_ref = &plan.inputs;
            let values_ref = values.as_slice();
            std::thread::scope(|pool| {
                let jobs: Vec<_> = (0..threads)
                    .map(|_| {
                        let next_idx = &next_idx;
                        pool.spawn(move || {
                            let mut calc = Calc::new_worker(wb, ctx, scope, Some(shared_ref));
                            calc.scheduling = true;
                            calc.parallel_values = Some((keys, values_ref));
                            let mut out = Vec::new();
                            loop {
                                if cancelled(cancel) {
                                    break;
                                }
                                let start = next_idx.fetch_add(WORK_CHUNK, Ordering::Relaxed);
                                if start >= ready_slice.len() {
                                    break;
                                }
                                let end = (start + WORK_CHUNK).min(ready_slice.len());
                                eval_ready_slice(
                                    &mut calc,
                                    keys,
                                    &ready_slice[start..end],
                                    inputs_ref,
                                    values_ref,
                                    cancel,
                                    &mut out,
                                );
                            }
                            out
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
                    if consumer < n_real {
                        plan.ready.push(consumer);
                    } else {
                        for &real_consumer in plan.consumers.get(consumer) {
                            plan.waiting[real_consumer] -= 1;
                            if plan.waiting[real_consumer] == 0 {
                                plan.ready.push(real_consumer);
                            }
                        }
                    }
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
    let mut pending_out = Vec::new();
    let out_values = keys
        .iter()
        .zip(values)
        .enumerate()
        .map(|(slot, (&(si, row, col), value))| {
            let v = value.unwrap_or_else(|| {
                pending_out.push(slot);
                FValue::Empty
            });
            (si, row, col, v)
        })
        .collect();
    Some(Batch {
        values: out_values,
        pending: pending_out,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Value;
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
                set_formula(
                    wb.sheet_mut(si),
                    row,
                    col,
                    &formula,
                    FValue::Number(-999.0),
                );
            }
        }
        set_formula(wb.sheet_mut(0), 0, 7, "=1+1", FValue::Number(999.));
        wb
    }

    #[test]
    fn evaluated_levels_match_serial_with_ranges_across_sheets_and_stale_inputs() {
        let mut wb = fixture();
        wb.names.push(("Rate".into(), None, "=5".into()));
        wb.names
            .push(("Tripler".into(), None, "=LAMBDA(x,x*3)".into()));
        set_formula(wb.sheet_mut(0), 0, 9, "=Rate+1", FValue::Empty);
        set_formula(wb.sheet_mut(0), 1, 9, "=J1+2", FValue::Empty);
        set_formula(wb.sheet_mut(0), 2, 9, "=Tripler(2)", FValue::Empty);
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
        let ctx = EvalCtx::for_workbook(wb.date_system);
        let batch = evaluate_with_workers(&wb, &keys, None, None, None, ctx, None, Some(&progress), 4)
            .unwrap();
        let pending: Vec<_> = batch.pending.iter().map(|&i| keys[i]).collect();
        // Static defined names (`Rate`, `J1+2`) are now evaluated in parallel;
        // dynamic array `SEQUENCE`, `INDIRECT`, `NOW`, cyclic `D4`/`D5`, and named `LAMBDA` stay pending.
        assert_eq!(
            pending,
            [
                (0, 0, 3),
                (0, 1, 3),
                (0, 2, 3),
                (0, 2, 9),
                (0, 3, 3),
                (0, 4, 3),
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
        assert_eq!(wb.sheet(0).value(0, 9), Value::Number(6.));
        assert_eq!(wb.sheet(0).value(1, 9), Value::Number(8.));
        assert_eq!(wb.sheet(0).value(2, 9), Value::Number(6.));
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
        let ctx = EvalCtx::for_workbook(wb.date_system);
        let batch =
            evaluate_with_workers(&wb, &keys, None, Some(&dirty), None, ctx, None, None, 4)
                .unwrap();
        assert!(batch.pending.is_empty());
        assert_eq!(
            batch.values[keys.binary_search(&(1, 759, 0)).unwrap()].3,
            FValue::Number(28.)
        );
        let flag = AtomicBool::new(true);
        assert!(
            evaluate_with_workers(&wb, &keys, None, Some(&dirty), None, ctx, Some(&flag), None, 4)
                .is_none()
        );
        assert_eq!(wb.sheet(0).value(759, 1), Value::Number(2280.));
        recalc_dirty(&mut wb, &deps, &seeds);
        assert_eq!(wb.sheet(1).value(759, 0), Value::Number(28.));
    }
}
