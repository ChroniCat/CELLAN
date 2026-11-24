#[cfg(test)]
use super::Arg;
use super::fxhash::{FxBuildHasher, FxHashMap};
use super::lookup_cache::{SharedCondColumn, TextDict};
use super::{
    CriteriaKey, ErrKind, FValue, RangeKey,
    functions::{Crit, range_cell_len},
};
use std::collections::VecDeque;
use std::sync::Arc;

const SORTED_BLOCK: usize = 256;
const EXACT_INT_LIMIT: u64 = 1 << 53;

// A single numeric inequality can be answered from two cut points in a sorted column.
// Only exactly summable integer amounts may change summation order.
// Uses 256-entry block prefix sums so value-column delta patching touches O(changed_rows + blocks)
// instead of rewriting an O(N) prefix array!
#[derive(Clone, Debug)]
pub(crate) struct Sorted {
    keys: Arc<Vec<f64>>,
    weights: Option<Arc<Vec<i64>>>,
    present_bits: Option<Arc<Vec<u64>>>,
    block_prefix: Option<Vec<i64>>,
    block_cnt_prefix: Option<Vec<u32>>,
    /// Fast path when every row in `vals` has the identical exact integer `w`:
    /// eliminates 200k random page dereferences and `weights`/`block_prefix` allocations!
    uniform_weight: Option<i64>,
    total: i64,
    total_cnt: u32,
    abs_total: u64,
}

impl Sorted {
    fn compute_block_prefixes(
        weights: &[i64],
        present_bits: &[u64],
        from_block: usize,
        bp: &mut Vec<i64>,
        bc: &mut Vec<u32>,
    ) {
        let m = weights.len();
        let num_blocks = m.div_ceil(SORTED_BLOCK);
        bp.resize(num_blocks + 1, 0);
        bc.resize(num_blocks + 1, 0);
        let start_b = from_block.min(num_blocks);
        let mut run_sum = bp[start_b];
        let mut run_cnt = bc[start_b];
        for b in start_b..num_blocks {
            let s = b * SORTED_BLOCK;
            let e = (s + SORTED_BLOCK).min(m);
            let mut bsum = 0i64;
            for &w in &weights[s..e] {
                bsum += w;
            }
            let sw = b * (SORTED_BLOCK / 64);
            let ew = e / 64;
            let mut bcnt = 0u32;
            for &word in &present_bits[sw..ew] {
                bcnt += word.count_ones();
            }
            let rem = e & 63;
            if rem != 0 {
                bcnt += (present_bits[ew] & ((1u64 << rem) - 1)).count_ones();
            }
            run_sum += bsum;
            run_cnt += bcnt;
            bp[b + 1] = run_sum;
            bc[b + 1] = run_cnt;
        }
    }

    fn build(keys: &[FValue], amounts: Option<&[FValue]>) -> Option<Self> {
        let n = keys.len().min(amounts.map_or(usize::MAX, |v| v.len()));
        let mut rows = Vec::with_capacity(n);
        let mut total = 0i64;
        let mut total_cnt = 0u32;
        let mut absolute = 0u64;
        for (i, key) in keys[..n].iter().enumerate() {
            let (amount, has_num) = match amounts {
                None => (1i64, 1u32),
                Some(v) => match v[i].to_number() {
                    Ok(x) => {
                        if !x.is_finite() || x.fract() != 0.0 || x.abs() > EXACT_INT_LIMIT as f64 {
                            return None;
                        }
                        let weight = x as i64;
                        absolute += weight.unsigned_abs();
                        if absolute > EXACT_INT_LIMIT {
                            return None;
                        }
                        (weight, 1u32)
                    }
                    Err(_) => (0i64, 0u32),
                },
            };
            total += amount;
            total_cnt += has_num;
            if let Ok(value) = key.to_number() {
                if !value.is_finite() {
                    return None;
                }
                rows.push((value, amount, has_num));
            }
        }
        rows.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
        let m = rows.len();
        let mut sorted_keys = Vec::with_capacity(m);
        if amounts.is_none() {
            for (value, _, _) in rows {
                sorted_keys.push(value);
            }
            return Some(Self {
                keys: Arc::new(sorted_keys),
                weights: None,
                present_bits: None,
                block_prefix: None,
                block_cnt_prefix: None,
                uniform_weight: None,
                total,
                total_cnt,
                abs_total: absolute,
            });
        }
        let mut weights = Vec::with_capacity(m);
        let mut present_bits = vec![0u64; m.div_ceil(64)];
        for (i, (value, amount, has_num)) in rows.into_iter().enumerate() {
            sorted_keys.push(value);
            weights.push(amount);
            if has_num != 0 {
                present_bits[i >> 6] |= 1u64 << (i & 63);
            }
        }
        let mut bp = Vec::new();
        let mut bc = Vec::new();
        Self::compute_block_prefixes(&weights, &present_bits, 0, &mut bp, &mut bc);
        Some(Self {
            keys: Arc::new(sorted_keys),
            weights: Some(Arc::new(weights)),
            present_bits: Some(Arc::new(present_bits)),
            block_prefix: Some(bp),
            block_cnt_prefix: Some(bc),
            uniform_weight: None,
            total,
            total_cnt,
            abs_total: absolute,
        })
    }

    /// Zero-sort, zero-`Vec<FValue>` build directly from incrementally maintained `SharedCondColumn`s.
    fn build_from_cond(
        cond: &SharedCondColumn,
        vals: Option<&SharedCondColumn>,
    ) -> Option<Self> {
        let sorted = cond.sorted_nums()?;
        let n = cond.len.min(vals.map_or(usize::MAX, |v| v.len));
        let full_span = vals.is_none_or(|v| v.len == n) && cond.len == n;

        let mut uniform_weight: Option<i64> = None;
        let (total, total_cnt, abs_total) = match vals {
            None => (n as i64, n as u32, n as u64),
            Some(vc) if full_span => {
                let mut tot = 0i64;
                let mut abs_sum = 0u64;
                let mut cnt = 0u32;
                let first_val = vc.pages.first().map(|p| p.min_num);
                let mut all_uniform = first_val.is_some();
                for p in &vc.pages {
                    if !p.all_exact_int {
                        return None;
                    }
                    if all_uniform
                        && (p.num_count != p.rows_in_page
                            || p.min_num != p.max_num
                            || Some(p.min_num) != first_val)
                    {
                        all_uniform = false;
                    }
                    tot = tot.wrapping_add(p.int_sum);
                    abs_sum = abs_sum.saturating_add(p.int_abs_sum);
                    if abs_sum > EXACT_INT_LIMIT {
                        return None;
                    }
                    cnt += p.num_count as u32;
                }
                if all_uniform && let Some(fv) = first_val {
                    uniform_weight = Some(fv as i64);
                }
                (tot, cnt, abs_sum)
            }
            Some(vc) => {
                let mut tot = 0i64;
                let mut abs_sum = 0u64;
                let mut cnt = 0u32;
                for row in 0..n {
                    if let Some(x) = vc.coerced_number_at(row) {
                        if !x.is_finite() || x.fract() != 0.0 || x.abs() > EXACT_INT_LIMIT as f64 {
                            return None;
                        }
                        let w = x as i64;
                        abs_sum = abs_sum.saturating_add(w.unsigned_abs());
                        if abs_sum > EXACT_INT_LIMIT {
                            return None;
                        }
                        tot += w;
                        cnt += 1;
                    }
                }
                (tot, cnt, abs_sum)
            }
        };

        let m = if cond.len == n {
            sorted.len()
        } else {
            sorted.iter().filter(|&&(_, r)| (r as usize) < n).count()
        };
        let mut sorted_keys = Vec::with_capacity(m);
        if vals.is_none() || uniform_weight.is_some() {
            if cond.len == n {
                sorted_keys.extend(sorted.iter().map(|&(k, _)| k));
            } else {
                for &(k, r) in sorted.iter() {
                    if (r as usize) < n {
                        sorted_keys.push(k);
                    }
                }
            }
            return Some(Self {
                keys: Arc::new(sorted_keys),
                weights: None,
                present_bits: None,
                block_prefix: None,
                block_cnt_prefix: None,
                uniform_weight,
                total,
                total_cnt,
                abs_total,
            });
        }
        let vc = vals.unwrap();
        let mut weights = Vec::with_capacity(m);
        let mut present_bits = vec![0u64; m.div_ceil(64)];
        let vr1 = vc.r1;
        let v_first_page = vr1 >> 8;
        let v_pages = vc.pages.as_slice();
        let mut pos = 0usize;
        for &(k, r) in sorted.iter() {
            if (r as usize) >= n {
                continue;
            }
            let abs_row = vr1 + r;
            let p_idx = ((abs_row >> 8) - v_first_page) as usize;
            let page = &v_pages[p_idx];
            let local = (abs_row & 255) as usize - page.start_in_slab as usize;
            if (page.coerced_ok[local >> 6] & (1u64 << (local & 63))) != 0 {
                weights.push(page.coerced_nums[local] as i64);
                present_bits[pos >> 6] |= 1u64 << (pos & 63);
            } else {
                weights.push(0);
            }
            sorted_keys.push(k);
            pos += 1;
        }
        let mut bp = Vec::new();
        let mut bc = Vec::new();
        Self::compute_block_prefixes(&weights, &present_bits, 0, &mut bp, &mut bc);
        Some(Self {
            keys: Arc::new(sorted_keys),
            weights: Some(Arc::new(weights)),
            present_bits: Some(Arc::new(present_bits)),
            block_prefix: Some(bp),
            block_cnt_prefix: Some(bc),
            uniform_weight: None,
            total,
            total_cnt,
            abs_total,
        })
    }

