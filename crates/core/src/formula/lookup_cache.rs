use super::fxhash::{FxBuildHasher, FxHashMap};
use super::{ErrKind, FValue, RangeKey, deps::CellKey};
use crate::store::{CellVersion, PAGE_ROWS};
use crate::{Cell, Value};
use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, Mutex, OnceLock};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) enum LookupKey {
    Empty,
    Bool(bool),
    Number(u64),
    Text(Arc<str>),
    Error(u8),
}

impl LookupKey {
    #[inline]
    pub(crate) fn from_fvalue(v: &FValue) -> Self {
        match v {
            FValue::Empty => Self::Empty,
            FValue::Number(n) => Self::Number(if *n == 0.0 { 0 } else { n.to_bits() }),
            FValue::Bool(b) => Self::Bool(*b),
            FValue::Text(s) => Self::Text(lowercase_arc(s)),
            FValue::Error(e) => Self::Error(*e as u8),
            FValue::Array(a) => Self::from_fvalue(&a.get(0, 0)),
            FValue::Lambda(_) => Self::Text("#func!".into()),
        }
    }

    #[inline]
    pub(crate) fn from_value(v: &Value) -> Self {
        match v {
            Value::Empty => Self::Empty,
            Value::Number(n) => Self::Number(if *n == 0.0 { 0 } else { n.to_bits() }),
            Value::Bool(b) => Self::Bool(*b),
            Value::Text(s) => Self::Text(lowercase_arc(s)),
            Value::Error(e) => Self::Error(*e as u8),
        }
    }
}

#[inline]
pub(crate) fn lowercase_arc(s: &Arc<str>) -> Arc<str> {
    if s.is_ascii() {
        if !s.bytes().any(|b| b.is_ascii_uppercase()) {
            return s.clone();
        }
        return s.to_ascii_lowercase().into();
    }
    if !s.contains('Σ') && !s.chars().any(char::is_uppercase) {
        return s.clone();
    }
    s.to_lowercase().into()
}

#[inline]
pub(crate) fn lowercase_str(s: &str) -> Arc<str> {
    if s.is_ascii() {
        if !s.bytes().any(|b| b.is_ascii_uppercase()) {
            return s.into();
        }
        return s.to_ascii_lowercase().into();
    }
    s.to_lowercase().into()
}

#[derive(Clone, Debug)]
pub(crate) enum Positions {
    One(u32),
    Many(Vec<u32>),
}

impl Positions {
    #[inline]
    pub(crate) fn first(&self) -> usize {
        match self {
            Self::One(i) => *i as usize,
            Self::Many(v) => v[0] as usize,
        }
    }

    #[inline]
    pub(crate) fn last(&self) -> usize {
        match self {
            Self::One(i) => *i as usize,
            Self::Many(v) => *v.last().unwrap() as usize,
        }
    }

    fn push_sorted(&mut self, pos: u32) {
        match self {
            Self::One(existing) => {
                if *existing == pos {
                    return;
                }
                let (a, b) = if *existing < pos {
                    (*existing, pos)
                } else {
                    (pos, *existing)
                };
                *self = Self::Many(vec![a, b]);
            }
            Self::Many(list) => {
                if list.last().is_some_and(|&last| last < pos) {
                    list.push(pos);
                } else if let Err(idx) = list.binary_search(&pos) {
                    list.insert(idx, pos);
                }
            }
        }
    }

    fn remove_pos(&mut self, pos: u32) -> bool {
        match self {
            Self::One(existing) => *existing == pos,
            Self::Many(list) => {
                if let Ok(idx) = list.binary_search(&pos) {
                    list.remove(idx);
                }
                if list.len() == 1 {
                    *self = Self::One(list[0]);
                    false
                } else {
                    list.is_empty()
                }
            }
        }
    }

