#[cfg(test)]
use super::Arg;
use super::{
    CriteriaKey, ErrKind, FValue, RangeKey,
    functions::{Crit, range_cell_len},
};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

// A single numeric inequality can be answered from two cut points in a sorted column.
// Only exactly summable integer amounts may change summation order.
#[derive(Clone)]
pub(crate) struct Sorted {
    keys: Vec<f64>,
    prefix: Option<Vec<i64>>,
    prefix_cnt: Option<Vec<u32>>,
    total: i64,
    total_cnt: u32,
}

impl Sorted {
    fn build(keys: &[FValue], amounts: Option<&[FValue]>) -> Option<Self> {
        const EXACT: u64 = 1 << 53;
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
                        if !x.is_finite() || x.fract() != 0.0 || x.abs() > EXACT as f64 {
                            return None;
                        }
                        let weight = x as i64;
                        absolute += weight.unsigned_abs();
                        if absolute > EXACT {
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
        let mut prefix = amounts.map(|_| {
            let mut v = Vec::with_capacity(rows.len() + 1);
            v.push(0i64);
            v
        });
        let mut prefix_cnt = amounts.map(|_| {
            let mut v = Vec::with_capacity(rows.len() + 1);
            v.push(0u32);
            v
        });
        let mut sorted_keys = Vec::with_capacity(rows.len());
        for (value, amount, has_num) in rows {
            if let Some(p) = &mut prefix {
                p.push(p.last().copied().unwrap_or(0) + amount);
            }
            if let Some(c) = &mut prefix_cnt {
                c.push(c.last().copied().unwrap_or(0) + has_num);
            }
            sorted_keys.push(value);
        }
        Some(Self {
            keys: sorted_keys,
            prefix,
            prefix_cnt,
            total,
            total_cnt,
        })
    }

    fn query(&self, op: &str, n: f64) -> FValue {
        self.query_fn(0, op, n)
    }

    fn query_fn(&self, function: u8, op: &str, n: f64) -> FValue {
        let at_sum = |pos: usize| self.prefix.as_ref().map_or(pos as i64, |p| p[pos]);
        let at_cnt = |pos: usize| self.prefix_cnt.as_ref().map_or(pos as u32, |c| c[pos]);
        let numeric_sum = at_sum(self.keys.len());
        let numeric_cnt = at_cnt(self.keys.len());
        let (sum, cnt) = match op {
            "<" => {
                let p = self.keys.partition_point(|x| *x < n);
                (at_sum(p), at_cnt(p))
            }
            "<=" => {
                let p = self.keys.partition_point(|x| *x <= n);
                (at_sum(p), at_cnt(p))
            }
            ">" => {
                let p = self.keys.partition_point(|x| *x <= n);
                (numeric_sum - at_sum(p), numeric_cnt - at_cnt(p))
            }
            ">=" => {
                let p = self.keys.partition_point(|x| *x < n);
                (numeric_sum - at_sum(p), numeric_cnt - at_cnt(p))
            }
            "<>" => {
                let lo = self.keys.partition_point(|x| x - n < -f64::EPSILON);
                let hi = self.keys.partition_point(|x| x - n <= f64::EPSILON);
                (
                    self.total - (at_sum(hi) - at_sum(lo)),
                    self.total_cnt - (at_cnt(hi) - at_cnt(lo)),
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

#[derive(Clone)]
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
        Some(Self {
            keys: sorted_keys,
            prefix_min,
            suffix_min,
            prefix_max,
            suffix_max,
            non_key_min,
            non_key_max,
        })
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

#[derive(Clone, Copy)]
struct BlockBounds {
    min_key: f64,
    max_key: f64,
    any_num_key: bool,
    has_non_num_key: bool,
}

/// Preserves exact IEEE-754 source-order summation for fractional SUMIFS/AVERAGEIF/AVERAGEIFS
/// while skipping non-matching 256-row blocks in O(1) and avoiding repeated FValue coercions.
#[derive(Clone)]
struct PagedOrdered {
    key_nums: Vec<f64>,
    val_nums: Vec<f64>,
    blocks: Vec<BlockBounds>,
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
            key_nums,
            val_nums,
            blocks,
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

#[derive(Clone)]
pub(crate) struct BuiltOrdered {
    len: usize,
    exact: Option<Sorted>,
    minmax: Option<MinMaxOrdered>,
    paged: Option<PagedOrdered>,
}

impl BuiltOrdered {
    fn build(keys: &[FValue], amounts: Option<&[FValue]>) -> Option<Self> {
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
        let exact = Sorted::build(keys, Some(amounts_slice));
        let minmax = MinMaxOrdered::build(keys, amounts_slice);
        let paged = if exact.is_none() {
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
        if let Some(shared) = shared_get(val_range, cond_range) {
            return shared.query(key.function, op, number);
        }
        let Some(i) = self
            .0
            .iter()
            .position(|e| e.val_range == val_range && e.cond_range == cond_range)
        else {
            if self.0.len() == 8 {
                self.0.pop_front();
            }
            self.0.push_back(Entry {
                val_range,
                cond_range,
                attempted: false,
                ordered: None,
            });
            return None;
        };
        let mut entry = self.0.remove(i)?;
        if !entry.attempted {
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
            let built = BuiltOrdered::build(&keys_col, amounts_col.as_deref().map(Vec::as_slice)).map(Arc::new);
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
    pub(crate) text: Option<Arc<HashMap<Arc<str>, Vec<u32>>>>,
    pub(crate) tried_numbers: bool,
    pub(crate) tried_text: bool,
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
        }
    }

    fn ensure_numbers(&mut self, input: &[FValue]) {
        if self.tried_numbers {
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
        self.tried_text = true;
        let mut dictionary = HashMap::<Arc<str>, Vec<u32>>::new();
        let mut bytes = 0usize;
        for (i, v) in input.iter().enumerate() {
            let text: Arc<str> = match v {
                FValue::Text(t) => super::lookup_cache::lowercase_arc(t),
                FValue::Bool(true) => "true".into(),
                FValue::Bool(false) => "false".into(),
                FValue::Empty => "".into(),
                _ => super::lookup_cache::lowercase_str(&v.display()),
            };
            if !dictionary.contains_key(&text) {
                bytes += text.len();
            }
            dictionary.entry(text).or_default().push(i as u32);
            if dictionary.len() > 65_536 || bytes > 8 * 1024 * 1024 {
                break;
            }
        }
        if dictionary.len() <= 65_536 && bytes <= 8 * 1024 * 1024 {
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
            Some(words.get(crit.text.as_str()).map_or(0, Vec::len))
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

    fn mark(&mut self, input: &[FValue], criterion: &Crit, bits: &mut [u64]) {
        let mut set = |row: u32| bits[row as usize / 64] |= 1 << (row % 64);
        if let Some((op, n)) = criterion.numeric_query() {
            self.ensure_numbers(input);
            if n.is_finite() && let Some(sorted) = &self.numbers {
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
                return;
            }
        } else {
            self.ensure_text(input);
            if let Some(words) = &self.text {
                if !criterion.wild && criterion.op == "=" {
                    if let Some(rows) = words.get(criterion.text.as_str()) {
                        for &row in rows {
                            set(row);
                        }
                    }
                    return;
                }
                if !criterion.wild && criterion.op == "<>" {
                    if let Some(rows) = words.get(criterion.text.as_str()) {
                        for &row in rows {
                            set(row);
                        }
                    }
                    for word in bits {
                        *word = !*word;
                    }
                    return;
                }
                for (word, rows) in words.iter() {
                    if criterion.matches_coerced_text(word) {
                        for &row in rows {
                            set(row);
                        }
                    }
                }
                return;
            }
        }
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
        let max_len = cond_ranges
            .iter()
            .copied()
            .map(range_cell_len)
            .max()?;
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

        // High-selectivity fast path: if any exact equality column already has (or can build)
        // a ColumnIndex with very few matching rows, probe remaining criteria only at those rows
        // without materializing multi-million-cell sibling/value columns or bitmasks.
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
                let ready = if crit.num.is_some() {
                    self.columns[pos].tried_numbers
                } else {
                    self.columns[pos].tried_text
                };
                if !ready && cond_ranges.len() > 1 {
                    if let Some(col_data) = data_src.load_col(rk) {
                        let mut col_ix = self.columns.remove(pos).unwrap();
                        if crit.num.is_some() {
                            col_ix.ensure_numbers(&col_data);
                        } else {
                            col_ix.ensure_text(&col_data);
                        }
                        shared_col_put(&col_ix);
                        self.put_column(col_ix);
                    }
                }
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
            let col_data = data_src.load_col(rk)?;
            let mask = self.mask(rk, pat, &col_data);
            if let Some(col_ix) = self.columns.back() {
                shared_col_put(col_ix);
            }
            masks.push(mask);
        }

        if key.function == 1 {
            return Some(Self::eval_masks(1, len, &masks, &[]));
        }
        let values = data_src.load_col(val_range?)?;
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
        for (id, func) in ["SUMIFS", "COUNTIFS", "AVERAGEIF", "AVERAGEIFS", "MINIFS", "MAXIFS"]
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
            let mut values: Vec<_> = (0..len).map(|i| samples[i % samples.len()].clone()).collect();
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
                    .map(|(i, _)| weights.map_or(1., |w| w[i].to_number().unwrap()))
                    .sum();
                assert_eq!(sorted.query(op, n), FValue::Number(expected), "{pattern}");
            }
        }
        for unsafe_amounts in [
            vec![FValue::Number(0.1)],
            vec![FValue::Number(2f64.powi(53)), FValue::Number(1.)],
        ] {
            assert!(Sorted::build(&data, Some(&unsafe_amounts)).is_none());
        }
    }
}
