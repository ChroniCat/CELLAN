use super::fxhash::{FxBuildHasher, FxHashMap};
use super::{ErrKind, FValue, RangeKey, deps::CellKey};
use crate::store::{CellVersion, CellView, ColSlab, NumberSlab, PAGE_ROWS, row_range_mask};
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
    slab: Option<Arc<ColSlab>>,
    rows_in_page: u16,
    has_formula: bool,
    has_invalid_formula_cache: bool,
    nonempty_keys: Arc<Vec<(LookupKey, u32)>>,
}

#[inline]
fn expected_page_rows(page_idx: u32, r1: u32, r2: u32) -> u16 {
    let base = page_idx * PAGE_ROWS;
    let start = r1.saturating_sub(base) as usize;
    let end = ((r2 - base) as usize + 1).min(PAGE_ROWS as usize);
    end.saturating_sub(start) as u16
}

#[derive(Clone, Copy)]
pub(crate) struct LookupEvalScope<'a> {
    pub scope: Option<&'a HashSet<CellKey>>,
    pub fast_scope: Option<&'a super::FastScope>,
    pub overlay: Option<&'a dyn Fn(u32) -> Option<Value>>,
    pub cancel: Option<&'a std::sync::atomic::AtomicBool>,
    pub deadline: Option<std::time::Instant>,
}

impl<'a> LookupEvalScope<'a> {
    pub const UNSCOPED: Self = Self {
        scope: None,
        fast_scope: None,
        overlay: None,
        cancel: None,
        deadline: None,
    };

    #[inline]
    fn is_scoped(&self) -> bool {
        self.scope.is_some() || self.fast_scope.is_some()
    }

    #[inline]
    fn cell_in_scope(&self, key: CellKey) -> bool {
        if let Some(fs) = self.fast_scope {
            fs.contains(key)
        } else if let Some(s) = self.scope {
            s.contains(&key)
        } else {
            false
        }
    }

    #[inline]
    fn col_range_has_dirty(&self, si: usize, r1: u32, r2: u32, col: u32) -> bool {
        if let Some(fs) = self.fast_scope
            && !fs.col_may_be_dirty(si, col)
        {
            return false;
        }
        if let Some(s) = self.scope {
            if (r2 - r1) as usize <= s.len() / 2 {
                for r in r1..=r2 {
                    if self.cell_in_scope((si, r, col)) {
                        return true;
                    }
                }
                return false;
            }
            return s
                .iter()
                .any(|&(s_si, sr, sc)| s_si == si && sc == col && sr >= r1 && sr <= r2);
        }
        false
    }

    #[inline]
    fn page_has_dirty(&self, si: usize, page_idx: u32, r1: u32, r2: u32, col: u32) -> bool {
        if let Some(fs) = self.fast_scope
            && !fs.col_may_be_dirty(si, col)
        {
            return false;
        }
        let base = page_idx * PAGE_ROWS;
        let lo = r1.max(base);
        let hi = r2.min(base + PAGE_ROWS - 1);
        for r in lo..=hi {
            if self.cell_in_scope((si, r, col)) {
                return true;
            }
        }
        false
    }

    #[inline]
    fn is_cancelled(&self) -> bool {
        self.cancel
            .is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed))
            || self.deadline.is_some_and(|d| std::time::Instant::now() >= d)
    }
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
) -> impl Iterator<Item = ((u32, usize), CellView<'_>)> {
    pages.iter().flat_map(|p| {
        let end = p.end.min(p.cells.len());
        let start = p.start.min(end);
        (start..end).filter_map(move |row| {
            p.cells
                .get_view(row as u32)
                .map(|v| ((p.page, row), v))
        })
    })
}