    /// O(changed_rows * log M + blocks) incremental value-column patch when `cond` is unchanged!
    fn try_patch_values_from_cond(
        &self,
        cond: &SharedCondColumn,
        old_vals: &SharedCondColumn,
        new_vals: &SharedCondColumn,
        changed_val_pages: &[usize],
    ) -> Option<Self> {
        let n = cond.len;
        if old_vals.len != n || new_vals.len != n || old_vals.r1 != new_vals.r1 {
            return None;
        }
        if self.uniform_weight.is_some() {
            // If previously uniform, check if still uniform with same weight; else rebuild weights once
            return None;
        }
        let sorted = cond.sorted_nums()?;
        if sorted.len() != self.keys.len() {
            return None;
        }
        let mut weights_arc = self.weights.clone()?;
        let mut bits_arc = self.present_bits.clone()?;
        let mut bp = self.block_prefix.clone()?;
        let mut bc = self.block_cnt_prefix.clone()?;
        let mut total = self.total;
        let mut total_cnt = self.total_cnt;
        let mut abs_total = self.abs_total;
        let mut min_dirty_pos = usize::MAX;
        let mut any_row_diff = false;

        for &p_idx in changed_val_pages {
            let new_p = new_vals.pages.get(p_idx)?;
            let old_p = old_vals.pages.get(p_idx)?;
            if !new_p.all_exact_int || new_p.rows_in_page != old_p.rows_in_page {
                return None;
            }
            let base_row = new_vals.page_offsets[p_idx] as usize;
            for local in 0..new_p.rows_in_page as usize {
                let old_has = (old_p.coerced_ok[local >> 6] & (1u64 << (local & 63))) != 0;
                let new_has = (new_p.coerced_ok[local >> 6] & (1u64 << (local & 63))) != 0;
                let old_w = if old_has { old_p.coerced_nums[local] as i64 } else { 0 };
                let new_w = if new_has { new_p.coerced_nums[local] as i64 } else { 0 };
                if old_has == new_has && old_w == new_w {
                    continue;
                }
                total = total.wrapping_add(new_w - old_w);
                total_cnt = total_cnt
                    .wrapping_add(u32::from(new_has))
                    .wrapping_sub(u32::from(old_has));
                abs_total = abs_total
                    .saturating_sub(old_w.unsigned_abs())
                    .saturating_add(new_w.unsigned_abs());
                if abs_total > EXACT_INT_LIMIT {
                    return None;
                }
                let row = base_row + local;
                if let Some(k) = cond.coerced_number_at(row) {
                    let row_u32 = row as u32;
                    let pos = sorted
                        .binary_search_by(|&(sk, sr)| sk.total_cmp(&k).then_with(|| sr.cmp(&row_u32)))
                        .ok()?;
                    Arc::make_mut(&mut weights_arc)[pos] = new_w;
                    let bits = Arc::make_mut(&mut bits_arc);
                    if new_has {
                        bits[pos >> 6] |= 1u64 << (pos & 63);
                    } else {
                        bits[pos >> 6] &= !(1u64 << (pos & 63));
                    }
                    min_dirty_pos = min_dirty_pos.min(pos);
                    any_row_diff = true;
                }
            }
        }

        if any_row_diff {
            Self::compute_block_prefixes(
                &weights_arc,
                &bits_arc,
                min_dirty_pos / SORTED_BLOCK,
                &mut bp,
                &mut bc,
            );
        }

        Some(Self {
            keys: self.keys.clone(),
            weights: Some(weights_arc),
            present_bits: Some(bits_arc),
            block_prefix: Some(bp),
            block_cnt_prefix: Some(bc),
            uniform_weight: None,
            total,
            total_cnt,
            abs_total,
        })
    }

    #[cfg(test)]
    fn query(&self, op: &str, n: f64) -> FValue {
        self.query_fn(0, op, n)
    }

    #[inline]
    fn sum_at(&self, pos: usize) -> i64 {
        if let Some(w) = self.uniform_weight {
            return pos as i64 * w;
        }
        let (Some(bp), Some(ws)) = (&self.block_prefix, &self.weights) else {
            return pos as i64;
        };
        let blk = pos / SORTED_BLOCK;
        let mut s = bp[blk];
        for &w in &ws[blk * SORTED_BLOCK..pos] {
            s += w;
        }
        s
    }

    #[inline]
    fn cnt_at(&self, pos: usize) -> u32 {
        if self.uniform_weight.is_some() {
            return pos as u32;
        }
        let (Some(bc), Some(pb)) = (&self.block_cnt_prefix, &self.present_bits) else {
            return pos as u32;
        };
        let blk = pos / SORTED_BLOCK;
        let mut c = bc[blk];
        let start_word = blk * (SORTED_BLOCK / 64);
        let end_word = pos >> 6;
        for &word in &pb[start_word..end_word] {
            c += word.count_ones();
        }
        let rem = pos & 63;
        if rem != 0 {
            c += (pb[end_word] & ((1u64 << rem) - 1)).count_ones();
        }
        c
    }

    fn query_fn(&self, function: u8, op: &str, n: f64) -> FValue {
        let m = self.keys.len();
        let numeric_sum = self.sum_at(m);
        let numeric_cnt = self.cnt_at(m);
        let (sum, cnt) = match op {
            "<" => {
                let p = self.keys.partition_point(|x| *x < n);
                (self.sum_at(p), self.cnt_at(p))
            }
            "<=" => {
                let p = self.keys.partition_point(|x| *x <= n);
                (self.sum_at(p), self.cnt_at(p))
            }
            ">" => {
                let p = self.keys.partition_point(|x| *x <= n);
                (numeric_sum - self.sum_at(p), numeric_cnt - self.cnt_at(p))
            }
            ">=" => {
                let p = self.keys.partition_point(|x| *x < n);
                (numeric_sum - self.sum_at(p), numeric_cnt - self.cnt_at(p))
            }
            "<>" => {
                let lo = self.keys.partition_point(|x| x - n < -f64::EPSILON);
                let hi = self.keys.partition_point(|x| x - n <= f64::EPSILON);
                (
                    self.total - (self.sum_at(hi) - self.sum_at(lo)),
                    self.total_cnt - (self.cnt_at(hi) - self.cnt_at(lo)),
                )
            }
            _ => unreachable!(),
        };
        match function {
            2 | 3 => {
                if cnt == 0 {
                    FValue::Error(ErrKind::Div0)
                } else {
                    FValue::number(sum as f64 / cnt as f64)
                }
            }
            _ => FValue::number(sum as f64),
        }
    }
}

