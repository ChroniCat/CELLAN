//! Multi-core topological formula evaluator using a persistent thread pool and
//! barrier-free atomic-indegree DAG ready queue.
//!
//! Key capabilities:
//! - Persistent worker pool (`formula::pool`): zero OS thread creation per pass/level.
//! - Barrier-free atomic-indegree scheduling with thread-local chain continuation:
//!   narrow/deep dependency chains (e.g. 20×2000) stay 100% multi-core with near-zero lock contention
//!   and never fall back to serial after 8 narrow levels.
//! - Parallel `Plan::build` prerequisite extraction with O(1) literal-sheet fast skip and
//!   Virtual Range Nodes (`O(1)` edges for ranges containing `> 64` formulas).
//! - Workload-weighted parallel threshold so heavy range/lookup/criteria batches also scale.

use super::{
    Calc, FValue, NameIndex, SharedCalcCaches, Workbook, compiled,
    deps::{self, CellKey, Rect},
    functions::EvalCtx,
    fxhash::{FxHashMap, FxHashSet, PackedCellKey},
    pool,
    scalar_batch::Batch,
};
use std::{
    cell::UnsafeCell,
    collections::{BTreeMap, HashSet},
    sync::{
        Condvar, Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
    },
};

const MIN_FORMULAS_FLOOR: usize = 256;
const MIN_WEIGHTED_WORK: usize = 1_024;
const DIRECT_RANGE_FORMULA_CAP: usize = 64;
const WORK_CHUNK: usize = 64;
const BUILD_CHUNK: usize = 512;

type VKey = (usize, u32, u32, u32, u32);

pub(super) struct ParallelSlot(UnsafeCell<Option<FValue>>);

// Safety: each slot is written at most once by the unique worker that claims its DAG node,
// strictly before `states[idx]` transitions to `2` with `Release`. Readers only dereference
// after observing `states[idx].load(Acquire) == 2`.
unsafe impl Sync for ParallelSlot {}

impl ParallelSlot {
    #[inline]
    fn new(v: Option<FValue>) -> Self {
        Self(UnsafeCell::new(v))
    }

    #[inline]
    fn into_inner(self) -> Option<FValue> {
        self.0.into_inner()
    }
}

#[derive(Clone, Copy)]
pub(super) struct ParallelValues<'a> {
    keys: &'a [CellKey],
    states: &'a [AtomicU8],
    slots: &'a [ParallelSlot],
    sheet_mask: u64,
}

impl<'a> ParallelValues<'a> {
    fn new(keys: &'a [CellKey], states: &'a [AtomicU8], slots: &'a [ParallelSlot]) -> Self {
        let mut sheet_mask = 0u64;
        for &(si, _, _) in keys {
            if si < 64 {
                sheet_mask |= 1u64 << si;
            } else {
                sheet_mask = u64::MAX;
                break;
            }
        }
        Self {
            keys,
            states,
            slots,
            sheet_mask,
        }
    }

    #[inline]
    pub(super) fn get(&self, key: CellKey) -> Option<&'a FValue> {
        if key.0 < 64 && (self.sheet_mask & (1u64 << key.0)) == 0 {
            return None;
        }
        let idx = self.keys.binary_search(&key).ok()?;
        if self.states[idx].load(Ordering::Acquire) == 2 {
            // Safety: `Release` store of 2 happens-after the sole write to `slots[idx]`.
            unsafe { (*self.slots[idx].0.get()).as_ref() }
        } else {
            None
        }
    }

    #[inline]
    pub(super) fn get_by_slot(&self, idx: usize) -> Option<&'a FValue> {
        if self.states[idx].load(Ordering::Acquire) == 2 {
            unsafe { (*self.slots[idx].0.get()).as_ref() }
        } else {
            None
        }
    }

    #[inline]
    pub(super) fn is_ready(&self, key: CellKey) -> bool {
        if key.0 < 64 && (self.sheet_mask & (1u64 << key.0)) == 0 {
            return false;
        }
        self.keys
            .binary_search(&key)
            .ok()
            .is_some_and(|idx| self.states[idx].load(Ordering::Acquire) == 2)
    }
}

struct ReadOnlyFormulaIndex<'a> {
    keys: &'a [CellKey],
    values: &'a [Option<FValue>],
    sheet_has_unresolved: Vec<bool>,
    /// For each sheet with unresolved formulas: col -> sorted Vec<(row, slot)>
    by_sheet: Vec<Option<BTreeMap<u32, Vec<(u32, usize)>>>>,
}

impl<'a> ReadOnlyFormulaIndex<'a> {
    fn new(wb: &Workbook, keys: &'a [CellKey], values: &'a [Option<FValue>]) -> Self {
        let num_sheets = wb.sheets.len();
        let mut sheet_has_unresolved = vec![false; num_sheets];
        for (slot, &(si, _, _)) in keys.iter().enumerate() {
            if values[slot].is_none() && si < num_sheets {
                sheet_has_unresolved[si] = true;
            }
        }
        let mut by_sheet: Vec<Option<BTreeMap<u32, Vec<(u32, usize)>>>> = vec![None; num_sheets];
        for (slot, &(si, row, col)) in keys.iter().enumerate() {
            if values[slot].is_none() && si < num_sheets {
                by_sheet[si]
                    .get_or_insert_with(BTreeMap::new)
                    .entry(col)
                    .or_default()
                    .push((row, slot));
            }
        }
        // `keys` is sorted by `(si, row, col)`, so each column's Vec<(row, slot)> is already
        // strictly sorted by `row`!
        Self {
            keys,
            values,
            sheet_has_unresolved,
            by_sheet,
        }
    }