fn same_occupied_keys(a: &[crate::cf::PageSlice], b: &[crate::cf::PageSlice]) -> bool {
    let mut ia = occupied_keys(a);
    let mut ib = occupied_keys(b);
    loop {
        match (ia.next(), ib.next()) {
            (Some((ka, va)), Some((kb, vb))) => {
                if ka != kb || va.formula() != vb.formula() || va.value() != vb.value() {
                    return false;
                }
            }
            (None, None) => return true,
            _ => return false,
        }
    }
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
    slab: Option<&Arc<ColSlab>>,
) -> PageKeyRecord {
    let base = page_idx * PAGE_ROWS;
    let start = r1.saturating_sub(base) as usize;
    let end = ((r2 - base) as usize + 1).min(PAGE_ROWS as usize);
    let rows_in_page = end.saturating_sub(start) as u16;
    let Some(cells) = slab else {
        return PageKeyRecord {
            slab: None,
            rows_in_page,
            has_formula: false,
            has_invalid_formula_cache: false,
            nonempty_keys: Arc::new(Vec::new()),
        };
    };
    let slice_end = end.min(cells.len());
    if start >= slice_end {
        return PageKeyRecord {
            slab: Some(cells.clone()),
            rows_in_page,
            has_formula: false,
            has_invalid_formula_cache: false,
            nonempty_keys: Arc::new(Vec::new()),
        };
    }
    let mut has_formula = false;
    let mut has_invalid = false;
    let mut out = Vec::with_capacity(slice_end - start);
    match cells.as_ref() {
        ColSlab::Numbers(ns) => {
            if NumberSlab::all_present_in(&ns.present, start, slice_end - start) {
                for (i, &n) in ns.values[start..slice_end].iter().enumerate() {
                    let ro = start + i;
                    let bits = if n == 0.0 { 0 } else { n.to_bits() };
                    out.push((LookupKey::Number(bits), base + ro as u32 - r1));
                }
            } else {
                for ro in start..slice_end {
                    if (ns.present[ro / 64] & (1u64 << (ro % 64))) != 0 {
                        let n = ns.values[ro];
                        let bits = if n == 0.0 { 0 } else { n.to_bits() };
                        out.push((LookupKey::Number(bits), base + ro as u32 - r1));
                    }
                }
            }
        }
        ColSlab::Values(vs) => {
            for ro in start..slice_end {
                let v = &vs[ro];
                if v.is_empty() {
                    continue;
                }
                let global_row = base + ro as u32;
                out.push((LookupKey::from_value(v), global_row - r1));
            }
        }
        ColSlab::Mixed(cs) => {
            for ro in start..slice_end {
                let cell = &cs[ro];
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
        }
    }
    PageKeyRecord {
        slab: Some(cells.clone()),
        rows_in_page,
        has_formula,
        has_invalid_formula_cache: has_invalid,
        nonempty_keys: Arc::new(out),
    }
}

fn build_page_key_record_overlay(
    si: usize,
    page_idx: u32,
    r1: u32,
    r2: u32,
    col: u32,
    slab: Option<&Arc<ColSlab>>,
    eval_scope: &LookupEvalScope<'_>,
) -> Option<PageKeyRecord> {
    let overlay_fn = eval_scope.overlay?;
    let base = page_idx * PAGE_ROWS;
    let start = r1.saturating_sub(base) as usize;
    let end = ((r2 - base) as usize + 1).min(PAGE_ROWS as usize);
    let rows_in_page = end.saturating_sub(start) as u16;
    let mut has_formula = false;
    let mut out = Vec::with_capacity(rows_in_page as usize);
    for ro in start..end {
        let abs_row = base + ro as u32;
        if eval_scope.cell_in_scope((si, abs_row, col)) {
            let val = overlay_fn(abs_row)?;
            has_formula = true;
            if !val.is_empty() {
                out.push((LookupKey::from_value(&val), abs_row - r1));
            }
            continue;
        }
        let Some(cells) = slab.map(Arc::as_ref) else {
            continue;
        };
        if ro >= cells.len() {
            continue;
        }
        match cells {
            ColSlab::Numbers(ns) => {
                if let Some(n) = ns.get(ro as u32) {
                    let bits = if n == 0.0 { 0 } else { n.to_bits() };
                    out.push((LookupKey::Number(bits), abs_row - r1));
                }
            }
            ColSlab::Values(vs) => {
                let v = &vs[ro];
                if !v.is_empty() {
                    out.push((LookupKey::from_value(v), abs_row - r1));
                }
            }
            ColSlab::Mixed(cs) => {
                let cell = &cs[ro];
                if cell.is_blank() {
                    continue;
                }
                if cell.formula.is_some() {
                    has_formula = true;
                    if invalid_formula_cache(cell) {
                        return None;
                    }
                }
                if !cell.value.is_empty() {
                    out.push((LookupKey::from_value(&cell.value), abs_row - r1));
                }
            }
        }
    }
    Some(PageKeyRecord {
        slab: None,
        rows_in_page,
        has_formula,
        has_invalid_formula_cache: false,
        nonempty_keys: Arc::new(out),
    })
}

fn column_slabs(
    sheet: &crate::Sheet,
    r1: u32,
    r2: u32,
    col: u32,
) -> Vec<Option<Arc<ColSlab>>> {
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
                    .map(|c| sheet.get(r1 as u32, c as u32).map(std::borrow::Cow::into_owned).unwrap_or_default())
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
                old == &now || same_occupied_keys(old, &now)
            }
            Self::Row(old) => (c1..=c2).zip(old).all(|(c, v)| {
                sheet
                    .get_view(r1 as u32, c as u32)
                    .map_or_else(|| v.is_blank(), |cell| cell.eq_cell(v))
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
                .map(|p| p.cells.payload_bytes() + std::mem::size_of::<crate::cf::PageSlice>())
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
    pub slab: Option<Arc<ColSlab>>,
    pub start_in_slab: u16,
    pub rows_in_page: u16,
    pub has_formula: bool,
    pub has_invalid_formula_cache: bool,
    pub has_non_num: bool,
    pub has_non_finite: bool,
    pub has_error: bool,
    pub has_empty: bool,
    pub has_coerced_non_strict: bool,
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
    /// Bitmask of non-empty cells in page (4 × 64 = 256 bits)
    pub nonempty_mask: [u64; 4],
    /// Pre-sorted finite coerced numbers `(value, local_row_offset)` within this 256-row page
    pub sorted_nums: Vec<(f64, u16)>,
    /// Optional pre-lowercased texts for transient dirty-overlay pages
    pub overlay_texts: Option<Vec<Arc<str>>>,
    /// Lazily materialized per-page lowercased text dictionary (only built when a text query runs!)
    page_text_dict: OnceLock<FxHashMap<Arc<str>, Vec<u16>>>,
}

impl CondPage {
    #[inline]
    pub(crate) fn has_coerced_num(&self, local: usize) -> bool {
        (self.coerced_ok[local / 64] & (1u64 << (local % 64))) != 0
    }

    #[inline]
    pub(crate) fn lower_text_at(&self, local: usize, empty_arc: &Arc<str>) -> Arc<str> {
        if let Some(ref ov) = self.overlay_texts {
            return ov.get(local).cloned().unwrap_or_else(|| empty_arc.clone());
        }
        let ro = self.start_in_slab as usize + local;
        match self.slab.as_deref() {
            None => empty_arc.clone(),
            Some(ColSlab::Numbers(ns)) => match ns.get(ro as u32) {
                Some(n) => crate::model::format_number(n).to_ascii_lowercase().into(),
                None => empty_arc.clone(),
            },
            Some(ColSlab::Values(vs)) => match vs.get(ro).filter(|v| !v.is_empty()) {
                None => empty_arc.clone(),
                Some(v) => value_to_coerced_lower_text(v),
            },
            Some(ColSlab::Mixed(cs)) => match cs.get(ro).filter(|c| !c.is_blank()) {
                None => empty_arc.clone(),
                Some(cell) => value_to_coerced_lower_text(&cell.value),
            },
        }
    }

    pub(crate) fn text_dict(&self) -> &FxHashMap<Arc<str>, Vec<u16>> {
        self.page_text_dict.get_or_init(|| {
            let mut dict: FxHashMap<Arc<str>, Vec<u16>> = FxHashMap::default();
            if let Some(ref ov) = self.overlay_texts {
                for (idx, txt) in ov.iter().enumerate() {
                    dict.entry(txt.clone()).or_default().push(idx as u16);
                }
                return dict;
            }
            let start = self.start_in_slab as usize;
            let empty_arc: Arc<str> = "".into();
            match self.slab.as_deref() {
                None => {
                    dict.insert(empty_arc, (0..self.rows_in_page).collect());
                }
                Some(ColSlab::Numbers(ns)) => {
                    for idx in 0..self.rows_in_page as usize {
                        let ro = start + idx;
                        let txt = if let Some(n) = ns.get(ro as u32) {
                            crate::model::format_number(n).into()
                        } else {
                            empty_arc.clone()
                        };
                        dict.entry(txt).or_default().push(idx as u16);
                    }
                }
                Some(ColSlab::Values(vs)) => {
                    for idx in 0..self.rows_in_page as usize {
                        let ro = start + idx;
                        let txt = match vs.get(ro).filter(|v| !v.is_empty()) {
                            None => empty_arc.clone(),
                            Some(v) => value_to_coerced_lower_text(v),
                        };
                        dict.entry(txt).or_default().push(idx as u16);
                    }
                }
                Some(ColSlab::Mixed(cs)) => {
                    for idx in 0..self.rows_in_page as usize {
                        let ro = start + idx;
                        let txt = match cs.get(ro).filter(|c| !c.is_blank()) {
                            None => empty_arc.clone(),
                            Some(cell) => value_to_coerced_lower_text(&cell.value),
                        };
                        dict.entry(txt).or_default().push(idx as u16);
                    }
                }
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

static FULL_EMPTY_COND_PAGE: std::sync::LazyLock<Arc<CondPage>> = std::sync::LazyLock::new(|| {
    let rows_in_page = PAGE_ROWS as u16;
    let coerced_nums = vec![0.0; PAGE_ROWS as usize];
    let coerced_ok = [u64::MAX; 4];
    let strict_num_ok = [0u64; 4];
    let nonempty_mask = [0u64; 4];
    let sorted_nums: Vec<(f64, u16)> = (0..rows_in_page).map(|idx| (0.0, idx)).collect();
    Arc::new(CondPage {
        slab: None,
        start_in_slab: 0,
        rows_in_page,
        has_formula: false,
        has_invalid_formula_cache: false,
        has_non_num: false,
        has_non_finite: false,
        has_error: false,
        has_empty: true,
        has_coerced_non_strict: false,
        all_exact_int: true,
        min_num: 0.0,
        max_num: 0.0,
        num_count: rows_in_page,
        int_sum: 0,
        int_abs_sum: 0,
        coerced_ok,
        strict_num_ok,
        nonempty_mask,
        coerced_nums,
        sorted_nums,
        overlay_texts: None,
        page_text_dict: OnceLock::new(),
    })
});

fn build_cond_page(
    page_idx: u32,
    r1: u32,
    r2: u32,
    slab: Option<&Arc<ColSlab>>,
) -> Arc<CondPage> {
    const EXACT: u64 = 1 << 53;
    let base = page_idx * PAGE_ROWS;
    let start = r1.saturating_sub(base) as usize;
    let end = ((r2 - base) as usize + 1).min(PAGE_ROWS as usize);
    if slab.is_none() && start == 0 && end == PAGE_ROWS as usize {
        return FULL_EMPTY_COND_PAGE.clone();
    }
    let rows_in_page = (end - start) as u16;
    let mut coerced_nums = vec![0.0; rows_in_page as usize];
    let mut coerced_ok = [0u64; 4];
    let mut strict_num_ok = [0u64; 4];
    let mut nonempty_mask = [0u64; 4];
    let mut sorted_nums = Vec::with_capacity(rows_in_page as usize);
    let mut has_formula = false;
    let mut has_invalid_formula_cache = false;
    let mut has_non_num = false;
    let mut has_non_finite = false;
    let mut has_error = false;
    let mut has_empty = false;
    let mut has_coerced_non_strict = false;
    let mut min_num = f64::INFINITY;
    let mut max_num = f64::NEG_INFINITY;
    let mut num_count = 0u16;
    let mut all_exact_int = true;
    let mut int_sum = 0i64;
    let mut int_abs_sum = 0u64;

    match slab.map(Arc::as_ref) {
        None => {
            if rows_in_page > 0 {
                has_empty = true;
                coerced_ok = row_range_mask(0, rows_in_page as u32 - 1);
                num_count = rows_in_page;
                min_num = 0.0;
                max_num = 0.0;
                for idx in 0..rows_in_page {
                    sorted_nums.push((0.0, idx));
                }
            }
        }
        Some(ColSlab::Numbers(ns)) => {
            let n_rows = rows_in_page as usize;
            if n_rows > 0 {
                coerced_ok = row_range_mask(0, rows_in_page as u32 - 1);
            }
            if end <= ns.values.len() && NumberSlab::all_present_in(&ns.present, start, n_rows) {
                // Dense pure numeric page: memcpy f64s + tight unbranched summary loop!
                coerced_nums.copy_from_slice(&ns.values[start..end]);
                strict_num_ok = coerced_ok;
                nonempty_mask = coerced_ok;
                for (idx, &n) in coerced_nums.iter().enumerate() {
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
            } else {
                for idx in 0..n_rows {
                    let ro = start + idx;
                    if let Some(n) = ns.get(ro as u32) {
                        let bit = 1u64 << (idx % 64);
                        strict_num_ok[idx / 64] |= bit;
                        nonempty_mask[idx / 64] |= bit;
                        coerced_nums[idx] = n;
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
                    } else {
                        has_empty = true;
                        num_count += 1;
                        min_num = min_num.min(0.0);
                        max_num = max_num.max(0.0);
                        sorted_nums.push((0.0, idx as u16));
                    }
                }
            }
        }
        Some(ColSlab::Values(vs)) => {
            for idx in 0..rows_in_page as usize {
                let ro = start + idx;
                let word = idx / 64;
                let bit = 1u64 << (idx % 64);
                match vs.get(ro).filter(|v| !v.is_empty()) {
                    None => {
                        has_empty = true;
                        coerced_ok[word] |= bit;
                        num_count += 1;
                        min_num = min_num.min(0.0);
                        max_num = max_num.max(0.0);
                        sorted_nums.push((0.0, idx as u16));
                    }
                    Some(val) => {
                        nonempty_mask[word] |= bit;
                        let is_strict = matches!(val, Value::Number(_));
                        if is_strict {
                            strict_num_ok[word] |= bit;
                        }
                        if matches!(val, Value::Error(_)) {
                            has_error = true;
                        }
                        match value_to_coerced_number(val) {
                            Ok(n) => {
                                if !is_strict {
                                    has_coerced_non_strict = true;
                                }
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
                                            int_abs_sum =
                                                int_abs_sum.saturating_add(w.unsigned_abs());
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
        }
        Some(ColSlab::Mixed(cs)) => {
            for idx in 0..rows_in_page as usize {
                let ro = start + idx;
                let word = idx / 64;
                let bit = 1u64 << (idx % 64);
                match cs.get(ro).filter(|c| !c.is_blank()) {
                    None => {
                        has_empty = true;
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
                        if cell.value.is_empty() {
                            has_empty = true;
                        } else {
                            nonempty_mask[word] |= bit;
                        }
                        let is_strict = matches!(cell.value, Value::Number(_));
                        if is_strict {
                            strict_num_ok[word] |= bit;
                        }
                        if matches!(cell.value, Value::Error(_)) {
                            has_error = true;
                        }
                        match value_to_coerced_number(&cell.value) {
                            Ok(n) => {
                                if !is_strict && !cell.value.is_empty() {
                                    has_coerced_non_strict = true;
                                }
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
                                            int_abs_sum =
                                                int_abs_sum.saturating_add(w.unsigned_abs());
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
        has_error,
        has_empty,
        has_coerced_non_strict,
        min_num,
        max_num,
        num_count,
        all_exact_int,
        int_sum,
        int_abs_sum,
        coerced_nums,
        coerced_ok,
        strict_num_ok,
        nonempty_mask,
        sorted_nums,
        overlay_texts: None,
        page_text_dict: OnceLock::new(),
    })
}

fn build_cond_page_overlay(
    si: usize,
    page_idx: u32,
    r1: u32,
    r2: u32,
    col: u32,
    slab: Option<&Arc<ColSlab>>,
    eval_scope: &LookupEvalScope<'_>,
) -> Option<Arc<CondPage>> {
    const EXACT: u64 = 1 << 53;
    let overlay_fn = eval_scope.overlay?;
    let base = page_idx * PAGE_ROWS;
    let start = r1.saturating_sub(base) as usize;
    let end = ((r2 - base) as usize + 1).min(PAGE_ROWS as usize);
    let rows_in_page = end.saturating_sub(start) as u16;
    let mut coerced_nums = vec![0.0; rows_in_page as usize];
    let mut coerced_ok = [0u64; 4];
    let mut strict_num_ok = [0u64; 4];
    let mut nonempty_mask = [0u64; 4];
    let mut sorted_nums = Vec::with_capacity(rows_in_page as usize);
    let empty_arc: Arc<str> = "".into();
    let mut overlay_texts = Vec::with_capacity(rows_in_page as usize);
    let mut has_formula = false;
    let mut has_non_num = false;
    let mut has_non_finite = false;
    let mut has_error = false;
    let mut has_empty = false;
    let mut has_coerced_non_strict = false;
    let mut min_num = f64::INFINITY;
    let mut max_num = f64::NEG_INFINITY;
    let mut num_count = 0u16;
    let mut all_exact_int = true;
    let mut int_sum = 0i64;
    let mut int_abs_sum = 0u64;

    for idx in 0..rows_in_page as usize {
        let ro = start + idx;
        let abs_row = base + ro as u32;
        let word = idx / 64;
        let bit = 1u64 << (idx % 64);

        let resolved: Value = if eval_scope.cell_in_scope((si, abs_row, col)) {
            has_formula = true;
            overlay_fn(abs_row)?
        } else {
            match slab.map(Arc::as_ref) {
                None => Value::Empty,
                Some(ColSlab::Numbers(ns)) => ns
                    .get(ro as u32)
                    .map(Value::Number)
                    .unwrap_or(Value::Empty),
                Some(ColSlab::Values(vs)) => vs.get(ro).cloned().unwrap_or(Value::Empty),
                Some(ColSlab::Mixed(cs)) => match cs.get(ro).filter(|c| !c.is_blank()) {
                    None => Value::Empty,
                    Some(cell) => {
                        if cell.formula.is_some() {
                            has_formula = true;
                            if invalid_formula_cache(cell) {
                                return None;
                            }
                        }
                        cell.value.clone()
                    }
                },
            }
        };

        if resolved.is_empty() {
            has_empty = true;
            coerced_ok[word] |= bit;
            num_count += 1;
            min_num = min_num.min(0.0);
            max_num = max_num.max(0.0);
            sorted_nums.push((0.0, idx as u16));
            overlay_texts.push(empty_arc.clone());
        } else {
            nonempty_mask[word] |= bit;
            overlay_texts.push(value_to_coerced_lower_text(&resolved));
            let is_strict = matches!(resolved, Value::Number(_));
            if is_strict {
                strict_num_ok[word] |= bit;
            }
            if matches!(resolved, Value::Error(_)) {
                has_error = true;
            }
            match value_to_coerced_number(&resolved) {
                Ok(n) => {
                    if !is_strict {
                        has_coerced_non_strict = true;
                    }
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

    if !has_non_finite {
        sorted_nums.sort_unstable_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    } else {
        sorted_nums.clear();
    }

    Some(Arc::new(CondPage {
        slab: None,
        start_in_slab: start as u16,
        rows_in_page,
        has_formula,
        has_invalid_formula_cache: false,
        has_non_num,
        has_non_finite,
        has_error,
        has_empty,
        has_coerced_non_strict,
        min_num,
        max_num,
        num_count,
        all_exact_int,
        int_sum,
        int_abs_sum,
        coerced_nums,
        coerced_ok,
        strict_num_ok,
        nonempty_mask,
        sorted_nums,
        overlay_texts: Some(overlay_texts),
        page_text_dict: OnceLock::new(),
    }))
}

#[derive(Debug)]
pub(crate) struct SharedCondColumn {
    pub r1: u32,
    pub r2: u32,
    pub col: u32,
    pub len: usize,
    pub is_transient_overlay: bool,
    pub has_formula: bool,
    pub has_non_finite: bool,
    pub has_non_num: bool,
    pub has_error: bool,
    pub has_empty: bool,
    pub has_coerced_non_strict: bool,
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

    /// O(N) linear streaming merge of `old_sorted` (dropping entries from changed/shrunk pages)
    /// with the pre-sorted entries from `changed_pages` (including appended tail pages),
    /// avoiding O(N log N) full re-sort!
    fn merge_sorted_nums_incremental(
        old_sorted: &[(f64, u32)],
        old_pages: &[Arc<CondPage>],
        pages: &[Arc<CondPage>],
        page_offsets: &[u32],
        changed_indices: &[usize],
        new_len: u32,
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

        let mut out = Vec::with_capacity((new_len as usize).min(old_sorted.len() + delta.len() + 256));
        let mut di = 0usize;

        if changed_indices.len() == 1 {
            // Hot path: single edited or tail-grown page `[lo, hi)` -> branch-predicted filter + hoisted merge
            let idx = changed_indices[0];
            let lo = page_offsets[idx];
            let old_rows = old_pages.get(idx).map_or(0, |p| p.rows_in_page as u32);
            let hi = lo + (pages[idx].rows_in_page as u32).max(old_rows);
            let dlen = delta.len();
            if dlen == 0 {
                for &(ov, orow) in old_sorted {
                    if orow < new_len && (orow < lo || orow >= hi) {
                        out.push((ov, orow));
                    }
                }
            } else {
                let mut cur_dv = delta[0].0;
                let mut cur_dr = delta[0].1;
                for &(ov, orow) in old_sorted {
                    if orow >= new_len || (orow >= lo && orow < hi) {
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
                    let old_rows = old_pages.get(idx).map_or(0, |p| p.rows_in_page as u32);
                    (lo, lo + (pages[idx].rows_in_page as u32).max(old_rows))
                })
                .collect();
            changed_ranges.sort_unstable();
            let in_changed = |row: u32| -> bool {
                let p = changed_ranges.partition_point(|&(_, hi)| hi <= row);
                p < changed_ranges.len() && row >= changed_ranges[p].0 && row < changed_ranges[p].1
            };
            for &(ov, orow) in old_sorted {
                if orow >= new_len || in_changed(orow) {
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

    /// Incrementally patch `old_dict` for ONLY the keys that changed inside `changed_indices`
    /// (including tail page growth/shrink), cloning only the outer map + CoW `Arc::make_mut`!
    fn patch_text_dict_incremental(
        old_dict: &TextDict,
        old_pages: &[Arc<CondPage>],
        old_offsets: &[u32],
        new_pages: &[Arc<CondPage>],
        page_offsets: &[u32],
        changed_indices: &[usize],
    ) -> Option<Arc<TextDict>> {
        let mut dict = old_dict.clone();
        let mut dict_bytes: usize = dict.keys().map(|k| k.len()).sum();

        // 0. If tail pages were shrunk away (`old_pages.len() > new_pages.len()`), truncate their rows
        if old_pages.len() > new_pages.len() {
            let cut_lo = old_offsets[new_pages.len()];
            for rem_idx in new_pages.len()..old_pages.len() {
                for txt in old_pages[rem_idx].text_dict().keys() {
                    if let Some(arc_list) = dict.get_mut(txt) {
                        let list = Arc::make_mut(arc_list);
                        let keep = list.partition_point(|&r| r < cut_lo);
                        if keep < list.len() {
                            list.truncate(keep);
                        }
                        if list.is_empty() {
                            dict_bytes = dict_bytes.saturating_sub(txt.len());
                            dict.remove(txt);
                        }
                    }
                }
            }
        }

        for &idx in changed_indices {
            let lo = page_offsets[idx];
            let new_pd = new_pages[idx].text_dict();

            if let Some(old_page) = old_pages.get(idx) {
                let hi = lo + (new_pages[idx].rows_in_page.max(old_page.rows_in_page)) as u32;
                let old_pd = old_page.text_dict();

                // 1. Handle keys present in old page that disappeared or changed local offsets
                for (txt, old_locals) in old_pd {
                    if new_pd.get(txt) == Some(old_locals) {
                        continue;
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
                        continue;
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
            } else {
                // Newly appended tail page (`idx >= old_pages.len()`): pure O(1) back-extend!
                for (txt, new_locals) in new_pd {
                    if let Some(arc_list) = dict.get_mut(txt) {
                        let list = Arc::make_mut(arc_list);
                        list.extend(new_locals.iter().map(|&off| lo + off as u32));
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
    pub(crate) fn coerced_lower_text_at(&self, row_offset: usize, empty_arc: &Arc<str>) -> Arc<str> {
        let abs_row = self.r1 + row_offset as u32;
        let p_idx = ((abs_row >> 8) - (self.r1 >> 8)) as usize;
        let Some(page) = self.pages.get(p_idx) else {
            return empty_arc.clone();
        };
        let local = (abs_row & 255) as usize - page.start_in_slab as usize;
        page.lower_text_at(local, empty_arc)
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
    pub(crate) fn strict_number_at(&self, row_offset: usize) -> f64 {
        let abs_row = self.r1 + row_offset as u32;
        let p_idx = ((abs_row >> 8) - (self.r1 >> 8)) as usize;
        let Some(page) = self.pages.get(p_idx) else {
            return 0.0;
        };
        let local = (abs_row & 255) as usize - page.start_in_slab as usize;
        if (page.strict_num_ok[local >> 6] & (1u64 << (local & 63))) != 0 {
            page.coerced_nums[local]
        } else {
            0.0
        }
    }

    #[inline]
    pub(crate) fn is_non_empty_at(&self, row_offset: usize) -> bool {
        let abs_row = self.r1 + row_offset as u32;
        let p_idx = ((abs_row >> 8) - (self.r1 >> 8)) as usize;
        let Some(page) = self.pages.get(p_idx) else {
            return false;
        };
        let local = (abs_row & 255) as usize - page.start_in_slab as usize;
        (page.nonempty_mask[local >> 6] & (1u64 << (local & 63))) != 0
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

#[derive(Debug)]
struct OrderedEntry {
    val_range: Option<RangeKey>,
    cond_range: RangeKey,
    need_minmax: bool,
    cond_col: Arc<SharedCondColumn>,
    val_col: Option<Arc<SharedCondColumn>>,
    built: Arc<super::conditional_index::BuiltOrdered>,
}

#[derive(Debug)]
struct GroupedEntry {
    val_range: Option<RangeKey>,
    cond_ranges: Vec<RangeKey>,
    numeric: Vec<bool>,
    cond_cols: Vec<Arc<SharedCondColumn>>,
    val_col: Option<Arc<SharedCondColumn>>,
    built: Arc<super::functions::BuiltGroups>,
}

#[derive(Debug, Default)]
struct InnerCaches {
    exact: VecDeque<Entry>,
    cond: VecDeque<CondEntry>,
    ordered: VecDeque<OrderedEntry>,
    grouped: VecDeque<GroupedEntry>,
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

    /// Returns true if a warm `SharedCondColumn` (even from a prior snapshot or prior tail length)
    /// exists on the same `(sheet, r1, col)`.
    pub(crate) fn has_cond_column(&self, range: RangeKey) -> bool {
        self.0.caches.lock().ok().is_some_and(|g| {
            g.cond.iter().any(|e| {
                e.key == range
                    || (e.key.0 == range.0
                        && e.key.1 == range.1
                        && e.key.2 == range.2
                        && e.key.4 == range.4)
            })
        })
    }

    /// Persistent `BuiltOrdered` lookup/patcher across recalc passes!
    /// - 0ns `Arc::clone` when `cond_sc` and `val_sc` match by `Arc::ptr_eq`;
    /// - O(changed_rows * log M + blocks) delta patch when `cond_sc` is unchanged and `<= 4` `val_sc` pages changed;
    /// - Zero-sort `build_from_cond` otherwise.
    pub(crate) fn get_or_build_ordered_persistent(
        &self,
        val_range: Option<RangeKey>,
        cond_range: RangeKey,
        need_minmax: bool,
        cond_sc: &Arc<SharedCondColumn>,
        val_sc: Option<&Arc<SharedCondColumn>>,
    ) -> Option<Arc<super::conditional_index::BuiltOrdered>> {
        const MAX_ORDERED: usize = 16;
        let can_persist = !cond_sc.is_transient_overlay
            && val_sc.is_none_or(|v| !v.is_transient_overlay);

        let mut patch_base: Option<(
            Arc<SharedCondColumn>,
            Arc<super::conditional_index::BuiltOrdered>,
        )> = None;

        if let Ok(mut guard) = self.0.caches.lock() {
            if let Some(pos) = guard.ordered.iter().rposition(|e| {
                e.val_range == val_range
                    && e.cond_range == cond_range
                    && e.need_minmax == need_minmax
                    && Arc::ptr_eq(&e.cond_col, cond_sc)
                    && match (&e.val_col, val_sc) {
                        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                        (None, None) => true,
                        _ => false,
                    }
            }) {
                let entry = guard.ordered.remove(pos).unwrap();
                let hit = entry.built.clone();
                guard.ordered.push_back(entry);
                return Some(hit);
            }
            if let Some(new_vc) = val_sc
                && let Some(prev) = guard.ordered.iter().rev().find(|e| {
                    e.cond_range == cond_range
                        && e.need_minmax == need_minmax
                        && e.val_range.is_some_and(|ovr| {
                            ovr.0 == val_range.unwrap().0
                                && ovr.1 == val_range.unwrap().1
                                && ovr.2 == val_range.unwrap().2
                                && ovr.4 == val_range.unwrap().4
                        })
                        && Arc::ptr_eq(&e.cond_col, cond_sc)
                        && e.val_col.as_ref().is_some_and(|ovc| ovc.len == new_vc.len)
                })
                && let Some(ref old_vc) = prev.val_col
            {
                patch_base = Some((old_vc.clone(), prev.built.clone()));
            }
        }

        let built = if let Some((old_vc, prev_built)) = patch_base
            && let Some(new_vc) = val_sc
            && let Some(patched) =
                prev_built.try_patch_values_from_cond(cond_sc, &old_vc, new_vc, need_minmax)
        {
            Arc::new(patched)
        } else {
            Arc::new(super::conditional_index::BuiltOrdered::build_from_cond(
                cond_sc,
                val_sc.map(Arc::as_ref),
                need_minmax,
            )?)
        };

        if can_persist && let Ok(mut guard) = self.0.caches.lock() {
            guard.ordered.retain(|e| {
                !(e.val_range == val_range
                    && e.cond_range == cond_range
                    && e.need_minmax == need_minmax)
            });
            while guard.ordered.len() >= MAX_ORDERED {
                guard.ordered.pop_front();
            }
            guard.ordered.push_back(OrderedEntry {
                val_range,
                cond_range,
                need_minmax,
                cond_col: cond_sc.clone(),
                val_col: val_sc.cloned(),
                built: built.clone(),
            });
        }
        Some(built)
    }

    /// Persistent `BuiltGroups` lookup across recalc passes via `SharedCondColumn` `Arc::ptr_eq`!
    pub(crate) fn get_grouped_persistent(
        &self,
        val_range: Option<RangeKey>,
        cond_ranges: &[RangeKey],
        numeric: &[bool],
        cond_cols: &[Arc<SharedCondColumn>],
        val_col: Option<&Arc<SharedCondColumn>>,
    ) -> Option<Arc<super::functions::BuiltGroups>> {
        let mut guard = self.0.caches.lock().ok()?;
        let pos = guard.grouped.iter().rposition(|e| {
            e.val_range == val_range
                && e.cond_ranges == cond_ranges
                && e.numeric == numeric
                && e.cond_cols.len() == cond_cols.len()
                && e.cond_cols
                    .iter()
                    .zip(cond_cols.iter())
                    .all(|(a, b)| Arc::ptr_eq(a, b))
                && match (&e.val_col, val_col) {
                    (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                    (None, None) => true,
                    _ => false,
                }
        })?;
        let entry = guard.grouped.remove(pos)?;
        let hit = entry.built.clone();
        guard.grouped.push_back(entry);
        Some(hit)
    }

    pub(crate) fn put_grouped_persistent(
        &self,
        val_range: Option<RangeKey>,
        cond_ranges: Vec<RangeKey>,
        numeric: Vec<bool>,
        cond_cols: Vec<Arc<SharedCondColumn>>,
        val_col: Option<Arc<SharedCondColumn>>,
        built: Arc<super::functions::BuiltGroups>,
    ) {
        const MAX_GROUPED: usize = 8;
        if cond_cols.iter().any(|c| c.is_transient_overlay)
            || val_col.as_ref().is_some_and(|v| v.is_transient_overlay)
        {
            return;
        }
        let Ok(mut guard) = self.0.caches.lock() else {
            return;
        };
        guard.grouped.retain(|e| {
            !(e.val_range == val_range
                && e.cond_ranges == cond_ranges
                && e.numeric == numeric)
        });
        while guard.grouped.len() >= MAX_GROUPED
            || guard.grouped.iter().map(|e| e.built.map.len()).sum::<usize>() + built.map.len()
                > 65_536
        {
            if guard.grouped.pop_front().is_none() {
                break;
            }
        }
        guard.grouped.push_back(GroupedEntry {
            val_range,
            cond_ranges,
            numeric,
            cond_cols,
            val_col,
            built,
        });
    }

    /// Page-aware lookup builder/updater for single-column vertical lookups.
    /// Reuses unchanged 256-row slabs via `Arc::ptr_eq`, incrementally patches tail row
    /// growth/shrink (`old_pages.len() != total_pages`), and supports page-level dirty formula overlays!
    pub(super) fn get_or_build_vertical(
        &self,
        sheet: &crate::Sheet,
        range: RangeKey,
        eval_scope: &LookupEvalScope<'_>,
    ) -> Option<(Index, usize)> {
        const BYTES: usize = 128 * 1024 * 1024;
        const MAX_ENTRIES: usize = 32;

        let (si, r1_i, c1_i, r2_i, c2_i) = range;
        if c1_i != c2_i || r1_i < 0 || c1_i < 0 || r2_i < r1_i {
            return None;
        }
        if eval_scope.is_cancelled() {
            return None;
        }
        let (r1, r2, col) = (r1_i as u32, r2_i as u32, c1_i as u32);
        let is_scoped = eval_scope.is_scoped();
        let has_dirty_in_range = is_scoped && eval_scope.col_range_has_dirty(si, r1, r2, col);
        let allow_formulas = is_scoped && !has_dirty_in_range;
        let full_key: Key = (range, true);

        // 1. Fast lock check for exact CellVersion hit or zero-change slab hit (when no dirty overlay needed)
        if !has_dirty_in_range && let Ok(mut guard) = self.0.caches.lock() {
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

        if has_dirty_in_range && eval_scope.overlay.is_none() {
            return None;
        }

        // Single-flight serialize cold/delta rebuild so concurrent workers don't duplicate work
        let _build_guard = self.0.build_mu.lock().ok()?;

        // Re-check after acquiring build_mu and select best base candidate (exact range first, then same `(si, r1, col)` for tail growth/shrink!)
        let mut base_candidate: Option<(Vec<PageKeyRecord>, Index)> = None;
        let mut same_col_base: Option<(Vec<PageKeyRecord>, Index)> = None;
        if let Ok(mut guard) = self.0.caches.lock() {
            for entry in guard.exact.iter_mut().rev() {
                if !has_dirty_in_range && entry.key == full_key {
                    if (!entry.has_formula || allow_formulas)
                        && entry.version == *sheet.cell_version()
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
                if let Keys::Column { pages, .. } = &entry.inputs {
                    if entry.key == full_key && base_candidate.is_none() {
                        base_candidate = Some((pages.clone(), entry.index.clone()));
                    } else if same_col_base.is_none()
                        && entry.key.1
                        && entry.key.0.0 == si
                        && entry.key.0.1 == r1_i
                        && entry.key.0.2 == c1_i
                        && entry.key.0.4 == c2_i
                    {
                        same_col_base = Some((pages.clone(), entry.index.clone()));
                    }
                }
            }
        }
        if base_candidate.is_none() {
            base_candidate = same_col_base;
        }

        let slabs = column_slabs(sheet, r1, r2, col);
        let first_page = r1 / PAGE_ROWS;
        let total_pages = slabs.len();

        // 2. Incremental page-delta patch (supports in-place edits, tail row growth/shrink, AND dirty overlays!)
        if let Some((old_pages, old_index)) = base_candidate {
            let old_len = old_pages.len();
            let common_len = old_len.min(total_pages);
            let mut changed_common = Vec::new();
            for idx in 0..common_len {
                let page_idx = first_page + idx as u32;
                let exp_rows = expected_page_rows(page_idx, r1, r2);
                let old = &old_pages[idx];
                let dirty_page =
                    has_dirty_in_range && eval_scope.page_has_dirty(si, page_idx, r1, r2, col);
                let same_ptr = !dirty_page
                    && old.rows_in_page == exp_rows
                    && match (&old.slab, &slabs[idx]) {
                        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                        (None, None) => true,
                        _ => false,
                    };
                if !same_ptr {
                    changed_common.push((idx, dirty_page));
                }
            }
            let tail_added = total_pages.saturating_sub(old_len);
            let tail_removed = old_len.saturating_sub(total_pages);
            let delta_pages = changed_common.len() + tail_added + tail_removed;
            let max_span = total_pages.max(old_len).max(4);
            if (total_pages >= 8 || is_scoped)
                && delta_pages > 0
                && delta_pages * 4 <= max_span
            {
                let mut new_pages = old_pages;
                let mut patched_map = (*old_index).clone();
                let mut reads = 0usize;
                let mut valid = true;

                // Remove keys from shrunk tail pages `[total_pages..old_len)`
                for idx in total_pages..old_len {
                    for (k, pos) in new_pages[idx].nonempty_keys.iter() {
                        patched_map.remove_pos(k, *pos);
                    }
                }
                new_pages.truncate(common_len);

                // Patch modified pages in `[0..common_len)`
                for (step, (idx, dirty_page)) in changed_common.into_iter().enumerate() {
                    if (step & 31) == 31 && eval_scope.is_cancelled() {
                        return None;
                    }
                    for (k, pos) in new_pages[idx].nonempty_keys.iter() {
                        patched_map.remove_pos(k, *pos);
                    }
                    let page_idx = first_page + idx as u32;
                    let rebuilt = if dirty_page {
                        let Some(r) = build_page_key_record_overlay(
                            si,
                            page_idx,
                            r1,
                            r2,
                            col,
                            slabs[idx].as_ref(),
                            eval_scope,
                        ) else {
                            valid = false;
                            break;
                        };
                        r
                    } else {
                        let r = build_page_key_record(page_idx, r1, r2, slabs[idx].as_ref());
                        if (r.has_formula && !is_scoped) || r.has_invalid_formula_cache {
                            valid = false;
                            break;
                        }
                        r
                    };
                    for (k, pos) in rebuilt.nonempty_keys.iter() {
                        patched_map.insert_next(k.clone(), *pos);
                    }
                    reads += rebuilt.rows_in_page as usize;
                    new_pages[idx] = rebuilt;
                }

                // Append grown tail pages `[old_len..total_pages)`
                if valid {
                    for idx in old_len..total_pages {
                        if ((idx - old_len) & 31) == 31 && eval_scope.is_cancelled() {
                            return None;
                        }
                        let page_idx = first_page + idx as u32;
                        let dirty_page = has_dirty_in_range
                            && eval_scope.page_has_dirty(si, page_idx, r1, r2, col);
                        let rebuilt = if dirty_page {
                            let Some(r) = build_page_key_record_overlay(
                                si,
                                page_idx,
                                r1,
                                r2,
                                col,
                                slabs[idx].as_ref(),
                                eval_scope,
                            ) else {
                                valid = false;
                                break;
                            };
                            r
                        } else {
                            let r = build_page_key_record(page_idx, r1, r2, slabs[idx].as_ref());
                            if (r.has_formula && !is_scoped) || r.has_invalid_formula_cache {
                                valid = false;
                                break;
                            }
                            r
                        };
                        for (k, pos) in rebuilt.nonempty_keys.iter() {
                            patched_map.insert_next(k.clone(), *pos);
                        }
                        reads += rebuilt.rows_in_page as usize;
                        new_pages.push(rebuilt);
                    }
                }

                if valid {
                    let has_formula = new_pages.iter().any(|p| p.has_formula);
                    if !has_formula || is_scoped {
                        let empty_pos = first_blank_offset(&new_pages, r2 - r1 + 1);
                        patched_map.0.remove(&LookupKey::Empty);
                        if let Some((first_b, last_b)) = empty_pos {
                            patched_map.insert_next(LookupKey::Empty, first_b);
                            if last_b != first_b {
                                patched_map.insert_next(LookupKey::Empty, last_b);
                            }
                        }
                        let index = Arc::new(patched_map);
                        if !has_dirty_in_range {
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
            if (idx & 31) == 31 && eval_scope.is_cancelled() {
                return None;
            }
            let page_idx = first_page + idx as u32;
            let dirty_page =
                has_dirty_in_range && eval_scope.page_has_dirty(si, page_idx, r1, r2, col);
            let rec = if dirty_page {
                build_page_key_record_overlay(
                    si,
                    page_idx,
                    r1,
                    r2,
                    col,
                    slab.as_ref(),
                    eval_scope,
                )?
            } else {
                let r = build_page_key_record(page_idx, r1, r2, slab.as_ref());
                if (r.has_formula && !is_scoped) || r.has_invalid_formula_cache {
                    return None;
                }
                r
            };
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
        if !has_dirty_in_range {
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
        }
        Some((index, total_rows))
    }

    /// Persistent 256-row page-block conditional column cache with O(N) incremental sorted merge,
    /// O(diff) incremental text-dictionary patching, tail row growth/shrink, and page-level dirty overlays!
    pub(crate) fn get_or_build_cond_column(
        &self,
        sheet: &crate::Sheet,
        range: RangeKey,
        eval_scope: &LookupEvalScope<'_>,
    ) -> Option<(Arc<SharedCondColumn>, usize)> {
        const MAX_COND_BYTES: usize = 192 * 1024 * 1024;
        const MAX_COND_COLS: usize = 24;

        let (si, r1_i, c1_i, r2_i, c2_i) = range;
        if c1_i != c2_i || r1_i < 0 || c1_i < 0 || r2_i < r1_i {
            return None;
        }
        if eval_scope.is_cancelled() {
            return None;
        }
        let (r1, r2, col) = (r1_i as u32, r2_i as u32, c1_i as u32);
        let len = (r2 - r1 + 1) as usize;
        if !(256..=1_048_576).contains(&len) {
            return None;
        }
        let is_scoped = eval_scope.is_scoped();
        let has_dirty_in_range = is_scoped && eval_scope.col_range_has_dirty(si, r1, r2, col);
        let allow_formulas = is_scoped && !has_dirty_in_range;

        // Fast exact CellVersion check without taking build_mu
        if !has_dirty_in_range && let Ok(guard) = self.0.caches.lock() {
            for entry in guard.cond.iter().rev().filter(|e| e.key == range) {
                if (!entry.col.has_formula || allow_formulas)
                    && entry.version == *sheet.cell_version()
                {
                    return Some((entry.col.clone(), 0));
                }
            }
        }

        if has_dirty_in_range && eval_scope.overlay.is_none() {
            return None;
        }

        // Single-flight serialize incremental merge / cold build across worker threads
        let _build_guard = self.0.build_mu.lock().ok()?;

        let mut base: Option<Arc<SharedCondColumn>> = None;
        let mut same_col_base: Option<Arc<SharedCondColumn>> = None;
        if let Ok(guard) = self.0.caches.lock() {
            for entry in guard.cond.iter().rev() {
                if entry.key == range {
                    if !has_dirty_in_range
                        && (!entry.col.has_formula || allow_formulas)
                        && entry.version == *sheet.cell_version()
                    {
                        return Some((entry.col.clone(), 0));
                    }
                    if base.is_none() {
                        base = Some(entry.col.clone());
                    }
                } else if same_col_base.is_none()
                    && entry.key.0 == si
                    && entry.key.1 == r1_i
                    && entry.key.2 == c1_i
                    && entry.key.4 == c2_i
                {
                    same_col_base = Some(entry.col.clone());
                }
            }
        }
        if base.is_none() {
            base = same_col_base;
        }

        let slabs = column_slabs(sheet, r1, r2, col);
        let first_page = r1 / PAGE_ROWS;
        if !has_dirty_in_range
            && let Some(prev) = &base
            && prev.len == len
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
                    .find(|e| e.key == range && Arc::ptr_eq(&e.col, prev))
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
        let mut has_error = false;
        let mut has_empty = false;
        let mut has_coerced_non_strict = false;
        let mut reads = 0usize;
        let mut row_cursor = 0u32;

        for (idx, slab) in slabs.iter().enumerate() {
            if (idx & 31) == 31 && eval_scope.is_cancelled() {
                return None;
            }
            page_offsets.push(row_cursor);
            let page_idx = first_page + idx as u32;
            let exp_rows = expected_page_rows(page_idx, r1, r2);
            let dirty_page =
                has_dirty_in_range && eval_scope.page_has_dirty(si, page_idx, r1, r2, col);
            let reused = if !dirty_page {
                base.as_ref().and_then(|prev| {
                    let old_p = prev.pages.get(idx)?;
                    let same = old_p.rows_in_page == exp_rows
                        && match (&old_p.slab, slab) {
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
                    let built = if dirty_page {
                        build_cond_page_overlay(
                            si,
                            page_idx,
                            r1,
                            r2,
                            col,
                            slab.as_ref(),
                            eval_scope,
                        )?
                    } else {
                        build_cond_page(page_idx, r1, r2, slab.as_ref())
                    };
                    reads += built.rows_in_page as usize;
                    built
                }
            };
            if !dirty_page && ((page.has_formula && !is_scoped) || page.has_invalid_formula_cache) {
                return None;
            }
            has_formula |= page.has_formula;
            has_non_finite |= page.has_non_finite;
            has_non_num |= page.has_non_num;
            has_error |= page.has_error;
            has_empty |= page.has_empty;
            has_coerced_non_strict |= page.has_coerced_non_strict;
            row_cursor += page.rows_in_page as u32;
            pages.push(page);
        }

        // Incrementally merge `sorted_nums` and patch `text_dict` when only a small fraction
        // of 256-row pages changed or grew/shrunk at the tail!
        let sorted_nums_cell = OnceLock::new();
        let text_dict_cell = OnceLock::new();

        if let Some(prev) = &base {
            let shrunk_pages = prev.pages.len().saturating_sub(pages.len());
            let total_delta = changed_indices.len() + shrunk_pages;
            let max_span = pages.len().max(prev.pages.len()).max(8);
            if total_delta > 0 && total_delta * 8 <= max_span {
                if !has_non_finite
                    && let Some(Some(old_sorted)) = prev.sorted_nums.get()
                {
                    let merged = SharedCondColumn::merge_sorted_nums_incremental(
                        old_sorted,
                        &prev.pages,
                        &pages,
                        &page_offsets,
                        &changed_indices,
                        len as u32,
                    );
                    let _ = sorted_nums_cell.set(Some(merged));
                }
                if let Some(Some(old_dict)) = prev.text_dict.get() {
                    let patched = SharedCondColumn::patch_text_dict_incremental(
                        old_dict,
                        &prev.pages,
                        &prev.page_offsets,
                        &pages,
                        &page_offsets,
                        &changed_indices,
                    );
                    let _ = text_dict_cell.set(patched);
                }
            }
        }

        let built = Arc::new(SharedCondColumn {
            r1,
            r2,
            col,
            len,
            is_transient_overlay: has_dirty_in_range,
            has_formula,
            has_non_finite,
            has_non_num,
            has_error,
            has_empty,
            has_coerced_non_strict,
            pages,
            page_offsets,
            sorted_nums: sorted_nums_cell,
            text_dict: text_dict_cell,
        });
        if !has_dirty_in_range {
            let bytes = built.approx_bytes();
            if bytes <= MAX_COND_BYTES
                && let Ok(mut guard) = self.0.caches.lock()
            {
                guard.cond.retain(|e| e.key != range);
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
            .get_or_build_vertical(&sheet, range, &LookupEvalScope::UNSCOPED)
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
            .get_or_build_vertical(&edited, range, &LookupEvalScope::UNSCOPED)
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
            .get_or_build_vertical(&sheet, range, &LookupEvalScope::UNSCOPED)
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
            .get_or_build_cond_column(&sheet, range, &LookupEvalScope::UNSCOPED)
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
            .get_or_build_cond_column(&edited, range, &LookupEvalScope::UNSCOPED)
            .unwrap();
        assert_eq!(reads2, 256);
        let fresh_lookup = SharedLookup::default();
        let (fresh, _) = fresh_lookup
            .get_or_build_cond_column(&edited, range, &LookupEvalScope::UNSCOPED)
            .unwrap();
        assert_eq!(c2.sorted_nums().unwrap(), fresh.sorted_nums().unwrap());
        assert_eq!(c2.text_dict().unwrap(), fresh.text_dict().unwrap());
    }
}