#[derive(Clone, Debug)]
struct MinMaxOrdered {
    keys: Vec<f64>,
    prefix_min: Vec<f64>,
    suffix_min: Vec<f64>,
    prefix_max: Vec<f64>,
    suffix_max: Vec<f64>,
    non_key_min: f64,
    non_key_max: f64,
}

impl MinMaxOrdered {
    fn build(keys: &[FValue], amounts: &[FValue]) -> Option<Self> {
        let n = keys.len().min(amounts.len());
        let mut rows: Vec<(f64, Option<f64>)> = Vec::with_capacity(n);
        let mut non_key_min = f64::INFINITY;
        let mut non_key_max = f64::NEG_INFINITY;
        for i in 0..n {
            let val = match amounts[i].to_number() {
                Ok(v) => {
                    if !v.is_finite() {
                        return None;
                    }
                    Some(v)
                }
                Err(_) => None,
            };
            match keys[i].to_number() {
                Ok(k) => {
                    if !k.is_finite() {
                        return None;
                    }
                    rows.push((k, val));
                }
                Err(_) => {
                    if let Some(v) = val {
                        non_key_min = non_key_min.min(v);
                        non_key_max = non_key_max.max(v);
                    }
                }
            }
        }
        rows.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
        Some(Self::finish_from_sorted(rows, non_key_min, non_key_max))
    }

    fn build_from_cond(cond: &SharedCondColumn, vals: &SharedCondColumn) -> Option<Self> {
        if vals.has_non_finite {
            return None;
        }
        let sorted = cond.sorted_nums()?;
        let n = cond.len.min(vals.len);
        let mut non_key_min = f64::INFINITY;
        let mut non_key_max = f64::NEG_INFINITY;
        if cond.has_non_num {
            for (page, &base) in cond.pages.iter().zip(cond.page_offsets.iter()) {
                if !page.has_non_num {
                    continue;
                }
                for local in 0..page.rows_in_page as usize {
                    let row = base as usize + local;
                    if row >= n {
                        break;
                    }
                    if !page.has_coerced_num(local)
                        && let Some(v) = vals.coerced_number_at(row)
                    {
                        non_key_min = non_key_min.min(v);
                        non_key_max = non_key_max.max(v);
                    }
                }
            }
        }
        let mut rows: Vec<(f64, Option<f64>)> = Vec::with_capacity(sorted.len().min(n));
        for &(k, r) in sorted.iter() {
            if (r as usize) >= n {
                continue;
            }
            rows.push((k, vals.coerced_number_at(r as usize)));
        }
        Some(Self::finish_from_sorted(rows, non_key_min, non_key_max))
    }

    fn finish_from_sorted(
        rows: Vec<(f64, Option<f64>)>,
        non_key_min: f64,
        non_key_max: f64,
    ) -> Self {
        let m = rows.len();
        let mut prefix_min = vec![f64::INFINITY; m + 1];
        let mut prefix_max = vec![f64::NEG_INFINITY; m + 1];
        for (i, &(_, val)) in rows.iter().enumerate() {
            prefix_min[i + 1] = match val {
                Some(v) => prefix_min[i].min(v),
                None => prefix_min[i],
            };
            prefix_max[i + 1] = match val {
                Some(v) => prefix_max[i].max(v),
                None => prefix_max[i],
            };
        }
        let mut suffix_min = vec![f64::INFINITY; m + 1];
        let mut suffix_max = vec![f64::NEG_INFINITY; m + 1];
        for (i, &(_, val)) in rows.iter().enumerate().rev() {
            suffix_min[i] = match val {
                Some(v) => suffix_min[i + 1].min(v),
                None => suffix_min[i + 1],
            };
            suffix_max[i] = match val {
                Some(v) => suffix_max[i + 1].max(v),
                None => suffix_max[i + 1],
            };
        }
        let sorted_keys = rows.into_iter().map(|(k, _)| k).collect();
        Self {
            keys: sorted_keys,
            prefix_min,
            suffix_min,
            prefix_max,
            suffix_max,
            non_key_min,
            non_key_max,
        }
    }

    fn query(&self, is_max: bool, op: &str, n: f64) -> FValue {
        if !is_max {
            let best = match op {
                "<" => self.prefix_min[self.keys.partition_point(|x| *x < n)],
                "<=" => self.prefix_min[self.keys.partition_point(|x| *x <= n)],
                ">" => self.suffix_min[self.keys.partition_point(|x| *x <= n)],
                ">=" => self.suffix_min[self.keys.partition_point(|x| *x < n)],
                "<>" => {
                    let lo = self.keys.partition_point(|x| x - n < -f64::EPSILON);
                    let hi = self.keys.partition_point(|x| x - n <= f64::EPSILON);
                    self.prefix_min[lo].min(self.suffix_min[hi]).min(self.non_key_min)
                }
                _ => f64::INFINITY,
            };
            FValue::number(if best.is_infinite() { 0.0 } else { best })
        } else {
            let best = match op {
                "<" => self.prefix_max[self.keys.partition_point(|x| *x < n)],
                "<=" => self.prefix_max[self.keys.partition_point(|x| *x <= n)],
                ">" => self.suffix_max[self.keys.partition_point(|x| *x <= n)],
                ">=" => self.suffix_max[self.keys.partition_point(|x| *x < n)],
                "<>" => {
                    let lo = self.keys.partition_point(|x| x - n < -f64::EPSILON);
                    let hi = self.keys.partition_point(|x| x - n <= f64::EPSILON);
                    self.prefix_max[lo].max(self.suffix_max[hi]).max(self.non_key_max)
                }
                _ => f64::NEG_INFINITY,
            };
            FValue::number(if best.is_infinite() { 0.0 } else { best })
        }
    }
}

const BLOCK_ROWS: usize = 256;

#[derive(Clone, Copy, Debug)]
struct BlockBounds {
    min_key: f64,
    max_key: f64,
    any_num_key: bool,
    has_non_num_key: bool,
}

/// Preserves exact IEEE-754 source-order summation for fractional SUMIFS/AVERAGEIF/AVERAGEIFS
/// while skipping non-matching 256-row blocks in O(1) and avoiding repeated FValue coercions.
#[derive(Clone, Debug)]
struct PagedOrdered {
    key_nums: Arc<Vec<f64>>,
    val_nums: Arc<Vec<f64>>,
    blocks: Arc<Vec<BlockBounds>>,
}

impl PagedOrdered {
    fn build(keys: &[FValue], amounts: &[FValue]) -> Option<Self> {
        let n = keys.len().min(amounts.len());
        let mut key_nums = Vec::with_capacity(n);
        let mut val_nums = Vec::with_capacity(n);
        let mut blocks = Vec::with_capacity(n.div_ceil(BLOCK_ROWS));
        for chunk_start in (0..n).step_by(BLOCK_ROWS) {
            let chunk_end = (chunk_start + BLOCK_ROWS).min(n);
            let mut min_key = f64::INFINITY;
            let mut max_key = f64::NEG_INFINITY;
            let mut any_num_key = false;
            let mut has_non_num_key = false;
            for i in chunk_start..chunk_end {
                match keys[i].to_number() {
                    Ok(k) => {
                        if !k.is_finite() {
                            return None;
                        }
                        min_key = min_key.min(k);
                        max_key = max_key.max(k);
                        any_num_key = true;
                        key_nums.push(k);
                    }
                    Err(_) => {
                        has_non_num_key = true;
                        key_nums.push(f64::NAN);
                    }
                }
                match amounts[i].to_number() {
                    Ok(v) => {
                        if !v.is_finite() {
                            return None;
                        }
                        val_nums.push(v);
                    }
                    Err(_) => val_nums.push(f64::NAN),
                }
            }
            blocks.push(BlockBounds {
                min_key,
                max_key,
                any_num_key,
                has_non_num_key,
            });
        }
        Some(Self {
            key_nums: Arc::new(key_nums),
            val_nums: Arc::new(val_nums),
            blocks: Arc::new(blocks),
        })
    }