    #[inline]
    fn point_near(&self, slot_hint: usize, key: CellKey, out: &mut Vec<usize>) {
        if slot_hint > 0 && self.keys[slot_hint - 1] == key {
            if self.values[slot_hint - 1].is_none() {
                out.push(slot_hint - 1);
            }
            return;
        }
        if slot_hint + 1 < self.keys.len() && self.keys[slot_hint + 1] == key {
            if self.values[slot_hint + 1].is_none() {
                out.push(slot_hint + 1);
            }
            return;
        }
        self.point(key, out);
    }

    #[inline]
    fn point(&self, key: CellKey, out: &mut Vec<usize>) {
        let (si, row, col) = key;
        if si >= self.sheet_has_unresolved.len() || !self.sheet_has_unresolved[si] {
            return;
        }
        if let Some(cols) = &self.by_sheet[si]
            && let Some(rows) = cols.get(&col)
            && let Ok(pos) = rows.binary_search_by_key(&row, |&(r, _)| r)
        {
            out.push(rows[pos].1);
        }
    }

    fn collect(
        &self,
        r: Rect,
        local_vcache: &mut FxHashMap<VKey, LocalRangeResult>,
        direct_out: &mut Vec<usize>,
        virt_out: &mut Vec<VKey>,
    ) -> bool {
        if r.sheet >= self.sheet_has_unresolved.len() || !self.sheet_has_unresolved[r.sheet] {
            return true;
        }
        if r.r1 == r.r2 && r.c1 == r.c2 {
            if let (Ok(row), Ok(col)) = (u32::try_from(r.r1), u32::try_from(r.c1)) {
                self.point((r.sheet, row, col), direct_out);
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
        if let Some(cached) = local_vcache.get(&vkey) {
            match cached {
                LocalRangeResult::Empty => {}
                LocalRangeResult::Direct(slots) => direct_out.extend_from_slice(slots),
                LocalRangeResult::Virtual(_) => virt_out.push(vkey),
            }
            return true;
        }

        let Some(cols) = self.by_sheet[r.sheet].as_ref() else {
            return true;
        };

        let mut buf: Vec<usize> = Vec::new();
        for (_, rows) in cols.range(left..=right) {
            let start = rows.partition_point(|(row, _)| *row < top);
            let end = rows.partition_point(|(row, _)| *row <= bottom);
            for &(_, slot) in &rows[start..end] {
                if self.values[slot].is_none() {
                    buf.push(slot);
                }
            }
        }

        if buf.is_empty() {
            local_vcache.insert(vkey, LocalRangeResult::Empty);
            return true;
        }
        if buf.len() <= DIRECT_RANGE_FORMULA_CAP {
            direct_out.extend_from_slice(&buf);
            if buf.len() > 8 {
                local_vcache.insert(vkey, LocalRangeResult::Direct(buf));
            }
            return true;
        }

        buf.sort_unstable();
        buf.dedup();
        local_vcache.insert(vkey, LocalRangeResult::Virtual(buf));
        virt_out.push(vkey);
        true
    }
}

enum LocalRangeResult {
    Empty,
    Direct(Vec<usize>),
    Virtual(Vec<usize>),
}

fn collect_lambda_names(wb: &Workbook) -> FxHashSet<String> {
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

fn uses_named_lambda(node: &super::Node, lambda_names: &FxHashSet<String>) -> bool {
    use super::Node;
    match node {
        Node::Name(n) => lambda_names.contains(n),
        Node::ScopedName { name, .. } => lambda_names.contains(&name.to_ascii_uppercase()),
        Node::Func(name, args) => {
            lambda_names.contains(name.as_str())
                || args.iter().any(|v| uses_named_lambda(v, lambda_names))
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

fn resolve_sheet_idx(wb: &Workbook, name: Option<&str>, cur: usize) -> usize {
    match name {
        Some(n) => wb
            .sheets
            .iter()
            .position(|s| s.name.eq_ignore_ascii_case(n))
            .unwrap_or(cur),
        None => cur,
    }
}

fn collect_slot_prerequisites(
    wb: &Workbook,
    name_index: Option<&NameIndex>,
    has_tables: bool,
    slot_hint: usize,
    key: CellKey,
    index: &ReadOnlyFormulaIndex<'_>,
    lambda_names: &FxHashSet<String>,
    local_vcache: &mut FxHashMap<VKey, LocalRangeResult>,
    direct_slots: &mut Vec<usize>,
    virt_keys: &mut Vec<VKey>,
) -> bool {
    let (si, row, col) = key;
    let Some(formula_arc) = wb.sheet(si).formula_arc_of(row, col) else {
        return false;
    };

    // Fast path for projected scalar formulas (same-sheet OR cross-sheet arithmetic)
    let may_use_table_ref = has_tables && formula_arc.as_bytes().contains(&b'[');
    if !may_use_table_ref {
        let mut fast_done = false;
        let mut has_lambda = false;
        let _ = compiled::with_eval_arc(formula_arc, row, col, |node, dr, dc, kind| {
            if kind != compiled::ScalarKind::None {
                compiled::visit_projected_refs(node, dr, dc, &mut |sheet_opt, r, c| {
                    let target_si = resolve_sheet_idx(wb, sheet_opt, si);
                    index.point_near(slot_hint, (target_si, r, c), direct_slots);
                });
                fast_done = true;
            } else if !lambda_names.is_empty() && uses_named_lambda(node, lambda_names) {
                has_lambda = true;
            }
        });
        if fast_done {
            direct_slots.sort_unstable();
            direct_slots.dedup();
            return true;
        }
        if has_lambda {
            return false;
        }
    } else if !lambda_names.is_empty()
        && compiled::with_shifted_arc(formula_arc, row, col, |node, _, _| {
            uses_named_lambda(node, lambda_names)
        })
        .unwrap_or(true)
    {
        return false;
    }

    let mut areas_ok = true;
    let inputs_ok = deps::visit_parallel_inputs_arc(
        wb,
        name_index,
        may_use_table_ref,
        key,
        formula_arc,
        |area| {
            if areas_ok && !index.collect(area, local_vcache, direct_slots, virt_keys) {
                areas_ok = false;
            }
        },
    );
    if !inputs_ok || !areas_ok {
        return false;
    }
    direct_slots.sort_unstable();
    direct_slots.dedup();
    virt_keys.sort_unstable();
    virt_keys.dedup();
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
    waiting: Vec<AtomicUsize>,
    ready: Vec<usize>,
}

struct ChunkBuildOut {
    start: usize,
    eligible: Vec<bool>,
    direct_offsets: Vec<usize>,
    direct_slots: Vec<usize>,
    virt_offsets: Vec<usize>,
    virt_keys: Vec<VKey>,
    virtual_defs: Vec<(VKey, Vec<usize>)>,
}

impl Plan {
    fn build(
        wb: &Workbook,
        keys: &[CellKey],
        values: &[Option<FValue>],
        workers: usize,
        cancel: Option<&AtomicBool>,
    ) -> Option<Self> {
        let index = ReadOnlyFormulaIndex::new(wb, keys, values);
        let lambda_names = collect_lambda_names(wb);
        let has_tables = wb.sheets.iter().any(|s| !s.tables.is_empty());
        let name_index = NameIndex::build_if_needed(wb);
        let name_ix_ref = name_index.as_ref();

        let n_real = keys.len();
        let edge_limit = (n_real.saturating_mul(24)).clamp(16_384, 8_388_608);
        let build_workers = if n_real >= 2_048 {
            workers.min(n_real.div_ceil(BUILD_CHUNK)).min(16)
        } else {
            1
        };

        let num_chunks = n_real.div_ceil(BUILD_CHUNK);
        let aborted = AtomicBool::new(false);
        let chunk_results: Vec<Mutex<Option<ChunkBuildOut>>> =
            (0..num_chunks).map(|_| Mutex::new(None)).collect();

        if build_workers <= 1 {
            let mut local_vcache: FxHashMap<VKey, LocalRangeResult> = FxHashMap::default();
            let mut eligible = vec![false; n_real];
            let mut direct_offsets = Vec::with_capacity(n_real + 1);
            let mut direct_slots = Vec::new();
            let mut virt_offsets = Vec::with_capacity(n_real + 1);
            let mut virt_keys = Vec::new();
            let mut d_scratch = Vec::new();
            let mut v_scratch = Vec::new();
            for (slot, &key) in keys.iter().enumerate() {
                if slot % 256 == 0 && cancelled(cancel) {
                    return None;
                }
                direct_offsets.push(direct_slots.len());
                virt_offsets.push(virt_keys.len());
                if values[slot].is_some() {
                    continue;
                }
                d_scratch.clear();
                v_scratch.clear();
                if !collect_slot_prerequisites(
                    wb,
                    name_ix_ref,
                    has_tables,
                    slot,
                    key,
                    &index,
                    &lambda_names,
                    &mut local_vcache,
                    &mut d_scratch,
                    &mut v_scratch,
                ) {
                    continue;
                }
                if direct_slots.len() + d_scratch.len() + virt_keys.len() + v_scratch.len()
                    > edge_limit
                {
                    return None;
                }
                eligible[slot] = true;
                direct_slots.extend_from_slice(&d_scratch);
                virt_keys.extend_from_slice(&v_scratch);
            }
            direct_offsets.push(direct_slots.len());
            virt_offsets.push(virt_keys.len());
            let virtual_defs: Vec<(VKey, Vec<usize>)> = local_vcache
                .into_iter()
                .filter_map(|(k, v)| match v {
                    LocalRangeResult::Virtual(m) => Some((k, m)),
                    _ => None,
                })
                .collect();
            *chunk_results[0].lock().unwrap() = Some(ChunkBuildOut {
                start: 0,
                eligible,
                direct_offsets,
                direct_slots,
                virt_offsets,
                virt_keys,
                virtual_defs,
            });
        } else {
            let next_chunk = AtomicUsize::new(0);
            let total_edges = AtomicUsize::new(0);
            pool::run_parallel(build_workers, |_| {
                let mut local_vcache: FxHashMap<VKey, LocalRangeResult> = FxHashMap::default();
                let mut d_scratch = Vec::new();
                let mut v_scratch = Vec::new();
                loop {
                    if aborted.load(Ordering::Relaxed) || cancelled(cancel) {
                        aborted.store(true, Ordering::Relaxed);
                        return;
                    }
                    let ci = next_chunk.fetch_add(1, Ordering::Relaxed);
                    if ci >= num_chunks {
                        break;
                    }
                    let start = ci * BUILD_CHUNK;
                    let end = (start + BUILD_CHUNK).min(n_real);
                    let len = end - start;
                    let mut eligible = vec![false; len];
                    let mut direct_offsets = Vec::with_capacity(len + 1);
                    let mut direct_slots = Vec::new();
                    let mut virt_offsets = Vec::with_capacity(len + 1);
                    let mut virt_keys = Vec::new();
                    for local_i in 0..len {
                        let slot = start + local_i;
                        if local_i % 128 == 0 && (aborted.load(Ordering::Relaxed) || cancelled(cancel)) {
                            aborted.store(true, Ordering::Relaxed);
                            return;
                        }
                        direct_offsets.push(direct_slots.len());
                        virt_offsets.push(virt_keys.len());
                        if values[slot].is_some() {
                            continue;
                        }
                        d_scratch.clear();
                        v_scratch.clear();
                        if !collect_slot_prerequisites(
                            wb,
                            name_ix_ref,
                            has_tables,
                            slot,
                            keys[slot],
                            &index,
                            &lambda_names,
                            &mut local_vcache,
                            &mut d_scratch,
                            &mut v_scratch,
                        ) {
                            continue;
                        }
                        eligible[local_i] = true;
                        direct_slots.extend_from_slice(&d_scratch);
                        virt_keys.extend_from_slice(&v_scratch);
                    }
                    direct_offsets.push(direct_slots.len());
                    virt_offsets.push(virt_keys.len());
                    let added = direct_slots.len() + virt_keys.len();
                    if total_edges.fetch_add(added, Ordering::Relaxed) + added > edge_limit {
                        aborted.store(true, Ordering::Relaxed);
                        return;
                    }
                    // Extract any newly discovered virtual range defs for this chunk
                    let mut vdefs = Vec::new();
                    if !virt_keys.is_empty() {
                        let mut seen = FxHashSet::default();
                        for &vk in &virt_keys {
                            if seen.insert(vk)
                                && let Some(LocalRangeResult::Virtual(members)) =
                                    local_vcache.get(&vk)
                            {
                                vdefs.push((vk, members.clone()));
                            }
                        }
                    }
                    *chunk_results[ci].lock().unwrap() = Some(ChunkBuildOut {
                        start,
                        eligible,
                        direct_offsets,
                        direct_slots,
                        virt_offsets,
                        virt_keys,
                        virtual_defs: vdefs,
                    });
                }
            });
        }

        if aborted.load(Ordering::Relaxed) || cancelled(cancel) {
            return None;
        }

        // Assemble global virtual range map and flat CSR inputs
        let chunks: Vec<ChunkBuildOut> = chunk_results
            .into_iter()
            .filter_map(|m| m.into_inner().unwrap())
            .collect();

        let mut global_virt_map: FxHashMap<VKey, usize> = FxHashMap::default();
        let mut virtual_defs: Vec<Vec<usize>> = Vec::new();
        let mut total_input_slots = 0usize;
        for ch in &chunks {
            total_input_slots += ch.direct_slots.len() + ch.virt_keys.len();
            for (vk, members) in &ch.virtual_defs {
                if !global_virt_map.contains_key(vk) {
                    let v_slot = n_real + virtual_defs.len();
                    global_virt_map.insert(*vk, v_slot);
                    virtual_defs.push(members.clone());
                }
            }
        }
        if total_input_slots > edge_limit {
            return None;
        }

        let mut eligible = vec![false; n_real];
        let mut inputs = Edges {
            offsets: Vec::with_capacity(n_real + 1),
            slots: Vec::with_capacity(total_input_slots),
        };
        for ch in &chunks {
            let len = ch.eligible.len();
            for local_i in 0..len {
                let slot = ch.start + local_i;
                inputs.offsets.push(inputs.slots.len());
                if !ch.eligible[local_i] {
                    continue;
                }
                eligible[slot] = true;
                let ds = &ch.direct_slots[ch.direct_offsets[local_i]..ch.direct_offsets[local_i + 1]];
                let vs = &ch.virt_keys[ch.virt_offsets[local_i]..ch.virt_offsets[local_i + 1]];
                inputs.slots.extend_from_slice(ds);
                for vk in vs {
                    if let Some(&v_slot) = global_virt_map.get(vk) {
                        inputs.slots.push(v_slot);
                    }
                }
            }
        }
        inputs.offsets.push(inputs.slots.len());

        let total_nodes = n_real + virtual_defs.len();
        let mut counts = vec![0usize; total_nodes + 1];
        let mut waiting_raw = vec![0usize; total_nodes];
        let mut ready = Vec::new();

        for slot in 0..n_real {
            if !eligible[slot] {
                continue;
            }
            let deps_slice = inputs.get(slot);
            for &dep in deps_slice {
                counts[dep + 1] += 1;
                waiting_raw[slot] += 1;
            }
            if waiting_raw[slot] == 0 {
                ready.push(slot);
            }
        }
        for (v_idx, members) in virtual_defs.iter().enumerate() {
            let v_node = n_real + v_idx;
            for &real_dep in members {
                counts[real_dep + 1] += 1;
                waiting_raw[v_node] += 1;
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
            if slot % 1024 == 0 && cancelled(cancel) {
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

        let waiting = waiting_raw.into_iter().map(AtomicUsize::new).collect();
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

/// Estimate calculation workload across a sample of formulas so heavy range/criteria/lookup
/// batches (e.g. 500 `SUMIFS` on 200k rows) parallelize without hurting tiny scalar edits.
fn estimate_workload(wb: &Workbook, keys: &[CellKey], pending: usize) -> usize {
    if pending < MIN_FORMULAS_FLOOR {
        return 0;
    }
    if pending >= MIN_WEIGHTED_WORK {
        return pending;
    }
    let samples = keys.len().min(16);
    let stride = (keys.len() / samples).max(1);
    let mut weight_sum = 0usize;
    for i in 0..samples {
        let (si, r, c) = keys[(i * stride) % keys.len()];
        let heavy = wb
            .sheet(si)
            .formula_of(r, c)
            .is_some_and(|f| {
                let b = f.as_bytes();
                b.contains(&b':')
                    || b.windows(5).any(|w| {
                        w.eq_ignore_ascii_case(b"SUMIF")
                            || w.eq_ignore_ascii_case(b"COUNT")
                            || w.eq_ignore_ascii_case(b"LOOKU")
                            || w.eq_ignore_ascii_case(b"MATCH")
                            || w.eq_ignore_ascii_case(b"AVERA")
                            || w.eq_ignore_ascii_case(b"MINIF")
                            || w.eq_ignore_ascii_case(b"MAXIF")
                            || w.eq_ignore_ascii_case(b"FILTE")
                            || w.eq_ignore_ascii_case(b"MMULT")
                    })
            });
        weight_sum += if heavy { 16 } else { 1 };
    }
    pending.saturating_mul(weight_sum) / samples
}

/// Ultra-fast zero-DAG-allocation streaming evaluator for width-1 sequential scalar chains
/// (e.g. `chain 1M` `A2=A1+1` or running balances `B2=B1+A2`). When strided sampling proves
/// `keys[s]` directly depends on `keys[s - 1]` (parallelism width == 1), building a 110 MB
/// multi-core CSR `Plan` + waking 15 idle worker threads is pure overhead.
pub(super) fn try_eval_tight_forward_chain(
    wb: &Workbook,
    keys: &[CellKey],
    is_dirty_subset: bool,
    cancel: Option<&AtomicBool>,
    done: Option<&AtomicUsize>,
) -> Option<Batch> {
    if keys.len() < 256 || cancelled(cancel) {
        return None;
    }
    let has_tables = wb.sheets.iter().any(|s| !s.tables.is_empty());
    const SAMPLES: usize = 16;
    let span = keys.len() - 1;
    let stride = (span / SAMPLES).max(1);
    let mut hits_prev = 0usize;
    for i in 0..SAMPLES {
        let slot = 1 + ((i * stride + i * 13) % span);
        let key = keys[slot];
        let prev = keys[slot - 1];
        if key.0 != prev.0 {
            continue;
        }
        let Some(f_arc) = wb.sheets[key.0].formula_arc_of(key.1, key.2) else {
            return None;
        };
        if has_tables && f_arc.as_bytes().contains(&b'[') {
            return None;
        }
        let mut saw_prev = false;
        let mut all_backward = true;
        let ok = compiled::with_eval_arc(f_arc, key.1, key.2, |node, dr, dc, kind| {
            if kind != compiled::ScalarKind::SameSheet {
                return false;
            }
            compiled::visit_projected_refs(node, dr, dc, &mut |_, rr, cc| {
                let target = (key.0, rr, cc);
                if target == prev {
                    saw_prev = true;
                }
                if target >= key {
                    all_backward = false;
                }
            });
            true
        })
        .unwrap_or(false);
        if !ok || !all_backward {
            return None;
        }
        if saw_prev {
            hits_prev += 1;
        }
    }
    if hits_prev + 1 < SAMPLES {
        return None;
    }

    let n = keys.len();
    let mut out: Vec<(usize, u32, u32, FValue)> = Vec::with_capacity(n);
    for slot in 0..n {
        if slot & 4095 == 0 {
            if cancelled(cancel) {
                return None;
            }
            if let Some(d) = done {
                d.store(slot, Ordering::Relaxed);
            }
        }
        let (si, r, c) = keys[slot];
        let f_arc = wb.sheets[si].formula_arc_of(r, c)?;
        if has_tables && f_arc.as_bytes().contains(&b'[') {
            return None;
        }
        let val = compiled::with_eval_arc(f_arc, r, c, |node, dr, dc, kind| match kind {
            compiled::ScalarKind::SameSheet => {
                compiled::eval_projected_scalar(node, dr, dc, &mut |_, rr, cc| {
                    if slot > 0 {
                        let prev = &out[slot - 1];
                        if (prev.0, prev.1, prev.2) == (si, rr, cc) {
                            return Some(prev.3.clone());
                        }
                    }
                    match wb.sheets[si].get_view(rr, cc) {
                        None => Some(FValue::Empty),
                        Some(crate::store::CellView::Number(num)) => Some(FValue::Number(num)),
                        Some(crate::store::CellView::Value(v)) => Some(super::to_fvalue(v)),
                        Some(crate::store::CellView::Cell(cell)) => {
                            if cell.formula.is_none() {
                                return Some(super::to_fvalue(&cell.value));
                            }
                            let target = (si, rr, cc);
                            if target < (si, r, c) {
                                if let Ok(dep_slot) = keys[..slot].binary_search(&target) {
                                    return Some(out[dep_slot].3.clone());
                                }
                            } else if keys[slot..].binary_search(&target).is_ok() {
                                return None;
                            }
                            if is_dirty_subset && !super::callable_cache_candidate(&cell.value) {
                                Some(super::to_fvalue(&cell.value))
                            } else {
                                None
                            }
                        }
                    }
                })
            }
            _ => None,
        })
        .ok()
        .flatten()
        .filter(|v| !matches!(v, FValue::Array(_) | FValue::Lambda(_)))?;
        out.push((si, r, c, val));
    }
    if let Some(d) = done {
        d.store(n, Ordering::Relaxed);
    }
    Some(Batch {
        values: out,
        pending: Vec::new(),
    })
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
    if initial.is_none()
        && let Some(chain_batch) =
            try_eval_tight_forward_chain(wb, keys, scope.is_some(), cancel, done)
    {
        return Some(chain_batch);
    }
    let workers = pool::max_workers();
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
    let workers = pool::max_workers();
    evaluate_with_workers(wb, keys, None, Some(scope), Some(shared), ctx, cancel, None, workers)
}

struct DagQueue {
    ready: Vec<usize>,
    active: usize,
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
    if workers < 2 || cancelled(cancel) || estimate_workload(wb, keys, pending) < MIN_WEIGHTED_WORK {
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

    let n_real = keys.len();
    let mut init_values: Vec<Option<FValue>> = vec![None; n_real];
    if let Some(batch) = &initial {
        let mut unresolved = batch.pending.iter().copied().peekable();
        for (i, (_, _, _, value)) in batch.values.iter().enumerate() {
            if unresolved.next_if_eq(&i).is_none() {
                init_values[i] = Some(value.clone());
            }
        }
    }

    let Some(plan) = Plan::build(wb, keys, &init_values, workers, cancel) else {
        return initial;
    };
    if plan.ready.is_empty() || cancelled(cancel) {
        return initial;
    }

    let states: Vec<AtomicU8> = init_values
        .iter()
        .map(|v| AtomicU8::new(if v.is_some() { 2 } else { 0 }))
        .collect();
    let slots: Vec<ParallelSlot> = init_values.into_iter().map(ParallelSlot::new).collect();
    let pvals = ParallelValues::new(keys, &states, &slots);

    let initial_completed = n_real - pending;
    let completed_total = AtomicUsize::new(initial_completed);
    let waiting_workers = AtomicUsize::new(0);
    let queue_mu = Mutex::new(DagQueue {
        ready: plan.ready,
        active: 0,
    });
    let queue_cv = Condvar::new();

    // Limit thread count sensibly for smaller batches while still allowing full parallelism on chains
    let effective_workers = workers.min(pending.max(1));

    // Pre-build a shared FxHashSet<PackedCellKey> for scope if scope is present
    let fast_scope = scope.map(super::FastScope::from_std);
    let fast_scope_ref = fast_scope.as_ref();
    let has_tables = wb.sheets.iter().any(|s| !s.tables.is_empty());

    pool::run_parallel(effective_workers, |_| {
        let mut calc = Calc::new_worker(wb, ctx, scope, Some(shared_ref));
        calc.fast_scope = fast_scope_ref;
        calc.cancel = cancel;
        calc.scheduling = true;
        calc.parallel_values = Some(pvals);

        let mut local_work: Vec<usize> = Vec::with_capacity(WORK_CHUNK);
        let mut newly_ready: Vec<usize> = Vec::new();
        let mut is_active = false;
        let mut local_done = 0usize;

        loop {
            let slot = if let Some(s) = local_work.pop() {
                s
            } else {
                if local_done > 0 {
                    let prev = completed_total.fetch_add(local_done, Ordering::Relaxed);
                    if let Some(d) = done {
                        d.store(prev + local_done, Ordering::Relaxed);
                    }
                    local_done = 0;
                }
                let mut q = queue_mu.lock().unwrap();
                if is_active {
                    q.active -= 1;
                }
                loop {
                    if cancelled(cancel) {
                        queue_cv.notify_all();
                        return;
                    }
                    if !q.ready.is_empty() {
                        let take = (q.ready.len() / effective_workers).clamp(1, WORK_CHUNK);
                        let split = q.ready.len() - take;
                        local_work.extend(q.ready.drain(split..));
                        q.active += 1;
                        is_active = true;
                        break;
                    }
                    if q.active == 0 {
                        queue_cv.notify_all();
                        return;
                    }
                    waiting_workers.fetch_add(1, Ordering::Relaxed);
                    q = queue_cv.wait(q).unwrap();
                    waiting_workers.fetch_sub(1, Ordering::Relaxed);
                }
                local_work.pop().unwrap()
            };

            if cancelled(cancel) {
                let mut q = queue_mu.lock().unwrap();
                if is_active {
                    q.active -= 1;
                }
                queue_cv.notify_all();
                return;
            }

            let (si, r, c) = keys[slot];
            let inputs = plan.inputs.get(slot);

            // Fast zero-overhead direct scalar path (arithmetic, IF, IFERROR, IFNA):
            // reads prerequisites directly from `plan.inputs` via `pvals.get_by_slot` without
            // touching `Calc::memo`, `Calc::visiting`, or `pvals.binary_search`.
            let fast_val = if let Some(f_arc) = wb.sheets[si].formula_arc_of(r, c)
                && (!has_tables || !f_arc.as_bytes().contains(&b'['))
            {
                compiled::with_eval_arc(f_arc, r, c, |node, dr, dc, kind| match kind {
                    compiled::ScalarKind::SameSheet => {
                        compiled::eval_projected_scalar(node, dr, dc, &mut |_, rr, cc| {
                            for &in_slot in inputs {
                                if in_slot < n_real && keys[in_slot] == (si, rr, cc) {
                                    return pvals.get_by_slot(in_slot).cloned();
                                }
                            }
                            match wb.sheets[si].get_view(rr, cc) {
                                None => Some(FValue::Empty),
                                Some(crate::store::CellView::Number(n)) => Some(FValue::Number(n)),
                                Some(crate::store::CellView::Value(v)) => Some(super::to_fvalue(v)),
                                Some(crate::store::CellView::Cell(c2)) if c2.formula.is_none() => {
                                    Some(super::to_fvalue(&c2.value))
                                }
                                Some(crate::store::CellView::Cell(c2))
                                    if fast_scope_ref.is_some_and(|fs| !fs.contains((si, rr, cc)))
                                        && !super::callable_cache_candidate(&c2.value) =>
                                {
                                    Some(super::to_fvalue(&c2.value))
                                }
                                Some(_) => pvals.get((si, rr, cc)).cloned(),
                            }
                        })
                    }
                    compiled::ScalarKind::CrossSheet => {
                        compiled::eval_projected_scalar(node, dr, dc, &mut |sh_opt, rr, cc| {
                            let target_si = match sh_opt {
                                Some(name) => wb
                                    .sheets
                                    .iter()
                                    .position(|s| s.name.eq_ignore_ascii_case(name))?,
                                None => si,
                            };
                            for &in_slot in inputs {
                                if in_slot < n_real && keys[in_slot] == (target_si, rr, cc) {
                                    return pvals.get_by_slot(in_slot).cloned();
                                }
                            }
                            match wb.sheets[target_si].get_view(rr, cc) {
                                None => Some(FValue::Empty),
                                Some(crate::store::CellView::Number(n)) => Some(FValue::Number(n)),
                                Some(crate::store::CellView::Value(v)) => Some(super::to_fvalue(v)),
                                Some(crate::store::CellView::Cell(c2)) if c2.formula.is_none() => {
                                    Some(super::to_fvalue(&c2.value))
                                }
                                Some(crate::store::CellView::Cell(c2))
                                    if fast_scope_ref
                                        .is_some_and(|fs| !fs.contains((target_si, rr, cc)))
                                        && !super::callable_cache_candidate(&c2.value) =>
                                {
                                    Some(super::to_fvalue(&c2.value))
                                }
                                Some(_) => pvals.get((target_si, rr, cc)).cloned(),
                            }
                        })
                    }
                    compiled::ScalarKind::None => None,
                })
                .ok()
                .flatten()
                .filter(|v| !matches!(v, FValue::Array(_) | FValue::Lambda(_)))
            } else {
                None
            };

            let v = if let Some(v) = fast_val {
                v
            } else {
                // Seed direct inputs into worker's FxHash memo if not already cached locally
                for &input in inputs {
                    if input < n_real {
                        let in_key = keys[input];
                        if !calc.has_memo(in_key)
                            && let Some(fv) = pvals.get_by_slot(input)
                        {
                            calc.insert_memo(in_key, fv.clone());
                        }
                    }
                }
                let v = calc.cell_value_inner(si, r, c, true);
                if calc.requested.is_some() || matches!(v, FValue::Array(_) | FValue::Lambda(_)) {
                    calc.reset_partial((si, r, c));
                    continue;
                }
                v
            };

            // Publish value with Release ordering before decrementing consumers' atomic indegree
            unsafe {
                *slots[slot].0.get() = Some(v);
            }
            states[slot].store(2, Ordering::Release);
            local_done += 1;
            if local_done >= 64 {
                let prev = completed_total.fetch_add(local_done, Ordering::Relaxed);
                if let Some(d) = done {
                    d.store(prev + local_done, Ordering::Relaxed);
                }
                local_done = 0;
            }

            // Decrement consumers' atomic indegree; collect newly unlocked real nodes
            newly_ready.clear();
            for &consumer in plan.consumers.get(slot) {
                if plan.waiting[consumer].fetch_sub(1, Ordering::AcqRel) == 1 {
                    if consumer < n_real {
                        newly_ready.push(consumer);
                    } else {
                        // Virtual range node unlocked -> decrement its real consumers
                        for &real_consumer in plan.consumers.get(consumer) {
                            if plan.waiting[real_consumer].fetch_sub(1, Ordering::AcqRel) == 1 {
                                newly_ready.push(real_consumer);
                            }
                        }
                    }
                }
            }

            if newly_ready.is_empty() {
                continue;
            }

            // Fast zero-lock chain continuation when 1 node unlocks or no other worker is idle
            if newly_ready.len() == 1 && local_work.is_empty() {
                local_work.push(newly_ready[0]);
            } else if waiting_workers.load(Ordering::Relaxed) == 0
                && local_work.len() + newly_ready.len() <= WORK_CHUNK
            {
                local_work.extend_from_slice(&newly_ready);
            } else {
                // Keep 1 locally if our stack is empty; share the rest globally to wake idle workers
                if local_work.is_empty()
                    && let Some(first) = newly_ready.pop()
                {
                    local_work.push(first);
                }
                if !newly_ready.is_empty() {
                    let mut q = queue_mu.lock().unwrap();
                    let pushed = newly_ready.len();
                    q.ready.extend_from_slice(&newly_ready);
                    if pushed == 1 {
                        queue_cv.notify_one();
                    } else {
                        queue_cv.notify_all();
                    }
                }
            }
        }
    });

    let completed = completed_total.load(Ordering::Relaxed);
    if let Some(d) = done {
        d.store(completed, Ordering::Relaxed);
    }
    if completed == initial_completed || cancelled(cancel) {
        return initial;
    }

    let mut pending_out = Vec::new();
    let out_values = keys
        .iter()
        .zip(slots)
        .enumerate()
        .map(|(slot, (&(si, row, col), cell))| {
            let v = cell.into_inner().unwrap_or_else(|| {
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

#[allow(dead_code)]
fn _use_packed_key(_: PackedCellKey) {}

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
        let batch =
            evaluate_with_workers(&wb, &keys, None, None, None, ctx, None, Some(&progress), 4)
                .unwrap();
        let pending: Vec<_> = batch.pending.iter().map(|&i| keys[i]).collect();
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
            evaluate_with_workers(
                &wb,
                &keys,
                None,
                Some(&dirty),
                None,
                ctx,
                Some(&flag),
                None,
                4
            )
            .is_none()
        );
        assert_eq!(wb.sheet(0).value(759, 1), Value::Number(2280.));
        recalc_dirty(&mut wb, &deps, &seeds);
        assert_eq!(wb.sheet(1).value(759, 0), Value::Number(28.));
    }

    #[test]
    fn narrow_parallel_chains_complete_all_nodes_without_serial_fallback() {
        let mut wb = Workbook::new_blank();
        let chains = 8u32;
        let depth = 300u32;
        {
            let sh = wb.sheet_mut(0);
            for c in 0..chains {
                let col = crate::col_label(c);
                sh.set(0, c, Cell { value: Value::Number(1.0), formula: None });
                for r in 1..depth {
                    set_formula(sh, r, c, &format!("=IF({col}{r}>0,{col}{r}+1,0)"), FValue::Empty);
                }
            }
        }
        let mut keys: Vec<_> = wb
            .sheet(0)
            .formula_cells()
            .map(|(k, _)| (0, k.row, k.col))
            .collect();
        keys.sort_unstable();
        let ctx = EvalCtx::for_workbook(wb.date_system);
        let batch = evaluate_with_workers(&wb, &keys, None, None, None, ctx, None, None, 4).unwrap();
        assert!(batch.pending.is_empty(), "all narrow chain nodes should complete in parallel");
        for c in 0..chains {
            let slot = keys.binary_search(&(0, depth - 1, c)).unwrap();
            assert_eq!(batch.values[slot].3, FValue::Number(depth as f64));
        }
    }
}