    fn extra_heap_bytes(&self) -> usize {
        match self {
            Self::One(_) => 0,
            Self::Many(v) => v.capacity() * std::mem::size_of::<u32>(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct LookupMap(FxHashMap<LookupKey, Positions>);

impl LookupMap {
    pub(crate) fn with_capacity(cap: usize) -> Self {
        Self(FxHashMap::with_capacity_and_hasher(cap, FxBuildHasher))
    }

    #[inline]
    pub(crate) fn get_first(&self, key: &LookupKey) -> Option<usize> {
        self.0.get(key).map(Positions::first)
    }

    #[inline]
    pub(crate) fn get_last(&self, key: &LookupKey) -> Option<usize> {
        self.0.get(key).map(Positions::last)
    }

    #[inline]
    pub(crate) fn insert_next(&mut self, key: LookupKey, pos: u32) {
        match self.0.get_mut(&key) {
            Some(existing) => existing.push_sorted(pos),
            None => {
                self.0.insert(key, Positions::One(pos));
            }
        }
    }

    fn remove_pos(&mut self, key: &LookupKey, pos: u32) {
        let empty = match self.0.get_mut(key) {
            Some(existing) => existing.remove_pos(pos),
            None => false,
        };
        if empty {
            self.0.remove(key);
        }
    }

    fn heap_bytes(&self) -> usize {
        self.0.capacity() * (std::mem::size_of::<(LookupKey, Positions)>() + 2)
            + self
                .0
                .iter()
                .map(|(k, v)| {
                    let kb = match k {
                        LookupKey::Text(s) => s.len(),
                        _ => 0,
                    };
                    kb + v.extra_heap_bytes()
                })
                .sum::<usize>()
    }
}

pub(super) type Index = Arc<LookupMap>;
pub(crate) type TextDict = FxHashMap<Arc<str>, Arc<Vec<u32>>>;
type Key = (RangeKey, bool);

#[derive(Clone, Debug)]
struct PageKeyRecord {
    slab: Option<Arc<Vec<Cell>>>,
    has_formula: bool,
    has_invalid_formula_cache: bool,
    nonempty_keys: Arc<Vec<(LookupKey, u32)>>,
}

#[derive(Clone, Debug)]
enum Keys {
    Column {
        r1: u32,
        r2: u32,
        col: u32,
        has_formula: bool,
        pages: Vec<PageKeyRecord>,
    },
    PlainColumn(Vec<crate::cf::PageSlice>),
    Row(Vec<Cell>),
}

// Extending a sparse slab outside the key range can introduce allocated blank
// slots within it. Those slots do not change a lookup.
fn occupied_keys(
    pages: &[crate::cf::PageSlice],
) -> impl Iterator<Item = ((u32, usize), &Cell)> {
    pages.iter().flat_map(|p| {
        (p.start..p.end).filter_map(move |row| {
            let cell = &p.cells[row];
            (!cell.is_blank()).then_some(((p.page, row), cell))
        })
    })
}

fn invalid_formula_cache(cell: &Cell) -> bool {
    cell.formula.is_some()
        && (cell.value.is_empty()
            || matches!(&cell.value, Value::Text(s) if s.as_ref() == "#CALC!"))
}

fn build_page_key_record(
    page_idx: u32,
    r1: u32,
    r2: u32,
    slab: Option<&Arc<Vec<Cell>>>,
) -> PageKeyRecord {
    let base = page_idx * PAGE_ROWS;
    let start = r1.saturating_sub(base) as usize;
    let end = ((r2 - base) as usize + 1).min(PAGE_ROWS as usize);
    let Some(cells) = slab else {
        return PageKeyRecord {
            slab: None,
            has_formula: false,
            has_invalid_formula_cache: false,
            nonempty_keys: Arc::new(Vec::new()),
        };
    };
    let slice_end = end.min(cells.len());
    if start >= slice_end {
        return PageKeyRecord {
            slab: Some(cells.clone()),
            has_formula: false,
            has_invalid_formula_cache: false,
            nonempty_keys: Arc::new(Vec::new()),
        };
    }
    let mut has_formula = false;
    let mut has_invalid = false;
    let mut out = Vec::new();
    for ro in start..slice_end {
        let cell = &cells[ro];
        if cell.is_blank() {
            continue;
        }
        if cell.formula.is_some() {
            has_formula = true;
            if invalid_formula_cache(cell) {
                has_invalid = true;
            }
        }
        let global_row = base + ro as u32;
        out.push((LookupKey::from_value(&cell.value), global_row - r1));
    }
    PageKeyRecord {
        slab: Some(cells.clone()),
        has_formula,
        has_invalid_formula_cache: has_invalid,
        nonempty_keys: Arc::new(out),
    }
}

fn column_slabs(
    sheet: &crate::Sheet,
    r1: u32,
    r2: u32,
    col: u32,
) -> Vec<Option<Arc<Vec<Cell>>>> {
    let first_page = r1 / PAGE_ROWS;
    let last_page = r2 / PAGE_ROWS;
    let count = (last_page - first_page + 1) as usize;
    let mut slabs = vec![None; count];
    for s in sheet.cf_pages((r1, col, r2, col)) {
        if s.page >= first_page && s.page <= last_page {
            slabs[(s.page - first_page) as usize] = Some(s.cells);
        }
    }
    slabs
}

impl Keys {
    fn capture(sheet: &crate::Sheet, key: Key) -> Self {
        let ((_, r1, c1, r2, c2), vertical) = key;
        if vertical {
            Self::PlainColumn(sheet.cf_pages((r1 as u32, c1 as u32, r2 as u32, c1 as u32)))
        } else {
            Self::Row(
                (c1..=c2)
                    .map(|c| sheet.get(r1 as u32, c as u32).cloned().unwrap_or_default())
                    .collect(),
            )
        }
    }

    fn matches(&self, sheet: &crate::Sheet, key: Key, allow_formulas: bool) -> bool {
        let ((_, r1, c1, r2, c2), _) = key;
        match self {
            Self::Column {
                r1: cr1,
                r2: cr2,
                col,
                has_formula,
                pages,
            } => {
                if *has_formula && !allow_formulas {
                    return false;
                }
                let slabs = column_slabs(sheet, *cr1, *cr2, *col);
                if slabs.len() != pages.len() {
                    return false;
                }
                let first_page = *cr1 / PAGE_ROWS;
                for (idx, (old, now_slab)) in pages.iter().zip(slabs.iter()).enumerate() {
                    let ptr_same = match (&old.slab, now_slab) {
                        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                        (None, None) => true,
                        _ => false,
                    };
                    if ptr_same {
                        continue;
                    }
                    let rebuilt = build_page_key_record(
                        first_page + idx as u32,
                        *cr1,
                        *cr2,
                        now_slab.as_ref(),
                    );
                    if (rebuilt.has_formula && !allow_formulas)
                        || rebuilt.has_invalid_formula_cache
                        || *rebuilt.nonempty_keys != *old.nonempty_keys
                    {
                        return false;
                    }
                }
                true
            }
            Self::PlainColumn(old) => {
                let now = sheet.cf_pages((r1 as u32, c1 as u32, r2 as u32, c1 as u32));
                old == &now || occupied_keys(old).eq(occupied_keys(&now))
            }
            Self::Row(old) => (c1..=c2).zip(old).all(|(c, v)| {
                sheet
                    .get(r1 as u32, c as u32)
                    .map_or_else(|| v.is_blank(), |cell| cell == v)
            }),
        }
    }

    fn bytes(&self) -> usize {
        match self {
            Self::Column { pages, .. } => {
                pages.capacity() * std::mem::size_of::<PageKeyRecord>()
                    + pages
                        .iter()
                        .map(|p| {
                            p.nonempty_keys.capacity() * std::mem::size_of::<(LookupKey, u32)>()
                        })
                        .sum::<usize>()
            }
            Self::PlainColumn(pages) => pages
                .iter()
                .map(|p| {
                    p.cells.capacity() * std::mem::size_of::<Cell>()
                        + std::mem::size_of::<crate::cf::PageSlice>()
                })
                .sum(),
            Self::Row(cells) => cells.capacity() * std::mem::size_of::<Cell>(),
        }
    }
}

#[derive(Debug)]
struct Entry {
    inputs: Keys,
    version: CellVersion,
    key: Key,
    has_formula: bool,
    bytes: usize,
    index: Index,
}

// ============================================================================
// Persistent 256-Row Page-Block Conditional Column Index (Incremental Merge!)
// ============================================================================

#[derive(Debug)]
pub(crate) struct CondPage {
    pub slab: Option<Arc<Vec<Cell>>>,
    pub start_in_slab: u16,
    pub rows_in_page: u16,
    pub has_formula: bool,
    pub has_invalid_formula_cache: bool,
    pub has_non_num: bool,
    pub has_non_finite: bool,
    pub min_num: f64,
    pub max_num: f64,
    pub num_count: u16,
    /// Exact-integer summary metadata for fast O(pages) SUMIFS/AVERAGEIFS validation
    pub all_exact_int: bool,
    pub int_sum: i64,
    pub int_abs_sum: u64,
    /// Coerced f64 per row in page (via Value -> f64 rules matching FValue::to_number)
    pub coerced_nums: Vec<f64>,
    /// Bitmask of rows in page where to_number() succeeded (4 × 64 = 256 bits)
    pub coerced_ok: [u64; 4],
    /// Strict Value::Number mask (for single-criterion COUNTIF/SUMIF non-coerced numeric)
    pub strict_num_ok: [u64; 4],
    /// Pre-sorted finite coerced numbers `(value, local_row_offset)` within this 256-row page
    pub sorted_nums: Vec<(f64, u16)>,
    /// Lazily materialized per-page lowercased text dictionary (only built when a text query runs!)
    page_text_dict: OnceLock<FxHashMap<Arc<str>, Vec<u16>>>,
}

impl CondPage {
    #[inline]
    pub(crate) fn has_coerced_num(&self, local: usize) -> bool {
        (self.coerced_ok[local / 64] & (1u64 << (local % 64))) != 0
    }

    pub(crate) fn text_dict(&self) -> &FxHashMap<Arc<str>, Vec<u16>> {
        self.page_text_dict.get_or_init(|| {
            let mut dict: FxHashMap<Arc<str>, Vec<u16>> = FxHashMap::default();
            let slice = self.slab.as_ref().map(|v| v.as_slice()).unwrap_or_default();
            let start = self.start_in_slab as usize;
            let empty_arc: Arc<str> = "".into();
            for idx in 0..self.rows_in_page as usize {
                let ro = start + idx;
                let txt = match slice.get(ro).filter(|c| !c.is_blank()) {
                    None => empty_arc.clone(),
                    Some(cell) => value_to_coerced_lower_text(&cell.value),
                };
                dict.entry(txt).or_default().push(idx as u16);
            }
            dict
        })
    }
}

#[inline]
fn value_to_coerced_number(v: &Value) -> Result<f64, ErrKind> {
    match v {
        Value::Empty => Ok(0.0),
        Value::Number(n) => Ok(*n),
        Value::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
        Value::Text(s) => {
            let t = s.trim();
            if t.is_empty() {
                Ok(0.0)
            } else {
                t.parse::<f64>().map_err(|_| ErrKind::Value)
            }
        }
        Value::Error(e) => Err(*e),
    }
}

#[inline]
fn value_to_coerced_lower_text(v: &Value) -> Arc<str> {
    match v {
        Value::Empty => "".into(),
        Value::Text(s) => lowercase_arc(s),
        Value::Bool(true) => "true".into(),
        Value::Bool(false) => "false".into(),
        Value::Number(n) => crate::model::format_number(*n).into(),
        Value::Error(e) => e.text().to_ascii_lowercase().into(),
    }
}

fn build_cond_page(
    page_idx: u32,
    r1: u32,
    r2: u32,
    slab: Option<&Arc<Vec<Cell>>>,
) -> Arc<CondPage> {
    const EXACT: u64 = 1 << 53;
    let base = page_idx * PAGE_ROWS;
    let start = r1.saturating_sub(base) as usize;
    let end = ((r2 - base) as usize + 1).min(PAGE_ROWS as usize);
    let rows_in_page = (end - start) as u16;
    let mut coerced_nums = vec![0.0; rows_in_page as usize];
    let mut coerced_ok = [0u64; 4];
    let mut strict_num_ok = [0u64; 4];
    let mut sorted_nums = Vec::with_capacity(rows_in_page as usize);
    let mut has_formula = false;
    let mut has_invalid_formula_cache = false;
    let mut has_non_num = false;
    let mut has_non_finite = false;
    let mut min_num = f64::INFINITY;
    let mut max_num = f64::NEG_INFINITY;
    let mut num_count = 0u16;
    let mut all_exact_int = true;
    let mut int_sum = 0i64;
    let mut int_abs_sum = 0u64;

    let slice = slab.map(|v| v.as_slice()).unwrap_or_default();

    for idx in 0..rows_in_page as usize {
        let ro = start + idx;
        let word = idx / 64;
        let bit = 1u64 << (idx % 64);
        match slice.get(ro).filter(|c| !c.is_blank()) {
            None => {
                // Blank cell: to_number() -> 0.0
                coerced_ok[word] |= bit;
                num_count += 1;
                min_num = min_num.min(0.0);
                max_num = max_num.max(0.0);
                sorted_nums.push((0.0, idx as u16));
            }
            Some(cell) => {
                if cell.formula.is_some() {
                    has_formula = true;
                    if invalid_formula_cache(cell) {
                        has_invalid_formula_cache = true;
                    }
                }
                if matches!(cell.value, Value::Number(_)) {
                    strict_num_ok[word] |= bit;
                }
                match value_to_coerced_number(&cell.value) {
                    Ok(n) => {
                        coerced_nums[idx] = n;
                        coerced_ok[word] |= bit;
                        if n.is_finite() {
                            num_count += 1;
                            min_num = min_num.min(n);
                            max_num = max_num.max(n);
                            sorted_nums.push((n, idx as u16));
                            if all_exact_int {
                                if n.fract() != 0.0 || n.abs() > EXACT as f64 {
                                    all_exact_int = false;
                                } else {
                                    let w = n as i64;
                                    int_sum = int_sum.wrapping_add(w);
                                    int_abs_sum = int_abs_sum.saturating_add(w.unsigned_abs());
                                    if int_abs_sum > EXACT {
                                        all_exact_int = false;
                                    }
                                }
                            }
                        } else {
                            has_non_finite = true;
                            all_exact_int = false;
                        }
                    }
                    Err(_) => {
                        has_non_num = true;
                    }
                }
            }
        }
    }

    if !has_non_finite {
        sorted_nums.sort_unstable_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    } else {
        sorted_nums.clear();
    }

    Arc::new(CondPage {
        slab: slab.cloned(),
        start_in_slab: start as u16,
        rows_in_page,
        has_formula,
        has_invalid_formula_cache,
        has_non_num,
        has_non_finite,
        min_num,
        max_num,
        num_count,
        all_exact_int,
        int_sum,
        int_abs_sum,
        coerced_nums,
        coerced_ok,
        strict_num_ok,
        sorted_nums,
        page_text_dict: OnceLock::new(),
    })
}

#[derive(Debug)]
pub(crate) struct SharedCondColumn {
    pub r1: u32,
    pub r2: u32,
    pub col: u32,
    pub len: usize,
    pub has_formula: bool,
    pub has_non_finite: bool,
    pub has_non_num: bool,
    pub pages: Vec<Arc<CondPage>>,
    /// Starting row offset in `[0..len)` of each page
    pub page_offsets: Vec<u32>,
    /// Lazily materialized or incrementally merged sorted `(value, row_offset)` across `[0..len)`
    sorted_nums: OnceLock<Option<Arc<Vec<(f64, u32)>>>>,
    /// Lazily materialized or incrementally patched `lowercased_text -> Arc<Vec<u32>>`
    text_dict: OnceLock<Option<Arc<TextDict>>>,
}

impl SharedCondColumn {
    fn build_sorted_nums_full(pages: &[Arc<CondPage>], page_offsets: &[u32]) -> Arc<Vec<(f64, u32)>> {
        let total_nums: usize = pages.iter().map(|p| p.num_count as usize).sum();
        let mut nums = Vec::with_capacity(total_nums);
        for (page, &row_base) in pages.iter().zip(page_offsets.iter()) {
            for &(val, local) in &page.sorted_nums {
                nums.push((val, row_base + local as u32));
            }
        }
        nums.sort_unstable_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        Arc::new(nums)
    }

    /// O(N) linear streaming merge of `old_sorted` (dropping entries from changed pages)
    /// with the pre-sorted entries from `changed_pages`, avoiding O(N log N) full re-sort!
    fn merge_sorted_nums_incremental(
        old_sorted: &[(f64, u32)],
        pages: &[Arc<CondPage>],
        page_offsets: &[u32],
        changed_indices: &[usize],
    ) -> Arc<Vec<(f64, u32)>> {
        let mut delta = Vec::with_capacity(changed_indices.len() * PAGE_ROWS as usize);
        for &idx in changed_indices {
            let base = page_offsets[idx];
            for &(val, local) in &pages[idx].sorted_nums {
                delta.push((val, base + local as u32));
            }
        }
        if changed_indices.len() > 1 {
            delta.sort_unstable_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        }

        let mut out = Vec::with_capacity(old_sorted.len() + delta.len());
        let mut di = 0usize;

        if changed_indices.len() == 1 {
            // Hot path: single edited page `[lo, hi)` -> branch-predicted filter + hoisted merge
            let idx = changed_indices[0];
            let lo = page_offsets[idx];
            let hi = lo + pages[idx].rows_in_page as u32;
            let dlen = delta.len();
            if dlen == 0 {
                for &(ov, orow) in old_sorted {
                    if orow < lo || orow >= hi {
                        out.push((ov, orow));
                    }
                }
            } else {
                let mut cur_dv = delta[0].0;
                let mut cur_dr = delta[0].1;
                for &(ov, orow) in old_sorted {
                    if orow >= lo && orow < hi {
                        continue;
                    }
                    while di < dlen
                        && (cur_dv < ov
                            || (cur_dv == ov
                                && cur_dv.total_cmp(&ov).then_with(|| cur_dr.cmp(&orow)).is_le()))
                    {
                        out.push((cur_dv, cur_dr));
                        di += 1;
                        if di < dlen {
                            cur_dv = delta[di].0;
                            cur_dr = delta[di].1;
                        }
                    }
                    out.push((ov, orow));
                }
            }
        } else {
            let mut changed_ranges: Vec<(u32, u32)> = changed_indices
                .iter()
                .map(|&idx| {
                    let lo = page_offsets[idx];
                    (lo, lo + pages[idx].rows_in_page as u32)
                })
                .collect();
            changed_ranges.sort_unstable();
            let in_changed = |row: u32| -> bool {
                let p = changed_ranges.partition_point(|&(_, hi)| hi <= row);
                p < changed_ranges.len() && row >= changed_ranges[p].0 && row < changed_ranges[p].1
            };
            for &(ov, orow) in old_sorted {
                if in_changed(orow) {
                    continue;
                }
                while di < delta.len() {
                    let (dv, drow) = delta[di];
                    if dv.total_cmp(&ov).then_with(|| drow.cmp(&orow)).is_le() {
                        out.push((dv, drow));
                        di += 1;
                    } else {
                        break;
                    }
                }
                out.push((ov, orow));
            }
        }

        if di < delta.len() {
            out.extend_from_slice(&delta[di..]);
        }
        Arc::new(out)
    }

    fn build_text_dict_full(pages: &[Arc<CondPage>], page_offsets: &[u32]) -> Option<Arc<TextDict>> {
        let mut raw: FxHashMap<Arc<str>, Vec<u32>> = FxHashMap::default();
        let mut dict_bytes = 0usize;
        for (page, &row_base) in pages.iter().zip(page_offsets.iter()) {
            let pdict = page.text_dict();
            for (txt, locals) in pdict {
                if let Some(list) = raw.get_mut(txt) {
                    list.extend(locals.iter().map(|&off| row_base + off as u32));
                } else {
                    dict_bytes += txt.len();
                    if raw.len() >= 131_072 || dict_bytes > 16 * 1024 * 1024 {
                        return None;
                    }
                    let mut list = Vec::with_capacity(locals.len());
                    list.extend(locals.iter().map(|&off| row_base + off as u32));
                    raw.insert(txt.clone(), list);
                }
            }
        }
        let mut dict: TextDict = FxHashMap::with_capacity_and_hasher(raw.len(), FxBuildHasher);
        for (k, mut v) in raw {
            if !v.is_sorted() {
                v.sort_unstable();
            }
            dict.insert(k, Arc::new(v));
        }
        Some(Arc::new(dict))
    }

    /// Incrementally patch `old_dict` for ONLY the keys that changed inside `changed_indices`,
    /// cloning only the outer map + CoW `Arc::make_mut` on the affected keys!
    fn patch_text_dict_incremental(
        old_dict: &TextDict,
        old_pages: &[Arc<CondPage>],
        new_pages: &[Arc<CondPage>],
        page_offsets: &[u32],
        changed_indices: &[usize],
    ) -> Option<Arc<TextDict>> {
        let mut dict = old_dict.clone();
        let mut dict_bytes: usize = dict.keys().map(|k| k.len()).sum();

        for &idx in changed_indices {
            let lo = page_offsets[idx];
            let hi = lo + new_pages[idx].rows_in_page as u32;
            let old_pd = old_pages[idx].text_dict();
            let new_pd = new_pages[idx].text_dict();

            // 1. Handle keys present in old page that disappeared or changed local offsets
            for (txt, old_locals) in old_pd {
                if new_pd.get(txt) == Some(old_locals) {
                    continue; // Exact same rows in this page -> untouched!
                }
                if let Some(arc_list) = dict.get_mut(txt) {
                    let list = Arc::make_mut(arc_list);
                    let start = list.partition_point(|&r| r < lo);
                    let end = list.partition_point(|&r| r < hi);
                    if let Some(new_locals) = new_pd.get(txt) {
                        list.splice(
                            start..end,
                            new_locals.iter().map(|&off| lo + off as u32),
                        );
                    } else {
                        list.drain(start..end);
                        if list.is_empty() {
                            dict_bytes = dict_bytes.saturating_sub(txt.len());
                            dict.remove(txt);
                        }
                    }
                }
            }

            // 2. Handle keys newly added in new page
            for (txt, new_locals) in new_pd {
                if old_pd.contains_key(txt) {
                    continue; // Already updated in step 1
                }
                if let Some(arc_list) = dict.get_mut(txt) {
                    let list = Arc::make_mut(arc_list);
                    let pos = list.partition_point(|&r| r < lo);
                    list.splice(
                        pos..pos,
                        new_locals.iter().map(|&off| lo + off as u32),
                    );
                } else {
                    dict_bytes += txt.len();
                    if dict.len() >= 131_072 || dict_bytes > 16 * 1024 * 1024 {
                        return None;
                    }
                    let list: Vec<u32> = new_locals.iter().map(|&off| lo + off as u32).collect();
                    dict.insert(txt.clone(), Arc::new(list));
                }
            }
        }
        Some(Arc::new(dict))
    }

    #[inline]
    pub(crate) fn sorted_nums(&self) -> Option<Arc<Vec<(f64, u32)>>> {
        if self.has_non_finite {
            return None;
        }
        self.sorted_nums
            .get_or_init(|| Some(Self::build_sorted_nums_full(&self.pages, &self.page_offsets)))
            .clone()
    }

    #[inline]
    pub(crate) fn text_dict(&self) -> Option<Arc<TextDict>> {
        self.text_dict
            .get_or_init(|| Self::build_text_dict_full(&self.pages, &self.page_offsets))
            .clone()
    }

    #[inline]
    pub(crate) fn coerced_number_at(&self, row_offset: usize) -> Option<f64> {
        let abs_row = self.r1 + row_offset as u32;
        let p_idx = ((abs_row >> 8) - (self.r1 >> 8)) as usize;
        let page = self.pages.get(p_idx)?;
        let local = (abs_row & 255) as usize - page.start_in_slab as usize;
        ((page.coerced_ok[local >> 6] & (1u64 << (local & 63))) != 0)
            .then(|| page.coerced_nums[local])
    }

    #[inline]
    pub(crate) fn has_coerced_num_at(&self, row_offset: usize) -> bool {
        let abs_row = self.r1 + row_offset as u32;
        let p_idx = ((abs_row >> 8) - (self.r1 >> 8)) as usize;
        let Some(page) = self.pages.get(p_idx) else {
            return false;
        };
        let local = (abs_row & 255) as usize - page.start_in_slab as usize;
        (page.coerced_ok[local >> 6] & (1u64 << (local & 63))) != 0
    }

    fn approx_bytes(&self) -> usize {
        self.pages.len() * (std::mem::size_of::<CondPage>() + 256 * 24)
            + self
                .sorted_nums
                .get()
                .and_then(|o| o.as_ref())
                .map_or(0, |v| v.capacity() * 16)
            + self
                .text_dict
                .get()
                .and_then(|o| o.as_ref())
                .map_or(0, |m| m.capacity() * 48 + self.len * 4)
    }
}

#[derive(Debug)]
struct CondEntry {
    version: CellVersion,
    key: RangeKey,
    bytes: usize,
    col: Arc<SharedCondColumn>,
}

#[derive(Debug, Default)]
struct InnerCaches {
    exact: VecDeque<Entry>,
    cond: VecDeque<CondEntry>,
}

#[derive(Debug, Default)]
struct SharedLookupState {
    caches: Mutex<InnerCaches>,
    /// Single-flight mutex preventing thundering-herd duplicate index builds across worker threads
    build_mu: Mutex<()>,
}

/// Owned by the sheet; clones can reuse immutable results, including from another worker.
/// CellVersion distinguishes divergent edits to cloned snapshots, not just a revision counter.
#[derive(Clone, Debug, Default)]
pub(crate) struct SharedLookup(Arc<SharedLookupState>);

impl SharedLookup {
    pub(super) fn get(&self, sheet: &crate::Sheet, key: Key) -> Option<Index> {
        let mut guard = self.0.caches.lock().ok()?;
        for entry in guard
            .exact
            .iter_mut()
            .rev()
            .filter(|e| e.key == key && !e.has_formula)
        {
            if entry.version == *sheet.cell_version() || entry.inputs.matches(sheet, key, false) {
                entry.version = sheet.cell_version().clone();
                return Some(entry.index.clone());
            }
        }
        None
    }

    pub(super) fn insert(&self, sheet: &crate::Sheet, key: Key, index: Index) {
        const BYTES: usize = 128 * 1024 * 1024;
        const MAX_ENTRIES: usize = 32;
        let version = sheet.cell_version();
        let inputs = Keys::capture(sheet, key);
        let bytes = inputs.bytes() + index.heap_bytes();
        if bytes > BYTES {
            return;
        }
        let Ok(mut guard) = self.0.caches.lock() else {
            return;
        };
        if guard
            .exact
            .iter()
            .any(|e| e.version == *version && e.key == key)
        {
            return;
        }
        while guard.exact.len() >= MAX_ENTRIES
            || bytes + guard.exact.iter().map(|e| e.bytes).sum::<usize>() > BYTES
        {
            guard.exact.pop_front();
        }
        guard.exact.push_back(Entry {
            inputs,
            version: version.clone(),
            key,
            has_formula: false,
            bytes,
            index,
        });
    }

    /// Returns true if a warm `SharedCondColumn` (even from a prior snapshot) exists for `range`.
    pub(crate) fn has_cond_column(&self, range: RangeKey) -> bool {
        self.0
            .caches
            .lock()
            .ok()
            .is_some_and(|g| g.cond.iter().any(|e| e.key == range))
    }

    /// Page-aware lookup builder/updater for single-column vertical lookups.
    /// Reuses unchanged 256-row slabs via `Arc::ptr_eq` and incrementally patches
    /// `LookupMap` when only a small fraction of pages changed.
    pub(super) fn get_or_build_vertical(
        &self,
        sheet: &crate::Sheet,
        range: RangeKey,
        scope: Option<&HashSet<CellKey>>,
    ) -> Option<(Index, usize)> {
        const BYTES: usize = 128 * 1024 * 1024;
        const MAX_ENTRIES: usize = 32;

        let (si, r1_i, c1_i, r2_i, c2_i) = range;
        if c1_i != c2_i || r1_i < 0 || c1_i < 0 || r2_i < r1_i {
            return None;
        }
        let (r1, r2, col) = (r1_i as u32, r2_i as u32, c1_i as u32);
        let allow_formulas = scope.is_some_and(|s| {
            !s.iter()
                .any(|&(s_si, sr, sc)| s_si == si && sc == col && sr >= r1 && sr <= r2)
        });
        let full_key: Key = (range, true);

        // 1. Fast lock check for exact CellVersion hit or zero-change slab hit
        if let Ok(mut guard) = self.0.caches.lock() {
            for entry in guard.exact.iter_mut().rev().filter(|e| e.key == full_key) {
                if (!entry.has_formula || allow_formulas) && entry.version == *sheet.cell_version()
                {
                    return Some((entry.index.clone(), 0));
                }
                if (!entry.has_formula || allow_formulas)
                    && entry.inputs.matches(sheet, full_key, allow_formulas)
                {
                    entry.version = sheet.cell_version().clone();
                    return Some((entry.index.clone(), 0));
                }
            }
        }

        // Single-flight serialize cold/delta rebuild so concurrent workers don't duplicate work
        let _build_guard = self.0.build_mu.lock().ok()?;

        // Re-check after acquiring build_mu
        let mut base_candidate: Option<(Vec<PageKeyRecord>, Index)> = None;
        if let Ok(mut guard) = self.0.caches.lock() {
            for entry in guard.exact.iter_mut().rev().filter(|e| e.key == full_key) {
                if (!entry.has_formula || allow_formulas) && entry.version == *sheet.cell_version()
                {
                    return Some((entry.index.clone(), 0));
                }
                if (!entry.has_formula || allow_formulas)
                    && entry.inputs.matches(sheet, full_key, allow_formulas)
                {
                    entry.version = sheet.cell_version().clone();
                    return Some((entry.index.clone(), 0));
                }
                if base_candidate.is_none()
                    && let Keys::Column { pages, .. } = &entry.inputs
                {
                    base_candidate = Some((pages.clone(), entry.index.clone()));
                }
            }
        }

        let slabs = column_slabs(sheet, r1, r2, col);
        let first_page = r1 / PAGE_ROWS;
        let total_pages = slabs.len();

        // 2. Incremental page-delta patch if a prior snapshot shares most 256-row slabs
        if let Some((old_pages, old_index)) = base_candidate
            && old_pages.len() == total_pages
        {
            let mut changed_indices = Vec::new();
            for (idx, (old, now_slab)) in old_pages.iter().zip(slabs.iter()).enumerate() {
                let same_ptr = match (&old.slab, now_slab) {
                    (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                    (None, None) => true,
                    _ => false,
                };
                if !same_ptr {
                    changed_indices.push(idx);
                }
            }
            if (total_pages >= 8 || scope.is_some())
                && !changed_indices.is_empty()
                && changed_indices.len() * 4 <= total_pages.max(4)
            {
                let mut new_pages = old_pages;
                let mut patched_map = (*old_index).clone();
                let mut reads = 0usize;
                let mut valid = true;
                for idx in changed_indices {
                    for (k, pos) in new_pages[idx].nonempty_keys.iter() {
                        patched_map.remove_pos(k, *pos);
                    }
                    let rebuilt =
                        build_page_key_record(first_page + idx as u32, r1, r2, slabs[idx].as_ref());
                    if (rebuilt.has_formula && !allow_formulas)
                        || rebuilt.has_invalid_formula_cache
                    {
                        valid = false;
                        break;
                    }
                    for (k, pos) in rebuilt.nonempty_keys.iter() {
                        patched_map.insert_next(k.clone(), *pos);
                    }
                    reads += PAGE_ROWS as usize;
                    new_pages[idx] = rebuilt;
                }
                if valid {
                    let has_formula = new_pages.iter().any(|p| p.has_formula);
                    if !has_formula || allow_formulas {
                        let empty_pos = first_blank_offset(&new_pages, r2 - r1 + 1);
                        patched_map.0.remove(&LookupKey::Empty);
                        if let Some((first_b, last_b)) = empty_pos {
                            patched_map.insert_next(LookupKey::Empty, first_b);
                            if last_b != first_b {
                                patched_map.insert_next(LookupKey::Empty, last_b);
                            }
                        }
                        let index = Arc::new(patched_map);
                        let inputs = Keys::Column {
                            r1,
                            r2,
                            col,
                            has_formula,
                            pages: new_pages,
                        };
                        let bytes = inputs.bytes() + index.heap_bytes();
                        if bytes <= BYTES
                            && let Ok(mut guard) = self.0.caches.lock()
                        {
                            while guard.exact.len() >= MAX_ENTRIES
                                || bytes + guard.exact.iter().map(|e| e.bytes).sum::<usize>()
                                    > BYTES
                            {
                                guard.exact.pop_front();
                            }
                            guard.exact.push_back(Entry {
                                inputs,
                                version: sheet.cell_version().clone(),
                                key: full_key,
                                has_formula,
                                bytes,
                                index: index.clone(),
                            });
                        }
                        return Some((index, reads.min((r2 - r1 + 1) as usize)));
                    }
                }
            }
        }

        // 3. Full page-wise build directly from Store slabs (zero Vec<FValue>, zero format!)
        let mut pages = Vec::with_capacity(total_pages);
        let mut has_formula = false;
        for (idx, slab) in slabs.iter().enumerate() {
            let rec = build_page_key_record(first_page + idx as u32, r1, r2, slab.as_ref());
            if (rec.has_formula && !allow_formulas) || rec.has_invalid_formula_cache {
                return None;
            }
            has_formula |= rec.has_formula;
            pages.push(rec);
        }
        let total_rows = (r2 - r1 + 1) as usize;
        let nonempty_total: usize = pages.iter().map(|p| p.nonempty_keys.len()).sum();
        let mut map = LookupMap::with_capacity(nonempty_total + 1);
        for page in &pages {
            for (k, pos) in page.nonempty_keys.iter() {
                map.insert_next(k.clone(), *pos);
            }
        }
        if let Some((first_b, last_b)) = first_blank_offset(&pages, r2 - r1 + 1) {
            map.insert_next(LookupKey::Empty, first_b);
            if last_b != first_b {
                map.insert_next(LookupKey::Empty, last_b);
            }
        }
        let index = Arc::new(map);
        let inputs = Keys::Column {
            r1,
            r2,
            col,
            has_formula,
            pages,
        };
        let bytes = inputs.bytes() + index.heap_bytes();
        if bytes <= BYTES
            && let Ok(mut guard) = self.0.caches.lock()
        {
            while guard.exact.len() >= MAX_ENTRIES
                || bytes + guard.exact.iter().map(|e| e.bytes).sum::<usize>() > BYTES
            {
                guard.exact.pop_front();
            }
            guard.exact.push_back(Entry {
                inputs,
                version: sheet.cell_version().clone(),
                key: full_key,
                has_formula,
                bytes,
                index: index.clone(),
            });
        }
        Some((index, total_rows))
    }

    /// Persistent 256-row page-block conditional column cache with O(N) incremental sorted merge
    /// and O(diff) incremental text-dictionary patching!
    pub(crate) fn get_or_build_cond_column(
        &self,
        sheet: &crate::Sheet,
        range: RangeKey,
        scope: Option<&HashSet<CellKey>>,
    ) -> Option<(Arc<SharedCondColumn>, usize)> {
        const MAX_COND_BYTES: usize = 192 * 1024 * 1024;
        const MAX_COND_COLS: usize = 24;

        let (si, r1_i, c1_i, r2_i, c2_i) = range;
        if c1_i != c2_i || r1_i < 0 || c1_i < 0 || r2_i < r1_i {
            return None;
        }
        let (r1, r2, col) = (r1_i as u32, r2_i as u32, c1_i as u32);
        let len = (r2 - r1 + 1) as usize;
        if !(256..=1_048_576).contains(&len) {
            return None;
        }
        let allow_formulas = scope.is_some_and(|s| {
            !s.iter()
                .any(|&(s_si, sr, sc)| s_si == si && sc == col && sr >= r1 && sr <= r2)
        });

        // Fast exact CellVersion check without taking build_mu
        if let Ok(guard) = self.0.caches.lock() {
            for entry in guard.cond.iter().rev().filter(|e| e.key == range) {
                if (!entry.col.has_formula || allow_formulas)
                    && entry.version == *sheet.cell_version()
                {
                    return Some((entry.col.clone(), 0));
                }
            }
        }

        // Single-flight serialize incremental merge / cold build across worker threads
        let _build_guard = self.0.build_mu.lock().ok()?;

        let mut base: Option<Arc<SharedCondColumn>> = None;
        if let Ok(guard) = self.0.caches.lock() {
            for entry in guard.cond.iter().rev().filter(|e| e.key == range) {
                if (!entry.col.has_formula || allow_formulas)
                    && entry.version == *sheet.cell_version()
                {
                    return Some((entry.col.clone(), 0));
                }
                if base.is_none() {
                    base = Some(entry.col.clone());
                }
            }
        }

        let slabs = column_slabs(sheet, r1, r2, col);
        let first_page = r1 / PAGE_ROWS;
        if let Some(prev) = &base
            && prev.pages.len() == slabs.len()
            && (!prev.has_formula || allow_formulas)
            && prev
                .pages
                .iter()
                .zip(slabs.iter())
                .all(|(p, now)| match (&p.slab, now) {
                    (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                    (None, None) => true,
                    _ => false,
                })
        {
            if let Ok(mut guard) = self.0.caches.lock()
                && let Some(entry) = guard
                    .cond
                    .iter_mut()
                    .rev()
                    .find(|e| Arc::ptr_eq(&e.col, prev))
            {
                entry.version = sheet.cell_version().clone();
            }
            return Some((prev.clone(), 0));
        }

        let mut pages = Vec::with_capacity(slabs.len());
        let mut page_offsets = Vec::with_capacity(slabs.len());
        let mut changed_indices = Vec::new();
        let mut has_formula = false;
        let mut has_non_finite = false;
        let mut has_non_num = false;
        let mut reads = 0usize;
        let mut row_cursor = 0u32;

        let can_reuse_base = base
            .as_ref()
            .is_some_and(|prev| prev.pages.len() == slabs.len());

        for (idx, slab) in slabs.iter().enumerate() {
            page_offsets.push(row_cursor);
            let reused = if can_reuse_base {
                base.as_ref().and_then(|prev| {
                    let old_p = prev.pages.get(idx)?;
                    let same = match (&old_p.slab, slab) {
                        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                        (None, None) => true,
                        _ => false,
                    };
                    same.then(|| old_p.clone())
                })
            } else {
                None
            };
            let page = match reused {
                Some(p) => p,
                None => {
                    changed_indices.push(idx);
                    let built = build_cond_page(first_page + idx as u32, r1, r2, slab.as_ref());
                    reads += built.rows_in_page as usize;
                    built
                }
            };
            if (page.has_formula && !allow_formulas) || page.has_invalid_formula_cache {
                return None;
            }
            has_formula |= page.has_formula;
            has_non_finite |= page.has_non_finite;
            has_non_num |= page.has_non_num;
            row_cursor += page.rows_in_page as u32;
            pages.push(page);
        }

        // Incrementally merge `sorted_nums` and patch `text_dict` when only a small fraction
        // of 256-row pages changed (and the previous snapshot already materialized them).
        let sorted_nums_cell = OnceLock::new();
        let text_dict_cell = OnceLock::new();

        if can_reuse_base
            && let Some(prev) = &base
            && !changed_indices.is_empty()
            && changed_indices.len() * 8 <= pages.len().max(8)
        {
            if !has_non_finite
                && let Some(Some(old_sorted)) = prev.sorted_nums.get()
            {
                let merged = SharedCondColumn::merge_sorted_nums_incremental(
                    old_sorted,
                    &pages,
                    &page_offsets,
                    &changed_indices,
                );
                let _ = sorted_nums_cell.set(Some(merged));
            }
            if let Some(Some(old_dict)) = prev.text_dict.get() {
                let patched = SharedCondColumn::patch_text_dict_incremental(
                    old_dict,
                    &prev.pages,
                    &pages,
                    &page_offsets,
                    &changed_indices,
                );
                let _ = text_dict_cell.set(patched);
            }
        }

        let built = Arc::new(SharedCondColumn {
            r1,
            r2,
            col,
            len,
            has_formula,
            has_non_finite,
            has_non_num,
            pages,
            page_offsets,
            sorted_nums: sorted_nums_cell,
            text_dict: text_dict_cell,
        });
        let bytes = built.approx_bytes();
        if bytes <= MAX_COND_BYTES
            && let Ok(mut guard) = self.0.caches.lock()
        {
            while guard.cond.len() >= MAX_COND_COLS
                || bytes + guard.cond.iter().map(|e| e.bytes).sum::<usize>() > MAX_COND_BYTES
            {
                guard.cond.pop_front();
            }
            guard.cond.push_back(CondEntry {
                version: sheet.cell_version().clone(),
                key: range,
                bytes,
                col: built.clone(),
            });
        }
        Some((built, reads))
    }
}

fn first_blank_offset(pages: &[PageKeyRecord], total_rows: u32) -> Option<(u32, u32)> {
    let nonempty_count: usize = pages.iter().map(|p| p.nonempty_keys.len()).sum();
    if nonempty_count >= total_rows as usize {
        return None;
    }
    let mut expected = 0u32;
    let mut first = None;
    for page in pages {
        for &(_, pos) in page.nonempty_keys.iter() {
            if pos > expected {
                first = Some(expected);
                break;
            }
            if pos == expected {
                expected += 1;
            }
        }
        if first.is_some() {
            break;
        }
    }
    let first = first.unwrap_or(expected);
    let mut expected_back = total_rows - 1;
    let mut last = None;
    for page in pages.iter().rev() {
        for &(_, pos) in page.nonempty_keys.iter().rev() {
            if pos < expected_back {
                last = Some(expected_back);
                break;
            }
            if pos == expected_back {
                if expected_back == 0 {
                    break;
                }
                expected_back -= 1;
            }
        }
        if last.is_some() {
            break;
        }
    }
    let last = last.unwrap_or(expected_back);
    Some((first, last))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Cell, Sheet, Value};

    #[test]
    fn mutations_outside_lookup_keys_reuse_the_index_in_divergent_snapshots() {
        for vertical in [false, true] {
            let value = |v| Cell {
                value: Value::Number(v),
                formula: None,
            };
            let mut sheet = Sheet::new("keys");
            sheet.set(0, 0, value(1.));
            let cache = SharedLookup::default();
            let key = ((0, 0, 0, 100, 3), vertical);
            let index = Index::default();
            cache.insert(&sheet, key, index.clone());
            let mut changed = sheet.clone();
            changed.set(30, 2, value(7.));
            changed.set(150, 0, value(8.));
            assert!(Arc::ptr_eq(&index, &cache.get(&changed, key).unwrap()));
            let (r, c) = if vertical { (30, 0) } else { (0, 2) };
            changed.set(r, c, value(3.));
            assert!(cache.get(&changed, key).is_none());
            assert!(Arc::ptr_eq(&index, &cache.get(&sheet, key).unwrap()));
        }
    }

    #[test]
    fn incremental_page_patch_updates_only_modified_page_and_preserves_prior_snapshot() {
        let mut sheet = Sheet::new("Data");
        for r in 0..2048u32 {
            sheet.set(
                r,
                0,
                Cell {
                    value: Value::Number(r as f64 * 10.0),
                    formula: None,
                },
            );
        }
        let range = (0, 0, 0, 2047, 0);
        let (idx1, reads1) = sheet
            .shared_lookup
            .get_or_build_vertical(&sheet, range, None)
            .unwrap();
        assert_eq!(reads1, 2048);
        assert_eq!(
            idx1.get_first(&LookupKey::from_fvalue(&FValue::Number(500.0))),
            Some(50)
        );

        let mut edited = sheet.clone();
        edited.set(
            50,
            0,
            Cell {
                value: Value::Number(999_999.0),
                formula: None,
            },
        );
        let (idx2, reads2) = edited
            .shared_lookup
            .get_or_build_vertical(&edited, range, None)
            .unwrap();
        assert_eq!(reads2, 256);
        assert_eq!(
            idx2.get_first(&LookupKey::from_fvalue(&FValue::Number(500.0))),
            None
        );
        assert_eq!(
            idx2.get_first(&LookupKey::from_fvalue(&FValue::Number(999_999.0))),
            Some(50)
        );
        let (idx_orig, reads_orig) = sheet
            .shared_lookup
            .get_or_build_vertical(&sheet, range, None)
            .unwrap();
        assert_eq!(reads_orig, 0);
        assert!(Arc::ptr_eq(&idx1, &idx_orig));
    }

    #[test]
    fn incremental_cond_column_merge_matches_full_sort_and_text_dict() {
        let mut sheet = Sheet::new("Data");
        for r in 0..2048u32 {
            sheet.set(
                r,
                0,
                Cell {
                    value: Value::Number((r % 50) as f64),
                    formula: None,
                },
            );
        }
        let range = (0, 0, 0, 2047, 0);
        let (c1, _) = sheet
            .shared_lookup
            .get_or_build_cond_column(&sheet, range, None)
            .unwrap();
        let _ = c1.sorted_nums().unwrap();
        let _ = c1.text_dict().unwrap();

        let mut edited = sheet.clone();
        edited.set(
            1000,
            0,
            Cell {
                value: Value::Number(99.0),
                formula: None,
            },
        );
        let (c2, reads2) = edited
            .shared_lookup
            .get_or_build_cond_column(&edited, range, None)
            .unwrap();
        assert_eq!(reads2, 256);
        let fresh_lookup = SharedLookup::default();
        let (fresh, _) = fresh_lookup
            .get_or_build_cond_column(&edited, range, None)
            .unwrap();
        assert_eq!(c2.sorted_nums().unwrap(), fresh.sorted_nums().unwrap());
        assert_eq!(c2.text_dict().unwrap(), fresh.text_dict().unwrap());
    }
}