    fn build_from_cond(cond: &SharedCondColumn, vals: &SharedCondColumn) -> Option<Self> {
        if cond.has_non_finite || vals.has_non_finite {
            return None;
        }
        let n = cond.len.min(vals.len);
        let mut key_nums = Vec::with_capacity(n);
        let mut val_nums = Vec::with_capacity(n);
        let mut blocks = Vec::with_capacity(n.div_ceil(BLOCK_ROWS));
        for chunk_start in (0..n).step_by(BLOCK_ROWS) {
            let chunk_end = (chunk_start + BLOCK_ROWS).min(n);
            let mut min_key = f64::INFINITY;
            let mut max_key = f64::NEG_INFINITY;
            let mut any_num_key = false;
            let mut has_non_num_key = false;
            for row in chunk_start..chunk_end {
                match cond.coerced_number_at(row) {
                    Some(k) => {
                        min_key = min_key.min(k);
                        max_key = max_key.max(k);
                        any_num_key = true;
                        key_nums.push(k);
                    }
                    None => {
                        has_non_num_key = true;
                        key_nums.push(f64::NAN);
                    }
                }
                match vals.coerced_number_at(row) {
                    Some(v) => val_nums.push(v),
                    None => val_nums.push(f64::NAN),
                }
            }
            blocks.push(BlockBounds {
                min_key,
                max_key,
                any_num_key,
                has_non_num_key,
            });
        }
        Some(Self {
            key_nums: Arc::new(key_nums),
            val_nums: Arc::new(val_nums),
            blocks: Arc::new(blocks),
        })
    }

    fn try_patch_values_from_cond(
        &self,
        cond: &SharedCondColumn,
        old_vals: &SharedCondColumn,
        new_vals: &SharedCondColumn,
        changed_val_pages: &[usize],
    ) -> Option<Self> {
        if cond.has_non_finite || new_vals.has_non_finite {
            return None;
        }
        let n = cond.len.min(new_vals.len);
        if n != self.val_nums.len() || old_vals.len.min(cond.len) != n {
            return None;
        }
        let mut val_nums_arc = self.val_nums.clone();
        let vals_mut = Arc::make_mut(&mut val_nums_arc);
        for &p_idx in changed_val_pages {
            let new_p = new_vals.pages.get(p_idx)?;
            let base_row = new_vals.page_offsets[p_idx] as usize;
            let end_row = (base_row + new_p.rows_in_page as usize).min(n);
            for row in base_row..end_row {
                let local = row - base_row;
                vals_mut[row] = if (new_p.coerced_ok[local >> 6] & (1u64 << (local & 63))) != 0 {
                    new_p.coerced_nums[local]
                } else {
                    f64::NAN
                };
            }
        }
        Some(Self {
            key_nums: self.key_nums.clone(),
            val_nums: val_nums_arc,
            blocks: self.blocks.clone(),
        })
    }

    fn query(&self, function: u8, op: &str, target: f64) -> FValue {
        let n = self.key_nums.len();
        let mut total = 0.0f64;
        let mut hits = 0usize;
        for (bi, b) in self.blocks.iter().enumerate() {
            let (none_match, all_match) = match op {
                ">" => (
                    !b.any_num_key || b.max_key <= target,
                    !b.has_non_num_key && b.any_num_key && b.min_key > target,
                ),
                ">=" => (
                    !b.any_num_key || b.max_key < target,
                    !b.has_non_num_key && b.any_num_key && b.min_key >= target,
                ),
                "<" => (
                    !b.any_num_key || b.min_key >= target,
                    !b.has_non_num_key && b.any_num_key && b.max_key < target,
                ),
                "<=" => (
                    !b.any_num_key || b.min_key > target,
                    !b.has_non_num_key && b.any_num_key && b.max_key <= target,
                ),
                "<>" => (
                    !b.has_non_num_key
                        && b.any_num_key
                        && b.min_key - target >= -f64::EPSILON
                        && b.max_key - target <= f64::EPSILON,
                    !b.any_num_key
                        || b.max_key - target < -f64::EPSILON
                        || b.min_key - target > f64::EPSILON,
                ),
                _ => (false, false),
            };
            if none_match {
                continue;
            }
            let start = bi * BLOCK_ROWS;
            let end = (start + BLOCK_ROWS).min(n);
            if all_match {
                for &v in &self.val_nums[start..end] {
                    if !v.is_nan() {
                        total += v;
                        hits += 1;
                    }
                }
            } else {
                for i in start..end {
                    let k = self.key_nums[i];
                    let matched = if k.is_nan() {
                        op == "<>"
                    } else {
                        match op {
                            ">" => k > target,
                            ">=" => k >= target,
                            "<" => k < target,
                            "<=" => k <= target,
                            _ => (k - target).abs() > f64::EPSILON,
                        }
                    };
                    if matched {
                        let v = self.val_nums[i];
                        if !v.is_nan() {
                            total += v;
                            hits += 1;
                        }
                    }
                }
            }
        }
        match function {
            2 | 3 => {
                if hits == 0 {
                    FValue::Error(ErrKind::Div0)
                } else {
                    FValue::number(total / hits as f64)
                }
            }
            _ => FValue::number(total),
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct BuiltOrdered {
    len: usize,
    exact: Option<Sorted>,
    minmax: Option<MinMaxOrdered>,
    paged: Option<PagedOrdered>,
}

impl BuiltOrdered {
    /// Incrementally patch `BuiltOrdered` in O(changed_rows * log M + blocks) when `cond` is
    /// unchanged (`Arc::ptr_eq`) and only `<= 4` pages of `vals` changed!
    pub(crate) fn try_patch_values_from_cond(
        &self,
        cond: &SharedCondColumn,
        old_vals: &SharedCondColumn,
        new_vals: &SharedCondColumn,
        need_minmax: bool,
    ) -> Option<Self> {
        if need_minmax || old_vals.pages.len() != new_vals.pages.len() {
            return None;
        }
        let mut changed_val_pages = Vec::new();
        for (idx, (op, np)) in old_vals.pages.iter().zip(new_vals.pages.iter()).enumerate() {
            if !Arc::ptr_eq(op, np) {
                changed_val_pages.push(idx);
                if changed_val_pages.len() > 4 {
                    return None;
                }
            }
        }
        if changed_val_pages.is_empty() {
            return Some(self.clone());
        }
        if let Some(ref ex) = self.exact
            && let Some(patched_ex) =
                ex.try_patch_values_from_cond(cond, old_vals, new_vals, &changed_val_pages)
        {
            return Some(Self {
                len: self.len,
                exact: Some(patched_ex),
                minmax: None,
                paged: None,
            });
        }
        if let Some(ref pg) = self.paged
            && let Some(patched_pg) =
                pg.try_patch_values_from_cond(cond, old_vals, new_vals, &changed_val_pages)
        {
            return Some(Self {
                len: self.len,
                exact: None,
                minmax: None,
                paged: Some(patched_pg),
            });
        }
        None
    }
    fn build(keys: &[FValue], amounts: Option<&[FValue]>, need_minmax: bool) -> Option<Self> {
        let len = keys.len().min(amounts.map_or(usize::MAX, |v| v.len()));
        if amounts.is_none() {
            let exact = Sorted::build(keys, None)?;
            return Some(Self {
                len,
                exact: Some(exact),
                minmax: None,
                paged: None,
            });
        }
        let amounts_slice = amounts.unwrap();
        let exact = if !need_minmax {
            Sorted::build(keys, Some(amounts_slice))
        } else {
            None
        };
        let minmax = if need_minmax {
            MinMaxOrdered::build(keys, amounts_slice)
        } else {
            None
        };
        let paged = if !need_minmax && exact.is_none() {
            PagedOrdered::build(keys, amounts_slice)
        } else {
            None
        };
        if exact.is_none() && minmax.is_none() && paged.is_none() {
            return None;
        }
        Some(Self {
            len,
            exact,
            minmax,
            paged,
        })
    }

    pub(crate) fn build_from_cond(
        cond: &SharedCondColumn,
        vals: Option<&SharedCondColumn>,
        need_minmax: bool,
    ) -> Option<Self> {
        let len = cond.len.min(vals.map_or(usize::MAX, |v| v.len));
        if vals.is_none() {
            let exact = Sorted::build_from_cond(cond, None)?;
            return Some(Self {
                len,
                exact: Some(exact),
                minmax: None,
                paged: None,
            });
        }
        let vc = vals.unwrap();
        let exact = if !need_minmax {
            Sorted::build_from_cond(cond, Some(vc))
        } else {
            None
        };
        let minmax = if need_minmax {
            MinMaxOrdered::build_from_cond(cond, vc)
        } else {
            None
        };
        let paged = if !need_minmax && exact.is_none() {
            PagedOrdered::build_from_cond(cond, vc)
        } else {
            None
        };
        if exact.is_none() && minmax.is_none() && paged.is_none() {
            return None;
        }
        Some(Self {
            len,
            exact,
            minmax,
            paged,
        })
    }

    pub(crate) fn supports(&self, function: u8) -> bool {
        match function {
            1 => self.exact.is_some(),
            4 | 5 => self.minmax.is_some(),
            0 | 2 | 3 => self.exact.is_some() || self.paged.is_some(),
            _ => false,
        }
    }

    pub(crate) fn query(&self, function: u8, op: &str, n: f64) -> Option<FValue> {
        match function {
            1 => self.exact.as_ref().map(|s| s.query_fn(1, op, n)),
            4 => self.minmax.as_ref().map(|m| m.query(false, op, n)),
            5 => self.minmax.as_ref().map(|m| m.query(true, op, n)),
            0 | 2 | 3 => {
                if let Some(ref s) = self.exact {
                    Some(s.query_fn(function, op, n))
                } else {
                    self.paged.as_ref().map(|p| p.query(function, op, n))
                }
            }
            _ => None,
        }
    }
}

struct Entry {
    val_range: Option<RangeKey>,
    cond_range: RangeKey,
    need_minmax: bool,
    attempted: bool,
    ordered: Option<Arc<BuiltOrdered>>,
}

#[derive(Default)]
pub(super) struct OrderedCriteria(VecDeque<Entry>);

impl OrderedCriteria {
    pub(super) fn lookup_lazy(
        &mut self,
        key: &CriteriaKey,
        shared_get: &mut dyn FnMut(Option<RangeKey>, RangeKey) -> Option<Arc<BuiltOrdered>>,
        shared_put: &mut dyn FnMut(Option<RangeKey>, RangeKey, Arc<BuiltOrdered>),
        shared_cond_warm: &mut dyn FnMut(RangeKey) -> bool,
        shared_build_from_cond: &mut dyn FnMut(Option<RangeKey>, RangeKey, bool) -> Option<Arc<BuiltOrdered>>,
        load_col: &mut dyn FnMut(RangeKey) -> Option<Arc<Vec<FValue>>>,
    ) -> Option<FValue> {
        const LIMIT: usize = 1_048_576;
        if key.criteria.len() != 1 {
            return None;
        }
        let (val_range, cond_ranges) = key.split_ranges()?;
        if cond_ranges.len() != 1 {
            return None;
        }
        let cond_range = cond_ranges[0];
        let min_len = val_range
            .map_or(usize::MAX, range_cell_len)
            .min(range_cell_len(cond_range));
        if !(256..=LIMIT).contains(&min_len) {
            return None;
        }
        let (op, number) = Crit::parse(&key.criteria[0]).numeric_bounds()?;
        let need_minmax = matches!(key.function, 4 | 5);

        if let Some(shared) = shared_get(val_range, cond_range)
            && shared.supports(key.function)
        {
            return shared.query(key.function, op, number);
        }

        // Fast path: if `SharedCondColumn` is already warm on the sheet (e.g. after a single-cell
        // edit to a 200k table), build `BuiltOrdered` directly on query #0 via single-flight O(N)
        // incremental merge + zero sorting + zero `Vec<FValue>` materialization!
        let pos = self
            .0
            .iter()
            .position(|e| {
                e.val_range == val_range
                    && e.cond_range == cond_range
                    && e.need_minmax == need_minmax
            });

        let mut entry = match pos {
            Some(i) => self.0.remove(i)?,
            None => {
                let warm = shared_cond_warm(cond_range)
                    && val_range.is_none_or(|vr| shared_cond_warm(vr));
                if !warm {
                    if self.0.len() == 8 {
                        self.0.pop_front();
                    }
                    self.0.push_back(Entry {
                        val_range,
                        cond_range,
                        need_minmax,
                        attempted: false,
                        ordered: None,
                    });
                    return None;
                }
                Entry {
                    val_range,
                    cond_range,
                    need_minmax,
                    attempted: false,
                    ordered: None,
                }
            }
        };

        if !entry.attempted {
            // Try zero-sort, zero-materialization single-flight build via `SharedCondColumn` first!
            let built = if let Some(b) = shared_build_from_cond(val_range, cond_range, need_minmax)
            {
                Some(b)
            } else {
                let Some(keys_col) = load_col(cond_range) else {
                    self.0.push_back(entry);
                    return None;
                };
                let amounts_col = match val_range {
                    Some(vr) => {
                        let Some(c) = load_col(vr) else {
                            self.0.push_back(entry);
                            return None;
                        };
                        Some(c)
                    }
                    None => None,
                };
                BuiltOrdered::build(
                    &keys_col,
                    amounts_col.as_deref().map(Vec::as_slice),
                    need_minmax,
                )
                .map(Arc::new)
            };

            entry.attempted = true;
            while self
                .0
                .iter()
                .filter_map(|e| e.ordered.as_ref())
                .map(|s| s.len)
                .sum::<usize>()
                + min_len
                > LIMIT * 2
            {
                if self.0.pop_front().is_none() {
                    break;
                }
            }
            if let Some(ref b) = built {
                shared_put(val_range, cond_range, b.clone());
            }
            entry.ordered = built;
        }
        let answer = entry
            .ordered
            .as_ref()
            .and_then(|o| o.query(key.function, op, number));
        self.0.push_back(entry);
        answer
    }
}

/// Predicates on the same input column share row positions across aggregates.
/// Intersections preserve source order, including for non-associative floating sums.
#[derive(Default)]
pub(super) struct MatchIndexes {
    columns: VecDeque<ColumnIndex>,
    masks: VecDeque<(RangeKey, String, Arc<[u64]>)>,
}

#[derive(Clone)]
pub(crate) struct ColumnIndex {
    pub(crate) range: RangeKey,
    pub(crate) len: usize,
    pub(crate) numbers: Option<Arc<Vec<(f64, u32)>>>,
    pub(crate) text: Option<Arc<TextDict>>,
    pub(crate) tried_numbers: bool,
    pub(crate) tried_text: bool,
    pub(crate) shared_col: Option<Arc<SharedCondColumn>>,
}

impl ColumnIndex {
    pub(crate) fn new_empty(range: RangeKey, len: usize) -> Self {
        Self {
            range,
            len,
            numbers: None,
            text: None,
            tried_numbers: false,
            tried_text: false,
            shared_col: None,
        }
    }

    pub(crate) fn from_shared(range: RangeKey, sc: Arc<SharedCondColumn>) -> Self {
        Self {
            range,
            len: sc.len,
            numbers: None,
            text: None,
            tried_numbers: false,
            tried_text: false,
            shared_col: Some(sc),
        }
    }

    fn ensure_numbers_from_shared(&mut self) -> bool {
        if self.tried_numbers {
            return self.numbers.is_some();
        }
        if let Some(sc) = &self.shared_col {
            self.tried_numbers = true;
            self.numbers = sc.sorted_nums();
            return self.numbers.is_some();
        }
        false
    }

    fn ensure_text_from_shared(&mut self) -> bool {
        if self.tried_text {
            return self.text.is_some();
        }
        if let Some(sc) = &self.shared_col {
            self.tried_text = true;
            self.text = sc.text_dict();
            return self.text.is_some();
        }
        false
    }

    fn ensure_numbers(&mut self, input: &[FValue]) {
        if self.tried_numbers {
            return;
        }
        if self.ensure_numbers_from_shared() {
            return;
        }
        self.tried_numbers = true;
        let mut sorted = Vec::with_capacity(input.len());
        let mut finite = true;
        for (i, value) in input.iter().enumerate() {
            if let Ok(v) = value.to_number() {
                if !v.is_finite() {
                    finite = false;
                    break;
                }
                sorted.push((v, i as u32));
            }
        }
        if finite {
            sorted.sort_unstable_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
            self.numbers = Some(Arc::new(sorted));
        }
    }

    fn ensure_text(&mut self, input: &[FValue]) {
        if self.tried_text {
            return;
        }
        if self.ensure_text_from_shared() {
            return;
        }
        self.tried_text = true;
        let mut raw: FxHashMap<Arc<str>, Vec<u32>> = FxHashMap::default();
        let mut bytes = 0usize;
        for (i, v) in input.iter().enumerate() {
            let text: Arc<str> = match v {
                FValue::Text(t) => super::lookup_cache::lowercase_arc(t),
                FValue::Bool(true) => "true".into(),
                FValue::Bool(false) => "false".into(),
                FValue::Empty => "".into(),
                _ => super::lookup_cache::lowercase_str(&v.display()),
            };
            if !raw.contains_key(&text) {
                bytes += text.len();
            }
            raw.entry(text).or_default().push(i as u32);
            if raw.len() > 65_536 || bytes > 8 * 1024 * 1024 {
                break;
            }
        }
        if raw.len() <= 65_536 && bytes <= 8 * 1024 * 1024 {
            let mut dictionary: TextDict =
                FxHashMap::with_capacity_and_hasher(raw.len(), FxBuildHasher);
            for (k, v) in raw {
                dictionary.insert(k, Arc::new(v));
            }
            self.text = Some(Arc::new(dictionary));
        }
    }

    fn exact_rows_count(&self, crit: &Crit) -> Option<usize> {
        if !crit.indexable() {
            return None;
        }
        if let Some(n) = crit.num {
            if !n.is_finite() {
                return Some(0);
            }
            let sorted = self.numbers.as_ref()?;
            let lo = sorted.partition_point(|x| x.0 - n < -f64::EPSILON);
            let hi = sorted.partition_point(|x| x.0 - n <= f64::EPSILON);
            Some(hi - lo)
        } else {
            let words = self.text.as_ref()?;
            Some(words.get(crit.text.as_str()).map_or(0, |v| v.len()))
        }
    }

    fn collect_exact_rows_sorted(&self, crit: &Crit, max_row: usize) -> Option<Vec<u32>> {
        if !crit.indexable() {
            return None;
        }
        if let Some(n) = crit.num {
            if !n.is_finite() {
                return Some(Vec::new());
            }
            let sorted = self.numbers.as_ref()?;
            let lo = sorted.partition_point(|x| x.0 - n < -f64::EPSILON);
            let hi = sorted.partition_point(|x| x.0 - n <= f64::EPSILON);
            let slice = &sorted[lo..hi];
            let mut rows: Vec<u32> = slice
                .iter()
                .map(|&(_, r)| r)
                .filter(|&r| (r as usize) < max_row)
                .collect();
            if slice.first().map(|v| v.0.to_bits()) != slice.last().map(|v| v.0.to_bits()) {
                rows.sort_unstable();
            }
            Some(rows)
        } else {
            let words = self.text.as_ref()?;
            let Some(list) = words.get(crit.text.as_str()) else {
                return Some(Vec::new());
            };
            let end = list.partition_point(|&r| (r as usize) < max_row);
            Some(list[..end].to_vec())
        }
    }

    /// Try marking directly from already-built (or shared-page) indexes without materializing `Vec<FValue>`.
    fn try_mark_indexed(&mut self, criterion: &Crit, bits: &mut [u64]) -> bool {
        let mut set = |row: u32| bits[row as usize / 64] |= 1 << (row % 64);
        if let Some((op, n)) = criterion.numeric_query() {
            self.ensure_numbers_from_shared();
            if n.is_finite()
                && let Some(sorted) = &self.numbers
            {
                let eq = || {
                    sorted.partition_point(|x| x.0 - n < -f64::EPSILON)
                        ..sorted.partition_point(|x| x.0 - n <= f64::EPSILON)
                };
                let hits = match op {
                    ">" => sorted.partition_point(|x| x.0 <= n)..sorted.len(),
                    ">=" => sorted.partition_point(|x| x.0 < n)..sorted.len(),
                    "<" => 0..sorted.partition_point(|x| x.0 < n),
                    "<=" => 0..sorted.partition_point(|x| x.0 <= n),
                    _ => eq(),
                };
                for &(_, row) in &sorted[hits] {
                    set(row);
                }
                if op == "<>" {
                    for word in bits {
                        *word = !*word;
                    }
                }
                return true;
            }
            false
        } else {
            self.ensure_text_from_shared();
            if let Some(words) = &self.text {
                if !criterion.wild && criterion.op == "=" {
                    if let Some(rows) = words.get(criterion.text.as_str()) {
                        for &row in rows.iter() {
                            set(row);
                        }
                    }
                    return true;
                }
                if !criterion.wild && criterion.op == "<>" {
                    if let Some(rows) = words.get(criterion.text.as_str()) {
                        for &row in rows.iter() {
                            set(row);
                        }
                    }
                    for word in bits {
                        *word = !*word;
                    }
                    return true;
                }
                for (word, rows) in words.iter() {
                    if criterion.matches_coerced_text(word) {
                        for &row in rows.iter() {
                            set(row);
                        }
                    }
                }
                return true;
            }
            false
        }
    }

    fn mark(&mut self, input: &[FValue], criterion: &Crit, bits: &mut [u64]) {
        if self.try_mark_indexed(criterion, bits) {
            return;
        }
        if criterion.numeric_query().is_some() {
            self.ensure_numbers(input);
        } else {
            self.ensure_text(input);
        }
        if self.try_mark_indexed(criterion, bits) {
            return;
        }
        let mut set = |row: u32| bits[row as usize / 64] |= 1 << (row % 64);
        for (i, v) in input.iter().enumerate() {
            if criterion.matches_coerced(v) {
                set(i as u32);
            }
        }
    }
}

pub(super) trait CriteriaDataSource {
    fn load_col(&mut self, rk: RangeKey) -> Option<Arc<Vec<FValue>>>;
    fn probe_cell(&mut self, rk: RangeKey, row_offset: usize) -> Option<FValue>;
    fn shared_cond_col(&mut self, rk: RangeKey) -> Option<Arc<SharedCondColumn>>;
}

impl MatchIndexes {
    fn put_column(&mut self, column: ColumnIndex) {
        let len = column.len;
        while self.columns.len() >= 16
            || self.columns.iter().map(|c| c.len).sum::<usize>() + len > 4_194_304
        {
            if self.columns.pop_front().is_none() {
                break;
            }
        }
        self.columns.push_back(column);
    }

    #[cfg(test)]
    fn mask(&mut self, range: RangeKey, pattern: &str, input: &[FValue]) -> Arc<[u64]> {
        if let Some((_, _, bits)) = self.masks.iter().find(|(r, p, _)| *r == range && p == pattern) {
            return bits.clone();
        }
        let mut column = self
            .columns
            .iter()
            .position(|c| c.range == range)
            .map(|i| self.columns.remove(i).unwrap())
            .unwrap_or_else(|| ColumnIndex::new_empty(range, input.len()));
        let mut bits = vec![0; input.len().div_ceil(64)];
        column.mark(input, &Crit::parse(pattern), &mut bits);
        self.put_column(column);
        let bits: Arc<[u64]> = bits.into();
        if self.masks.len() >= 64 {
            self.masks.pop_front();
        }
        self.masks.push_back((range, pattern.to_owned(), bits.clone()));
        bits
    }

    fn mask_lazy(
        &mut self,
        range: RangeKey,
        pattern: &str,
        expected_len: usize,
        data_src: &mut dyn CriteriaDataSource,
    ) -> Option<Arc<[u64]>> {
        if let Some((_, _, bits)) = self.masks.iter().find(|(r, p, _)| *r == range && p == pattern) {
            return Some(bits.clone());
        }
        let crit = Crit::parse(pattern);
        let mut column = self
            .columns
            .iter()
            .position(|c| c.range == range)
            .map(|i| self.columns.remove(i).unwrap())
            .unwrap_or_else(|| ColumnIndex::new_empty(range, expected_len));
        let mut bits = vec![0u64; column.len.div_ceil(64)];
        if !column.try_mark_indexed(&crit, &mut bits) {
            let col_data = data_src.load_col(range)?;
            if bits.len() != col_data.len().div_ceil(64) {
                bits.resize(col_data.len().div_ceil(64), 0);
            }
            column.len = col_data.len();
            column.mark(&col_data, &crit, &mut bits);
        }
        self.put_column(column);
        let bits: Arc<[u64]> = bits.into();
        if self.masks.len() >= 64 {
            self.masks.pop_front();
        }
        self.masks
            .push_back((range, pattern.to_owned(), bits.clone()));
        Some(bits)
    }

    #[cfg(test)]
    pub(super) fn lookup(&mut self, key: &CriteriaKey, args: &[Arg]) -> Option<FValue> {
        let count = key.function == 1;
        let average_if = key.function == 2;
        let pairs = if average_if {
            args.get(..2)?
        } else {
            args.get(usize::from(!count)..)?
        };
        if pairs.is_empty() || pairs.len() % 2 != 0 || pairs.len() / 2 != key.criteria.len() {
            return None;
        }
        let columns = if average_if {
            key.ranges.get(..1)?
        } else {
            key.ranges.get(usize::from(!count)..)?
        };
        if columns.len() != key.criteria.len() {
            return None;
        }
        let values = if average_if {
            args.get(2).unwrap_or(&args[0])
        } else {
            &args[0]
        }
        .slice();
        let len = pairs
            .chunks_exact(2)
            .map(|p| p[0].slice().len())
            .min()?
            .min(values.len());
        if len < 256 || pairs.chunks_exact(2).any(|p| p[0].slice().len() > 2_097_152) {
            return None;
        }
        let masks: Vec<_> = columns
            .iter()
            .zip(&key.criteria)
            .zip(pairs.chunks_exact(2))
            .map(|((&r, p), args)| self.mask(r, p, args[0].slice()))
            .collect();
        Some(Self::eval_masks(key.function, len, &masks, values))
    }

    fn eval_masks(function: u8, len: usize, masks: &[Arc<[u64]>], values: &[FValue]) -> FValue {
        let count = function == 1;
        let mut hits = 0usize;
        let mut total = 0.0;
        let mut best: Option<f64> = None;
        for i in 0..len.div_ceil(64) {
            let mut word = masks.iter().fold(u64::MAX, |v, m| v & m[i]);
            if i == len / 64 && len % 64 != 0 {
                word &= (1u64 << (len % 64)) - 1;
            }
            if count {
                hits += word.count_ones() as usize;
                continue;
            }
            while word != 0 {
                let row = i * 64 + word.trailing_zeros() as usize;
                word &= word - 1;
                if let Ok(n) = values[row].to_number() {
                    hits += 1;
                    match function {
                        4 => best = Some(best.map_or(n, |v| v.min(n))),
                        5 => best = Some(best.map_or(n, |v| v.max(n))),
                        _ => total += n,
                    }
                }
            }
        }
        Self::finish_aggregate(function, hits, total, best)
    }

    fn eval_masks_from_cond(
        function: u8,
        len: usize,
        masks: &[Arc<[u64]>],
        vals: &SharedCondColumn,
    ) -> FValue {
        let mut hits = 0usize;
        let mut total = 0.0;
        let mut best: Option<f64> = None;
        for i in 0..len.div_ceil(64) {
            let mut word = masks.iter().fold(u64::MAX, |v, m| v & m[i]);
            if i == len / 64 && len % 64 != 0 {
                word &= (1u64 << (len % 64)) - 1;
            }
            while word != 0 {
                let row = i * 64 + word.trailing_zeros() as usize;
                word &= word - 1;
                if let Some(n) = vals.coerced_number_at(row) {
                    hits += 1;
                    match function {
                        4 => best = Some(best.map_or(n, |v| v.min(n))),
                        5 => best = Some(best.map_or(n, |v| v.max(n))),
                        _ => total += n,
                    }
                }
            }
        }
        Self::finish_aggregate(function, hits, total, best)
    }

    #[inline]
    fn finish_aggregate(function: u8, hits: usize, total: f64, best: Option<f64>) -> FValue {
        match function {
            1 => FValue::number(hits as f64),
            2 | 3 if hits == 0 => FValue::Error(ErrKind::Div0),
            2 | 3 => FValue::number(total / hits as f64),
            4 | 5 => FValue::number(best.unwrap_or(0.0)),
            _ => FValue::number(total),
        }
    }

    pub(super) fn lookup_lazy(
        &mut self,
        key: &CriteriaKey,
        shared_col_get: &mut dyn FnMut(RangeKey) -> Option<ColumnIndex>,
        shared_col_put: &mut dyn FnMut(&ColumnIndex),
        data_src: &mut dyn CriteriaDataSource,
    ) -> Option<FValue> {
        let (val_range, cond_ranges) = key.split_ranges()?;
        if cond_ranges.len() != key.criteria.len() {
            return None;
        }
        let max_len = cond_ranges.iter().copied().map(range_cell_len).max()?;
        let len = cond_ranges
            .iter()
            .copied()
            .chain(val_range)
            .map(range_cell_len)
            .min()?;
        if len < 256 || max_len > 2_097_152 {
            return None;
        }

        let crits: Vec<Crit> = key.criteria.iter().map(|s| Crit::parse(s)).collect();

        // High-selectivity fast path: probe remaining criteria only at matched rows
        let sparse_limit = (len / 64).max(8).min(4096);
        let mut best_sparse: Option<(usize, usize)> = None;
        for (idx, (&rk, crit)) in cond_ranges.iter().zip(&crits).enumerate() {
            if !crit.indexable() {
                continue;
            }
            if !self.columns.iter().any(|c| c.range == rk)
                && let Some(shared) = shared_col_get(rk)
            {
                self.put_column(shared);
            }
            if let Some(pos) = self.columns.iter().position(|c| c.range == rk) {
                let mut col_ix = self.columns.remove(pos).unwrap();
                let ready = if crit.num.is_some() {
                    col_ix.ensure_numbers_from_shared() || col_ix.tried_numbers
                } else {
                    col_ix.ensure_text_from_shared() || col_ix.tried_text
                };
                if !ready && cond_ranges.len() > 1 {
                    if let Some(col_data) = data_src.load_col(rk) {
                        if crit.num.is_some() {
                            col_ix.ensure_numbers(&col_data);
                        } else {
                            col_ix.ensure_text(&col_data);
                        }
                        shared_col_put(&col_ix);
                    }
                } else {
                    shared_col_put(&col_ix);
                }
                self.put_column(col_ix);
            } else if cond_ranges.len() > 1 {
                if let Some(col_data) = data_src.load_col(rk) {
                    let mut col_ix = ColumnIndex::new_empty(rk, col_data.len());
                    if crit.num.is_some() {
                        col_ix.ensure_numbers(&col_data);
                    } else {
                        col_ix.ensure_text(&col_data);
                    }
                    shared_col_put(&col_ix);
                    self.put_column(col_ix);
                }
            }
            if let Some(col_ix) = self.columns.iter().find(|c| c.range == rk)
                && let Some(cnt) = col_ix.exact_rows_count(crit)
            {
                if cnt == 0 {
                    return Some(Self::finish_aggregate(key.function, 0, 0.0, None));
                }
                if cnt <= sparse_limit && best_sparse.is_none_or(|(_, prev)| cnt < prev) {
                    best_sparse = Some((idx, cnt));
                }
            }
        }

        if let Some((best_idx, _)) = best_sparse {
            let rk = cond_ranges[best_idx];
            if let Some(rows) = self
                .columns
                .iter()
                .find(|c| c.range == rk)
                .and_then(|c| c.collect_exact_rows_sorted(&crits[best_idx], len))
            {
                let count_only = key.function == 1;
                let mut hits = 0usize;
                let mut total = 0.0;
                let mut best: Option<f64> = None;
                for row_u32 in rows {
                    let row = row_u32 as usize;
                    let mut all_ok = true;
                    for (j, (&other_rk, other_crit)) in cond_ranges.iter().zip(&crits).enumerate() {
                        if j == best_idx {
                            continue;
                        }
                        let v = data_src.probe_cell(other_rk, row)?;
                        if !other_crit.matches_coerced(&v) {
                            all_ok = false;
                            break;
                        }
                    }
                    if !all_ok {
                        continue;
                    }
                    if count_only {
                        hits += 1;
                    } else if let Some(vr) = val_range {
                        let v = data_src.probe_cell(vr, row)?;
                        if let Ok(n) = v.to_number() {
                            hits += 1;
                            match key.function {
                                4 => best = Some(best.map_or(n, |b| b.min(n))),
                                5 => best = Some(best.map_or(n, |b| b.max(n))),
                                _ => total += n,
                            }
                        }
                    }
                }
                return Some(Self::finish_aggregate(key.function, hits, total, best));
            }
        }

        let mut masks = Vec::with_capacity(cond_ranges.len());
        for (&rk, pat) in cond_ranges.iter().zip(&key.criteria) {
            if let Some((_, _, bits)) = self.masks.iter().find(|(r, p, _)| *r == rk && p == pat) {
                masks.push(bits.clone());
                continue;
            }
            if !self.columns.iter().any(|c| c.range == rk)
                && let Some(shared) = shared_col_get(rk)
            {
                self.put_column(shared);
            }
            let mask = self.mask_lazy(rk, pat, range_cell_len(rk), data_src)?;
            if let Some(col_ix) = self.columns.back() {
                shared_col_put(col_ix);
            }
            masks.push(mask);
        }

        if key.function == 1 {
            return Some(Self::eval_masks(1, len, &masks, &[]));
        }
        let vr = val_range?;
        if let Some(val_sc) = data_src.shared_cond_col(vr) {
            return Some(Self::eval_masks_from_cond(key.function, len, &masks, &val_sc));
        }
        let values = data_src.load_col(vr)?;
        Some(Self::eval_masks(key.function, len, &masks, &values))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_predicates_match_serial_aggregates_across_columns_and_partial_words() {
        fn range(v: Vec<FValue>) -> Arg {
            Arg::Range {
                rows: v.len(),
                cols: 1,
                values: Arc::new(v),
            }
        }
        let n = 329;
        let amounts = range(
            (0..n)
                .map(|i| FValue::Number([1e16, 0.125, -1e16, -0.25][i % 4]))
                .collect(),
        );
        let dates = range((0..n).map(|i| FValue::Number((i % 17) as f64)).collect());
        let names = range(
            (0..n)
                .map(|i| FValue::Text(["Apple", "ape", "other", ""][i % 4].into()))
                .collect(),
        );
        let mut cache = MatchIndexes::default();
        let col = |c| (0, 0, c, n as i64 - 1, c);
        for (id, func) in [
            "SUMIFS",
            "COUNTIFS",
            "AVERAGEIF",
            "AVERAGEIFS",
            "MINIFS",
            "MAXIFS",
        ]
        .into_iter()
        .enumerate()
        {
            for condition in [">=3", "<>5", "<12"] {
                let p = Arg::Val(FValue::Text(condition.into()));
                let w = Arg::Val(FValue::Text("a*".into()));
                let (args, ranges, criteria) = match id {
                    1 => (
                        vec![dates.clone(), p, names.clone(), w],
                        vec![col(1), col(2)],
                        vec![condition.into(), "a*".into()],
                    ),
                    2 => (
                        vec![dates.clone(), p, amounts.clone()],
                        vec![col(1), col(0)],
                        vec![condition.into()],
                    ),
                    _ => (
                        vec![amounts.clone(), dates.clone(), p, names.clone(), w],
                        vec![col(0), col(1), col(2)],
                        vec![condition.into(), "a*".into()],
                    ),
                };
                let key = CriteriaKey {
                    function: id as u8,
                    ranges,
                    criteria,
                };
                assert_eq!(
                    cache.lookup(&key, &args),
                    Some(super::super::functions::call(func, &args)),
                    "{func} {condition}"
                );
            }
        }
        assert_eq!(cache.columns.len(), 2);
        assert_eq!(cache.masks.len(), 4);
    }

    #[test]
    fn predicate_masks_match_mixed_types_epsilon_wildcards_and_eviction() {
        let samples = [
            FValue::Empty,
            FValue::Bool(false),
            FValue::Bool(true),
            FValue::number(1.),
            FValue::number(1. + f64::EPSILON),
            FValue::number(1. + 2. * f64::EPSILON),
            FValue::number(-0.),
            FValue::Text(" 1 ".into()),
            FValue::Error(super::super::ErrKind::Div0),
            FValue::Text("ΟΣ".into()),
            FValue::Text("a*".into()),
        ];
        let col = |c, len| (0, 0, c, len as i64 - 1, c);
        let mut cache = MatchIndexes::default();
        for (run, len) in [256, 257, 320, 321, 384, 385].into_iter().enumerate() {
            let mut values: Vec<_> = (0..len)
                .map(|i| samples[i % samples.len()].clone())
                .collect();
            if run % 2 == 1 {
                values[len - 1] = FValue::Number(f64::INFINITY);
            }
            let data = Arg::Range {
                rows: len,
                cols: 1,
                values: Arc::new(values),
            };
            let amount = Arg::Range {
                rows: len,
                cols: 1,
                values: Arc::new(
                    (0..len)
                        .map(|i| FValue::number([1e16, 0.0625, -1e16, -0.125][i % 4]))
                        .collect(),
                ),
            };
            for p in [
                "=1",
                "<>1",
                ">=0",
                "<1",
                "<=1",
                ">1",
                "*ς",
                "a~*",
                "<>true",
                "=false",
                "<empty",
                "<>#DIV/0!",
                "",
            ] {
                let q = Arg::Val(FValue::Text(p.into()));
                let q2 = Arg::Val(FValue::Text("<>never".into()));
                for (function, name) in [
                    (0, "SUMIFS"),
                    (1, "COUNTIFS"),
                    (3, "AVERAGEIFS"),
                    (4, "MINIFS"),
                    (5, "MAXIFS"),
                ] {
                    let mut args = vec![data.clone(), q.clone(), data.clone(), q2.clone()];
                    let mut ranges = vec![col(0, len), col(0, len)];
                    if function != 1 {
                        args.insert(0, amount.clone());
                        ranges.insert(0, col(1, len));
                    }
                    let key = CriteriaKey {
                        function,
                        ranges,
                        criteria: vec![p.to_owned(), "<>never".into()],
                    };
                    assert_eq!(
                        cache.lookup(&key, &args),
                        Some(super::super::functions::call(name, &args)),
                        "{name} {p}, {len} rows"
                    );
                }
            }
        }
    }

    #[test]
    fn inequalities_match_scan_coercions_boundaries_and_exact_sum_order() {
        let data = [
            FValue::Empty,
            FValue::Bool(true),
            FValue::Text(" 1 ".into()),
            FValue::Error(super::super::ErrKind::Value),
            FValue::Text("abc".into()),
            FValue::Number(-0.0),
            FValue::Number(1.0 + f64::EPSILON),
            FValue::Number(2.5),
        ];
        let amounts: Vec<_> = (0..data.len())
            .map(|i| FValue::Number(i as f64 - 3.))
            .collect();
        for weights in [None, Some(amounts.as_slice())] {
            let sorted = Sorted::build(&data, weights).unwrap();
            for pattern in [">0", "<=0", "<>1", "<=1", ">1", "<2.6", ">=1"] {
                let crit = Crit::parse(pattern);
                let (op, n) = crit.numeric_bounds().unwrap();
                let expected = data
                    .iter()
                    .enumerate()
                    .filter(|(_, v)| crit.matches_coerced(v))
                    .map(|(i, _)| {
                        weights.map_or(1.0, |w| w[i].to_number().unwrap_or(0.0))
                    })
                    .sum::<f64>();
                assert_eq!(sorted.query(op, n), FValue::number(expected), "{pattern}");
            }
        }
    }
}
