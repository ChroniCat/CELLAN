//! 依赖图：记录每条公式引用了哪些区域，用于编辑后的增量重算。
//!
//! 关键设计：
//! 1. 单格引用进入按列有序点表 `points_by_col`，二分查找 `O(log N)`，零行桶哈希探测；
//! 2. 多格区域通过 `AreaId` 全局去重（10 万条公式引用同一 `$A$1:$CV$100000` 仅存 1 份列区间）；
//! 3. 宽区域（列宽 > 512，如整行引用 `5:5`）对称按行分桶 `wide_by_row`，避免每次 frontier 全扫；
//! 4. `FormulaRefs` 内联存储 0~2 个紧凑 `CompactRect`（20B），消除每条普通公式的独立 `Vec` 堆分配；
//! 5. 所有大型索引容器采用 `Arc` Base + 小增量 Overlay 的 CoW 结构，`Deps::clone()` 为 `O(1)`，
//!    后台持有 `Arc<Deps>` 快照期间 `Arc::make_mut(&mut deps).update_cell(...)` 仅修改单条补丁（`< 0.15 ms`）；
//! 6. 动态数组活跃溢出区与 `#SPILL!` 阻塞监视矩形直接编入依赖图，局部唤醒锚点，消除整簿全量回退。

use super::func_id::FuncId;
use super::fxhash::{FxBuildHasher, FxHashMap, FxHashSet};
use super::{Node, RefExpr};
use std::collections::HashSet;
use std::hash::Hash;
use std::sync::{Arc, Mutex};

pub type CellKey = (usize, u32, u32);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rect {
    pub sheet: usize,
    pub r1: i64,
    pub c1: i64,
    pub r2: i64,
    pub c2: i64,
}

impl Rect {
    pub fn contains(&self, key: CellKey) -> bool {
        key.0 == self.sheet
            && key.1 as i64 >= self.r1
            && key.1 as i64 <= self.r2
            && key.2 as i64 >= self.c1
            && key.2 as i64 <= self.c2
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct CompactRect {
    pub sheet: u32,
    pub r1: u32,
    pub c1: u32,
    pub r2: u32,
    pub c2: u32,
}

impl CompactRect {
    #[inline]
    fn from_rect(r: Rect) -> Option<Self> {
        if r.r1 > u32::MAX as i64 || r.c1 > u32::MAX as i64 {
            return None;
        }
        let sheet = u32::try_from(r.sheet).ok()?;
        let r1 = r.r1.max(0) as u32;
        let c1 = r.c1.max(0) as u32;
        let r2 = r.r2.max(0).min(u32::MAX as i64) as u32;
        let c2 = r.c2.max(0).min(u32::MAX as i64) as u32;
        (r1 <= r2 && c1 <= c2).then_some(Self {
            sheet,
            r1,
            c1,
            r2,
            c2,
        })
    }

    #[inline]
    pub fn to_rect(self) -> Rect {
        Rect {
            sheet: self.sheet as usize,
            r1: self.r1 as i64,
            c1: self.c1 as i64,
            r2: self.r2 as i64,
            c2: self.c2 as i64,
        }
    }

    #[inline]
    fn is_point(self) -> bool {
        self.r1 == self.r2 && self.c1 == self.c2
    }
}

/// Inline storage for 0..=2 compact rectangles to avoid 1 heap Vec allocation per formula.
#[derive(Clone, Debug, Default)]
pub enum FormulaRefs {
    #[default]
    Empty,
    One(CompactRect),
    Two([CompactRect; 2]),
    Many(Box<[CompactRect]>),
}

impl FormulaRefs {
    fn from_compact(v: Vec<CompactRect>) -> Self {
        match v.len() {
            0 => Self::Empty,
            1 => Self::One(v[0]),
            2 => Self::Two([v[0], v[1]]),
            _ => Self::Many(v.into_boxed_slice()),
        }
    }

    #[inline]
    pub fn as_slice(&self) -> &[CompactRect] {
        match self {
            Self::Empty => &[],
            Self::One(r) => std::slice::from_ref(r),
            Self::Two(pair) => pair.as_slice(),
            Self::Many(b) => b,
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.as_slice().len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
    }

    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = Rect> + '_ {
        self.as_slice().iter().copied().map(CompactRect::to_rect)
    }
}

// ============================================================================
// O(1) CoW Base + Small Overlay Containers
// ============================================================================

#[derive(Clone)]
struct CowMap<K, V> {
    base: Arc<FxHashMap<K, V>>,
    overlay: Arc<FxHashMap<K, Option<V>>>,
    len: usize,
}

impl<K, V> Default for CowMap<K, V> {
    fn default() -> Self {
        Self {
            base: Arc::new(FxHashMap::default()),
            overlay: Arc::new(FxHashMap::default()),
            len: 0,
        }
    }
}

impl<K: Eq + Hash + Clone, V: Clone> CowMap<K, V> {
    fn with_capacity(cap: usize) -> Self {
        Self {
            base: Arc::new(FxHashMap::with_capacity_and_hasher(cap, FxBuildHasher)),
            overlay: Arc::new(FxHashMap::default()),
            len: 0,
        }
    }

    #[inline]
    fn len(&self) -> usize {
        self.len
    }

    #[inline]
    fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline]
    fn get(&self, key: &K) -> Option<&V> {
        if !self.overlay.is_empty()
            && let Some(entry) = self.overlay.get(key)
        {
            return entry.as_ref();
        }
        self.base.get(key)
    }

    #[inline]
    fn contains_key(&self, key: &K) -> bool {
        self.get(key).is_some()
    }

    fn maybe_flush(&mut self) {
        let ov_len = self.overlay.len();
        if ov_len == 0 {
            return;
        }
        let unshared = Arc::strong_count(&self.base) == 1;
        if ov_len >= 2048 || (unshared && ov_len >= 256) {
            let base = Arc::make_mut(&mut self.base);
            let ov = std::mem::take(Arc::make_mut(&mut self.overlay));
            for (k, v_opt) in ov {
                match v_opt {
                    Some(v) => {
                        base.insert(k, v);
                    }
                    None => {
                        base.remove(&k);
                    }
                }
            }
            self.len = base.len();
        }
    }

    fn insert(&mut self, key: K, val: V) -> Option<V> {
        if self.overlay.is_empty() && Arc::strong_count(&self.base) == 1 {
            let base = Arc::get_mut(&mut self.base).unwrap();
            let old = base.insert(key, val);
            if old.is_none() {
                self.len += 1;
            }
            return old;
        }
        let old = {
            let ov = Arc::make_mut(&mut self.overlay);
            if let Some(slot) = ov.get_mut(&key) {
                let prev = slot.replace(val);
                if prev.is_none() {
                    self.len += 1;
                }
                prev
            } else {
                let prev = self.base.get(&key).cloned();
                if prev.is_none() {
                    self.len += 1;
                }
                ov.insert(key, Some(val));
                prev
            }
        };
        self.maybe_flush();
        old
    }

    fn remove(&mut self, key: &K) -> Option<V> {
        if self.overlay.is_empty() && Arc::strong_count(&self.base) == 1 {
            let base = Arc::get_mut(&mut self.base).unwrap();
            let old = base.remove(key);
            if old.is_some() {
                self.len -= 1;
            }
            return old;
        }
        // Fast bail-out if absent from both overlay and base (avoids cloning overlay)
        if !self.overlay.contains_key(key) && !self.base.contains_key(key) {
            return None;
        }
        let old = {
            let ov = Arc::make_mut(&mut self.overlay);
            if let Some(slot) = ov.get_mut(key) {
                let prev = slot.take();
                if prev.is_some() {
                    self.len -= 1;
                }
                prev
            } else {
                let prev = self.base.get(key).cloned();
                if prev.is_some() {
                    ov.insert(key.clone(), None);
                    self.len -= 1;
                }
                prev
            }
        };
        self.maybe_flush();
        old
    }

    /// Mutate an existing entry or insert a default value, copying ONLY that single entry
    /// into `overlay` when `base` is shared with another snapshot.
    fn get_or_default_mut(&mut self, key: K) -> &mut V
    where
        V: Default,
    {
        if self.overlay.is_empty() && Arc::strong_count(&self.base) == 1 {
            let base = Arc::get_mut(&mut self.base).unwrap();
            let mut inserted = false;
            let entry = base.entry(key).or_insert_with(|| {
                inserted = true;
                V::default()
            });
            if inserted {
                self.len += 1;
            }
            return entry;
        }
        let ov = Arc::make_mut(&mut self.overlay);
        let slot = ov.entry(key.clone()).or_insert_with(|| {
            match self.base.get(&key) {
                Some(v) => Some(v.clone()),
                None => {
                    self.len += 1;
                    Some(V::default())
                }
            }
        });
        if slot.is_none() {
            self.len += 1;
            *slot = Some(V::default());
        }
        slot.as_mut().unwrap()
    }

    fn flush_into_base(&mut self) {
        if self.overlay.is_empty() {
            return;
        }
        let base = Arc::make_mut(&mut self.base);
        let ov = std::mem::take(Arc::make_mut(&mut self.overlay));
        for (k, v_opt) in ov {
            match v_opt {
                Some(v) => {
                    base.insert(k, v);
                }
                None => {
                    base.remove(&k);
                }
            }
        }
        self.len = base.len();
    }

    fn base_mut_flushed(&mut self) -> &mut FxHashMap<K, V> {
        self.flush_into_base();
        Arc::make_mut(&mut self.base)
    }

    fn keys(&self) -> impl Iterator<Item = &K> + '_ {
        let base_keys = self
            .base
            .keys()
            .filter(|k| self.overlay.is_empty() || !self.overlay.contains_key(*k));
        let ov_keys = self
            .overlay
            .iter()
            .filter_map(|(k, v)| v.is_some().then_some(k));
        base_keys.chain(ov_keys)
    }

    fn iter(&self) -> impl Iterator<Item = (&K, &V)> + '_ {
        let base_it = self
            .base
            .iter()
            .filter(|(k, _)| self.overlay.is_empty() || !self.overlay.contains_key(*k));
        let ov_it = self
            .overlay
            .iter()
            .filter_map(|(k, v)| v.as_ref().map(|val| (k, val)));
        base_it.chain(ov_it)
    }
}

#[derive(Clone)]
struct CowSet<K> {
    inner: CowMap<K, ()>,
}

impl<K> Default for CowSet<K> {
    fn default() -> Self {
        Self {
            inner: CowMap::default(),
        }
    }
}

impl<K: Eq + Hash + Clone> CowSet<K> {
    #[inline]
    fn len(&self) -> usize {
        self.inner.len()
    }

    #[inline]
    fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    #[inline]
    fn contains(&self, key: &K) -> bool {
        self.inner.contains_key(key)
    }

    #[inline]
    fn insert(&mut self, key: K) -> bool {
        self.inner.insert(key, ()).is_none()
    }

    #[inline]
    fn remove(&mut self, key: &K) -> bool {
        self.inner.remove(key).is_some()
    }

    #[inline]
    fn iter(&self) -> impl Iterator<Item = &K> + '_ {
        self.inner.keys()
    }
}

/// Public CoW wrapper for `Deps::refs` preserving `.get()`, `.len()`, `.is_empty()`,
/// `.contains_key()`, `.keys()`, and `.iter()` compatibility.
#[derive(Clone, Default)]
pub struct RefMap {
    inner: CowMap<CellKey, FormulaRefs>,
}

impl RefMap {
    fn with_capacity(cap: usize) -> Self {
        Self {
            inner: CowMap::with_capacity(cap),
        }
    }

    #[inline]
    pub fn get(&self, key: &CellKey) -> Option<&FormulaRefs> {
        self.inner.get(key)
    }

    #[inline]
    pub fn contains_key(&self, key: &CellKey) -> bool {
        self.inner.contains_key(key)
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    #[inline]
    pub fn keys(&self) -> impl Iterator<Item = &CellKey> + '_ {
        self.inner.keys()
    }

    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = (&CellKey, &FormulaRefs)> + '_ {
        self.inner.iter()
    }

    #[inline]
    fn insert(&mut self, key: CellKey, val: FormulaRefs) -> Option<FormulaRefs> {
        self.inner.insert(key, val)
    }

    #[inline]
    fn remove(&mut self, key: &CellKey) -> Option<FormulaRefs> {
        self.inner.remove(key)
    }
}

#[derive(Clone, Default)]
struct SlotTable {
    base: Arc<Vec<CellKey>>,
    dead_base: Arc<FxHashSet<u32>>,
    extra: Arc<Vec<Option<CellKey>>>,
}

impl SlotTable {
    fn with_capacity(cap: usize) -> Self {
        Self {
            base: Arc::new(Vec::with_capacity(cap)),
            dead_base: Arc::new(FxHashSet::default()),
            extra: Arc::new(Vec::new()),
        }
    }

    #[inline]
    fn len(&self) -> usize {
        self.base.len() + self.extra.len()
    }

    #[inline]
    fn get(&self, id: u32) -> Option<CellKey> {
        let idx = id as usize;
        if idx < self.base.len() {
            if !self.dead_base.is_empty() && self.dead_base.contains(&id) {
                None
            } else {
                Some(self.base[idx])
            }
        } else {
            self.extra.get(idx - self.base.len()).copied().flatten()
        }
    }

    #[inline]
    fn is_live(&self, id: u32) -> bool {
        self.get(id).is_some()
    }

    fn alloc(&mut self, key: CellKey) -> u32 {
        if self.extra.is_empty() && Arc::strong_count(&self.base) == 1 {
            let base = Arc::get_mut(&mut self.base).unwrap();
            let id = u32::try_from(base.len()).expect("more than 2^32 indexed formulas");
            base.push(key);
            return id;
        }
        let extra = Arc::make_mut(&mut self.extra);
        let id =
            u32::try_from(self.base.len() + extra.len()).expect("more than 2^32 indexed formulas");
        extra.push(Some(key));
        id
    }

    fn tombstone(&mut self, id: u32) -> bool {
        let idx = id as usize;
        if idx < self.base.len() {
            Arc::make_mut(&mut self.dead_base).insert(id)
        } else if let Some(slot) = Arc::make_mut(&mut self.extra).get_mut(idx - self.base.len()) {
            slot.take().is_some()
        } else {
            false
        }
    }
}

#[derive(Clone)]
struct CowVec<T> {
    base: Arc<Vec<T>>,
    overlay: Arc<FxHashMap<usize, T>>,
    extra: Arc<Vec<T>>,
}

impl<T> Default for CowVec<T> {
    fn default() -> Self {
        Self {
            base: Arc::new(Vec::new()),
            overlay: Arc::new(FxHashMap::default()),
            extra: Arc::new(Vec::new()),
        }
    }
}

impl<T: Clone> CowVec<T> {
    #[inline]
    fn len(&self) -> usize {
        self.base.len() + self.extra.len()
    }

    #[inline]
    fn get(&self, idx: usize) -> Option<&T> {
        if idx < self.base.len() {
            if !self.overlay.is_empty()
                && let Some(v) = self.overlay.get(&idx)
            {
                return Some(v);
            }
            Some(&self.base[idx])
        } else {
            self.extra.get(idx - self.base.len())
        }
    }

    fn push(&mut self, val: T) -> usize {
        if self.extra.is_empty() && Arc::strong_count(&self.base) == 1 {
            let base = Arc::get_mut(&mut self.base).unwrap();
            let id = base.len();
            base.push(val);
            return id;
        }
        let extra = Arc::make_mut(&mut self.extra);
        let id = self.base.len() + extra.len();
        extra.push(val);
        id
    }

    fn get_mut(&mut self, idx: usize) -> &mut T {
        if idx < self.base.len() {
            if self.overlay.is_empty() && Arc::strong_count(&self.base) == 1 {
                return &mut Arc::get_mut(&mut self.base).unwrap()[idx];
            }
            let base = &self.base;
            Arc::make_mut(&mut self.overlay)
                .entry(idx)
                .or_insert_with(|| base[idx].clone())
        } else {
            &mut Arc::make_mut(&mut self.extra)[idx - self.base.len()]
        }
    }

    fn flush_into_base(&mut self) {
        if self.overlay.is_empty() && self.extra.is_empty() {
            return;
        }
        let base = Arc::make_mut(&mut self.base);
        let ov = std::mem::take(Arc::make_mut(&mut self.overlay));
        for (i, v) in ov {
            base[i] = v;
        }
        let extra = std::mem::take(Arc::make_mut(&mut self.extra));
        base.extend(extra);
    }

    fn as_mut_slice_flushed(&mut self) -> &mut [T] {
        self.flush_into_base();
        Arc::make_mut(&mut self.base).as_mut_slice()
    }
}

// ============================================================================
// Area & Point Indexes
// ============================================================================

// A multi-cell range reference is indexed once per unique AreaId per column (or per row bucket
// for wide ranges > 512 cols), in the smallest aligned row bucket containing both endpoints.
// A point lookup visits at most seven buckets.
const ROW_SHIFTS: [u32; 7] = [5, 10, 15, 20, 25, 30, 32];

#[inline]
fn bucket(row: u32, shift: u32) -> u32 {
    row.checked_shr(shift).unwrap_or(0)
}

#[inline]
fn row_bucket(first: u32, last: u32) -> (u32, u32) {
    let bits = u32::BITS - (first ^ last).leading_zeros();
    let shift = ROW_SHIFTS[ROW_SHIFTS.partition_point(|&s| s < bits)];
    (shift, bucket(first, shift))
}

#[derive(Clone, Copy)]
struct AreaInterval {
    area_id: u32,
    first: u32,
    last: u32,
}

#[derive(Clone, Copy)]
struct WideInterval {
    area_id: u32,
    r1: u32,
    r2: u32,
    c1: u32,
    c2: u32,
}

#[derive(Clone, Default)]
struct SharedArea {
    rect: CompactRect,
    subscribers: Arc<Vec<u32>>,
    extra_subscribers: Arc<Vec<u32>>,
}

impl SharedArea {
    fn new(rect: CompactRect, first_sub: u32) -> Self {
        Self {
            rect,
            subscribers: Arc::new(vec![first_sub]),
            extra_subscribers: Arc::new(Vec::new()),
        }
    }

    fn push_subscriber(&mut self, formula_id: u32) {
        if self.extra_subscribers.is_empty() && Arc::strong_count(&self.subscribers) == 1 {
            Arc::get_mut(&mut self.subscribers)
                .unwrap()
                .push(formula_id);
            return;
        }
        let extra = Arc::make_mut(&mut self.extra_subscribers);
        extra.push(formula_id);
        if extra.len() >= 256
            || (extra.len() >= 32 && Arc::strong_count(&self.subscribers) == 1)
        {
            let subs = Arc::make_mut(&mut self.subscribers);
            subs.extend_from_slice(extra);
            extra.clear();
        }
    }

    #[inline]
    fn visit_subscribers(&self, emit: &mut impl FnMut(u32) -> bool) -> bool {
        for &sub in self.subscribers.iter() {
            if !emit(sub) {
                return false;
            }
        }
        for &sub in self.extra_subscribers.iter() {
            if !emit(sub) {
                return false;
            }
        }
        true
    }

    fn retain_live(&mut self, slots: &SlotTable) {
        if !self.extra_subscribers.is_empty() {
            let extra = std::mem::take(Arc::make_mut(&mut self.extra_subscribers));
            Arc::make_mut(&mut self.subscribers).extend(extra);
        }
        Arc::make_mut(&mut self.subscribers).retain(|&id| slots.is_live(id));
    }
}

#[derive(Clone, Default)]
struct PointColumn {
    sorted: Arc<Vec<(u32, u32)>>,
    unsorted: Arc<Vec<(u32, u32)>>,
}

impl PointColumn {
    #[inline]
    fn push_bulk(&mut self, row: u32, formula_id: u32) {
        Arc::make_mut(&mut self.unsorted).push((row, formula_id));
    }

    #[inline]
    fn push_unsorted(&mut self, row: u32, formula_id: u32) {
        let unsorted = Arc::make_mut(&mut self.unsorted);
        unsorted.push((row, formula_id));
        if unsorted.len() >= 64 {
            self.seal();
        }
    }

    fn seal(&mut self) {
        if self.unsorted.is_empty() {
            return;
        }
        let mut unsorted = std::mem::take(Arc::make_mut(&mut self.unsorted));
        let sorted = Arc::make_mut(&mut self.sorted);
        if sorted.is_empty() {
            if !unsorted.is_sorted_by_key(|&(r, id)| (r, id)) {
                unsorted.sort_unstable_by_key(|&(row, id)| (row, id));
            }
            unsorted.dedup();
            *sorted = unsorted;
        } else {
            sorted.append(&mut unsorted);
            sorted.sort_unstable_by_key(|&(row, id)| (row, id));
            sorted.dedup();
        }
    }

    fn query_sorted_rows_with_cursor(
        &self,
        rows: &[CellKey],
        cursor: &mut usize,
        mut emit: impl FnMut(u32) -> bool,
    ) -> bool {
        if !self.sorted.is_empty() {
            let sorted = self.sorted.as_slice();
            let mut lo = if *cursor < sorted.len()
                && rows.first().is_some_and(|first| sorted[*cursor].0 <= first.1)
            {
                *cursor
            } else {
                0
            };
            for &(_, r, _) in rows {
                if lo < sorted.len() && sorted[lo].0 < r {
                    // Fast sequential / galloping step for linear column chains (`chain 1M`):
                    // avoids 20-step binary searches when consecutive waves advance by 1 row!
                    let probe = (lo + 4).min(sorted.len() - 1);
                    if sorted[probe].0 >= r {
                        while lo < sorted.len() && sorted[lo].0 < r {
                            lo += 1;
                        }
                    } else {
                        lo += sorted[lo..].partition_point(|&(pr, _)| pr < r);
                    }
                }
                let mut k = lo;
                while k < sorted.len() && sorted[k].0 == r {
                    if !emit(sorted[k].1) {
                        return false;
                    }
                    k += 1;
                }
                lo = k;
            }
            *cursor = lo;
        }
        if !self.unsorted.is_empty() {
            for &(pr, id) in self.unsorted.iter() {
                if rows.binary_search_by_key(&pr, |k| k.1).is_ok() && !emit(id) {
                    return false;
                }
            }
        }
        true
    }
}

#[derive(Clone, Debug)]
enum DynamicWatch {
    Empty,
    One(CompactRect),
    Many(Vec<CompactRect>),
}

impl DynamicWatch {
    fn push(&mut self, cr: CompactRect) -> bool {
        match self {
            Self::Empty => {
                *self = Self::One(cr);
                true
            }
            Self::One(existing) => {
                if *existing != cr {
                    *self = Self::Many(vec![*existing, cr]);
                    true
                } else {
                    false
                }
            }
            Self::Many(list) => {
                if !list.contains(&cr) {
                    list.push(cr);
                    true
                } else {
                    false
                }
            }
        }
    }

    fn as_slice(&self) -> &[CompactRect] {
        match self {
            Self::Empty => &[],
            Self::One(r) => std::slice::from_ref(r),
            Self::Many(v) => v.as_slice(),
        }
    }
}

/// Inverted index for resolved `OFFSET`/`INDIRECT` target rects so 50k formulas sharing one
/// dynamic named range check a single unique `CompactRect` in O(1) instead of scanning 50k entries.
#[derive(Clone, Debug, Default)]
struct DynamicWatchIndex {
    by_cell: FxHashMap<CellKey, DynamicWatch>,
    by_rect: FxHashMap<CompactRect, Vec<CellKey>>,
}

impl DynamicWatchIndex {
    fn remove(&mut self, key: CellKey) {
        if let Some(old) = self.by_cell.remove(&key) {
            for &cr in old.as_slice() {
                if let Some(list) = self.by_rect.get_mut(&cr) {
                    if let Some(pos) = list.iter().position(|&k| k == key) {
                        list.swap_remove(pos);
                    }
                    if list.is_empty() {
                        self.by_rect.remove(&cr);
                    }
                }
            }
        }
    }

    fn mark_evaluated(&mut self, key: CellKey) {
        self.by_cell.entry(key).or_insert(DynamicWatch::Empty);
    }

    fn record(&mut self, key: CellKey, cr: CompactRect) {
        let added = match self.by_cell.get_mut(&key) {
            Some(existing) => existing.push(cr),
            None => {
                self.by_cell.insert(key, DynamicWatch::One(cr));
                true
            }
        };
        if added {
            self.by_rect.entry(cr).or_default().push(key);
        }
    }
}

#[derive(Default, Clone)]
pub struct Deps {
    /// 公式单元格 → 引用区域。内联 0~2 个紧凑矩形以避免每条公式一次堆分配。
    pub refs: RefMap,
    formula_ids: CowMap<CellKey, u32>,
    slots: SlotTable,
    tombstones: usize,
    /// 单格引用列索引：(sheet, col) → 按行有序的 (row, formula_id)
    points_by_col: CowMap<(u32, u32), PointColumn>,
    /// 多格区域共享池：相同矩形仅在列桶/行桶登记一次
    area_map: CowMap<CompactRect, u32>,
    areas: CowVec<SharedArea>,
    /// 拥有至少一个多格区间的 (sheet, col)，让纯点引用链完全跳过 7 次行桶查询
    range_cols: CowSet<(u32, u32)>,
    /// (表, 列, 行桶位数, 桶号) → AreaInterval
    by_col: CowMap<(u32, u32, u32, u32), Arc<Vec<AreaInterval>>>,
    /// 拥有宽区域（列宽 > 512）的表下标，让普通表跳过宽区域行桶查询
    wide_sheets: CowSet<u32>,
    /// 宽区域按行分桶索引：(表, 行桶位数, 桶号) → WideInterval
    wide_by_row: CowMap<(u32, u32, u32), Arc<Vec<WideInterval>>>,
    /// 动态数组活跃溢出区与 `#SPILL!` 阻塞监视区：anchor -> (watch_formula_id, watch_rect)
    spill_watches: CowMap<CellKey, (u32, CompactRect)>,
    untracked_spill_errors: CowSet<CellKey>,
    has_spill_op_refs: bool,
    volatile: CowSet<CellKey>,
    today_only_cells: CowSet<CellKey>,
    volatile_refs: CowSet<CellKey>,
    visibility: CowSet<CellKey>,
    runtime_refs: CowSet<CellKey>,
    structural_sensitive: CowSet<CellKey>,
    indirect_definitions: CowSet<CellKey>,
    /// Last resolved target rectangle(s) of `OFFSET`/`INDIRECT` cells so edits & spill hops
    /// prune unrelated dynamic formulas in O(unique rects) instead of re-evaluating all dynamic cells.
    dynamic_watches: Arc<std::sync::RwLock<DynamicWatchIndex>>,
    computed_baseline: bool,
    pub rect_count: usize,
}

impl Deps {
    pub fn empty() -> Self {
        Deps::default()
    }

    pub fn is_empty(&self) -> bool {
        self.refs.is_empty()
            && self.volatile.is_empty()
            && self.volatile_refs.is_empty()
            && self.runtime_refs.is_empty()
            && self.spill_watches.is_empty()
            && self.untracked_spill_errors.is_empty()
            && self.visibility.is_empty()
    }

    pub fn requires_full_recalc(&self) -> bool {
        !self.runtime_refs.is_empty() || !self.untracked_spill_errors.is_empty()
    }

    #[inline]
    pub(crate) fn tracks_spill(&self, key: CellKey) -> bool {
        self.spill_watches.contains_key(&key)
    }

    #[inline]
    pub(crate) fn has_spill_op_refs(&self) -> bool {
        self.has_spill_op_refs
    }

    /// A value-to-value edit can share the existing immutable graph with a worker.
    pub fn cell_needs_update(&self, wb: &crate::Workbook, key: CellKey) -> bool {
        self.refs.contains_key(&key)
            || self.structural_sensitive.contains(&key)
            || self.spill_watches.contains_key(&key)
            || self.untracked_spill_errors.contains(&key)
            || self.runtime_refs.contains(&key)
            || self.volatile.contains(&key)
            || self.volatile_refs.contains(&key)
            || self.visibility.contains(&key)
            || wb
                .sheets
                .get(key.0)
                .and_then(|s| s.get(key.1, key.2))
                .is_some_and(|c| c.formula.is_some())
    }

    pub(crate) fn dynamic_cells(&self) -> impl Iterator<Item = CellKey> + '_ {
        self.runtime_refs
            .iter()
            .chain(self.volatile_refs.iter())
            .copied()
    }

    pub(crate) fn clear_dynamic_watch(&self, key: CellKey) {
        if self.volatile_refs.contains(&key) || self.runtime_refs.contains(&key) {
            if let Ok(mut guard) = self.dynamic_watches.write() {
                guard.remove(key);
            }
        }
    }

    pub(crate) fn mark_dynamic_evaluated(&self, key: CellKey) {
        if self.volatile_refs.contains(&key) || self.runtime_refs.contains(&key) {
            if let Ok(rg) = self.dynamic_watches.read()
                && rg.by_cell.contains_key(&key)
            {
                return;
            }
            if let Ok(mut wg) = self.dynamic_watches.write() {
                wg.mark_evaluated(key);
            }
        }
    }

    pub(crate) fn record_dynamic_watch(
        &self,
        key: CellKey,
        sheet: usize,
        r1: i64,
        c1: i64,
        r2: i64,
        c2: i64,
    ) {
        if !(self.volatile_refs.contains(&key) || self.runtime_refs.contains(&key)) {
            return;
        }
        let Some(cr) = CompactRect::from_rect(Rect {
            sheet,
            r1: r1.min(r2).max(0),
            c1: c1.min(c2).max(0),
            r2: r1.max(r2).max(0),
            c2: c1.max(c2).max(0),
        }) else {
            return;
        };
        if let Ok(rg) = self.dynamic_watches.read()
            && let Some(existing) = rg.by_cell.get(&key)
            && existing.as_slice().contains(&cr)
        {
            return;
        }
        if let Ok(mut wg) = self.dynamic_watches.write() {
            wg.record(key, cr);
        }
    }

    /// Filter `dynamic_cells()` against `sorted_targets` using the inverted `by_rect` index
    /// of recorded `OFFSET`/`INDIRECT` target rectangles so 50k formulas sharing a dynamic
    /// named range prune unrelated edits in O(unique rects).
    pub(crate) fn dynamic_cells_touching(&self, sorted_targets: &[CellKey]) -> Vec<CellKey> {
        if sorted_targets.is_empty()
            || (self.volatile_refs.is_empty() && self.runtime_refs.is_empty())
        {
            return Vec::new();
        }
        let max_sheet = sorted_targets.iter().map(|t| t.0).max().unwrap_or(0);
        let mut bboxes: Vec<Option<(i64, i64, i64, i64)>> = vec![None; max_sheet + 1];
        for &(s, r, c) in sorted_targets {
            let (ri, ci) = (i64::from(r), i64::from(c));
            match &mut bboxes[s] {
                Some((r1, c1, r2, c2)) => {
                    *r1 = (*r1).min(ri);
                    *c1 = (*c1).min(ci);
                    *r2 = (*r2).max(ri);
                    *c2 = (*c2).max(ci);
                }
                slot @ None => *slot = Some((ri, ci, ri, ci)),
            }
        }
        let Ok(watches) = self.dynamic_watches.read() else {
            return self.dynamic_cells().collect();
        };
        let mut out = Vec::new();
        // 1) Check unique watched target rects in O(unique_rects).
        for (cr, cells) in &watches.by_rect {
            if rect_hits_sorted_targets(
                cr.sheet as usize,
                i64::from(cr.r1),
                i64::from(cr.c1),
                i64::from(cr.r2),
                i64::from(cr.c2),
                &bboxes,
                sorted_targets,
            ) {
                out.extend(cells.iter().copied());
            }
        }
        // 2) Only scan individual dynamic cells if some have never been evaluated yet.
        let total_dyn = self.volatile_refs.len() + self.runtime_refs.len();
        if watches.by_cell.len() < total_dyn {
            for key in self.dynamic_cells() {
                if !watches.by_cell.contains_key(&key) {
                    out.push(key);
                }
            }
        }
        if out.len() > 1 {
            out.sort_unstable();
            out.dedup();
        }
        out
    }

    /// Recalculate visibility-aware formulas that could read the filtered sheet, plus
    /// volatile formulas. Unresolved references require the full calculation path.
    pub fn visibility_seeds(&self, sheet: usize) -> Option<Vec<CellKey>> {
        if self.requires_full_recalc() {
            return None;
        }
        let mut seeds: HashSet<CellKey> = self
            .volatile
            .iter()
            .chain(self.volatile_refs.iter())
            .copied()
            .collect();
        seeds.extend(self.visibility.iter().copied().filter(|key| {
            self.refs
                .get(key)
                .is_some_and(|refs| refs.as_slice().iter().any(|r| r.sheet as usize == sheet))
        }));
        Some(seeds.into_iter().collect())
    }
}

fn sheet_index(wb: &crate::Workbook, name: Option<&str>, cur: usize) -> usize {
    match name {
        Some(n) => wb
            .sheets
            .iter()
            .position(|s| s.name.eq_ignore_ascii_case(n))
            .unwrap_or(cur),
        None => cur,
    }
}

/// 三维引用 `Sheet1:Sheet3` 的 sheet 下标；端点缺失时返回空，让依赖图
/// 保守地不把错误引用挂到别的表上。
fn sheet_span(wb: &crate::Workbook, a: &str, b: &str) -> Vec<usize> {
    let Some(ia) = wb
        .sheets
        .iter()
        .position(|s| s.name.eq_ignore_ascii_case(a))
    else {
        return vec![];
    };
    let Some(ib) = wb
        .sheets
        .iter()
        .position(|s| s.name.eq_ignore_ascii_case(b))
    else {
        return vec![];
    };
    let (lo, hi) = if ia <= ib { (ia, ib) } else { (ib, ia) };
    (lo..=hi).collect()
}

#[inline]
fn resolve_row(r: &RefExpr, dr: i64) -> i64 {
    if r.row_abs { r.row } else { r.row + dr }
}

#[inline]
fn resolve_col(r: &RefExpr, dc: i64) -> i64 {
    if r.col_abs { r.col } else { r.col + dc }
}

/// Compute shifted SUMIF 3rd-argument amount rectangle directly without mutating/cloning AST nodes.
fn sumif_amount_rect_shifted(
    wb: &crate::Workbook,
    si: usize,
    args: &[Node],
    dr: i64,
    dc: i64,
) -> Option<Rect> {
    let [criteria, _, amounts] = args else {
        return None;
    };
    let (height, width) = match criteria {
        Node::Range(a, b) => (
            (resolve_row(a, dr) - resolve_row(b, dr)).abs(),
            (resolve_col(a, dc) - resolve_col(b, dc)).abs(),
        ),
        Node::Ref(_) => (0, 0),
        _ => return None,
    };
    let (start_ref, start_r, start_c) = match amounts {
        Node::Ref(a) => (a, resolve_row(a, dr), resolve_col(a, dc)),
        Node::Range(a, b) => (
            a,
            resolve_row(a, dr).min(resolve_row(b, dr)),
            resolve_col(a, dc).min(resolve_col(b, dc)),
        ),
        _ => return None,
    };
    if start_ref.book.is_some() || start_ref.sheet2.is_some() {
        return None;
    }
    let sheet = sheet_index(wb, start_ref.sheet.as_deref(), si);
    let r1 = start_r.max(0);
    let c1 = start_c.max(0);
    let r2 = (start_r + height).clamp(r1, 1_048_575);
    let c2 = (start_c + width).clamp(c1, 16_383);
    Some(Rect {
        sheet,
        r1,
        c1,
        r2,
        c2,
    })
}

fn literal_i64(node: &Node) -> Option<i64> {
    match node {
        Node::Number(n) if n.is_finite() => Some(n.trunc() as i64),
        Node::Unary(true, inner) => literal_i64(inner).map(|v| -v),
        Node::Unary(false, inner) => literal_i64(inner),
        _ => None,
    }
}

/// Statically resolve `OFFSET(ref, row_off, col_off, [height], [width])` when all arguments
/// are compile-time literals and the base reference is a single-sheet `Ref`/`Range`.
fn static_offset_rect(
    wb: &crate::Workbook,
    si: usize,
    args: &[Node],
    dr: i64,
    dc: i64,
) -> Option<Rect> {
    if !(3..=5).contains(&args.len()) {
        return None;
    }
    let (base_ref, br1, bc1, br2, bc2) = match &args[0] {
        Node::Ref(a) => {
            let r = resolve_row(a, dr);
            let c = resolve_col(a, dc);
            (a, r, c, r, c)
        }
        Node::Range(a, b) => {
            if a.book.is_some() || b.book.is_some() || a.sheet2.is_some() || b.sheet2.is_some() {
                return None;
            }
            let r1 = resolve_row(a, dr);
            let c1 = resolve_col(a, dc);
            let r2 = resolve_row(b, dr);
            let c2 = resolve_col(b, dc);
            (a, r1.min(r2), c1.min(c2), r1.max(r2), c1.max(c2))
        }
        _ => return None,
    };
    if base_ref.book.is_some() || base_ref.sheet2.is_some() {
        return None;
    }
    let row_off = literal_i64(&args[1])?;
    let col_off = literal_i64(&args[2])?;
    let base_h = br2 - br1 + 1;
    let base_w = bc2 - bc1 + 1;
    let h = match args.get(3) {
        None | Some(Node::Empty) => base_h,
        Some(n) => literal_i64(n)?,
    };
    let w = match args.get(4) {
        None | Some(Node::Empty) => base_w,
        Some(n) => literal_i64(n)?,
    };
    if h == 0 || w == 0 {
        return None;
    }
    let nr1 = br1.checked_add(row_off)?;
    let nc1 = bc1.checked_add(col_off)?;
    let (r1, r2) = if h > 0 {
        (nr1, nr1.checked_add(h - 1)?)
    } else {
        (nr1.checked_add(h + 1)?, nr1)
    };
    let (c1, c2) = if w > 0 {
        (nc1, nc1.checked_add(w - 1)?)
    } else {
        (nc1.checked_add(w + 1)?, nc1)
    };
    if r1 < 0 || c1 < 0 || r2 > 1_048_575 || c2 > 16_383 {
        return None;
    }
    let sheet = sheet_index(wb, base_ref.sheet.as_deref(), si);
    Some(Rect { sheet, r1, c1, r2, c2 })
}

#[derive(Default)]
struct Dependencies {
    rects: Vec<Rect>,
    volatile: bool,
    nondet_volatile: bool,
    has_today: bool,
    has_other_volatile: bool,
    volatile_ref: bool,
    visibility: bool,
    runtime: bool,
    spill_op: bool,
    cached_spill_error: bool,
    structural_sensitive: bool,
    indirect_definition: bool,
}

struct Walker<'a> {
    wb: &'a crate::Workbook,
    name_index: Option<&'a super::NameIndex>,
    has_tables: bool,
    row: u32,
    col: u32,
    bound: Vec<String>,
    names: Vec<(usize, usize)>,
    name_expansions: usize,
    result: Dependencies,
}

impl Walker<'_> {
    fn formula_arc(&mut self, si: usize, formula: &Arc<str>) {
        if self.has_tables {
            let raw = formula.strip_prefix('=').unwrap_or(formula);
            if let Some(expanded) =
                crate::table::expand_structured_at(self.wb, si, Some(self.row), Some(self.col), raw)
            {
                self.result.indirect_definition = true;
                self.result.structural_sensitive = true;
                let _ = super::compiled::with_shifted(&expanded, self.row, self.col, |node, dr, dc| {
                    self.walk(node, si, dr, dc)
                });
                return;
            }
        }
        // Parse failure yields zero static references instead of forcing requires_full_recalc().
        let _ = super::compiled::with_shifted_arc(formula, self.row, self.col, |node, dr, dc| {
            self.walk(node, si, dr, dc)
        });
    }

    fn formula(&mut self, si: usize, formula: &str) {
        let raw = formula.strip_prefix('=').unwrap_or(formula);
        let expanded = if self.has_tables {
            crate::table::expand_structured_at(self.wb, si, Some(self.row), Some(self.col), raw)
        } else {
            None
        };
        self.result.indirect_definition |= expanded.is_some();
        self.result.structural_sensitive |= expanded.is_some();
        let body = expanded.as_deref().unwrap_or(raw);
        // Parse failure yields zero static references instead of forcing requires_full_recalc().
        let _ = super::compiled::with_shifted(body, self.row, self.col, |node, dr, dc| {
            self.walk(node, si, dr, dc)
        });
    }

    #[inline]
    fn resolve_name(&self, name: &str, si: usize, scoped: bool) -> Option<usize> {
        match self.name_index {
            Some(ix) => ix.find(name, si, scoped),
            None => super::named_definition(self.wb, name, si, scoped),
        }
    }

    fn name(&mut self, si: usize, name: &str, scoped: bool) {
        let Some(index) = self.resolve_name(name, si, scoped) else {
            // Undefined name (e.g. `=UndefinedName+1`): treat as dynamic/volatile_ref so it
            // recalculates with edits without degrading the entire workbook to full recalc.
            self.result.volatile_ref = true;
            return;
        };
        self.definition(si, index);
    }

    fn definition(&mut self, si: usize, index: usize) {
        let wb = self.wb;
        // Cyclic/name-expanding definitions cannot supply a finite static index.
        if self.name_expansions >= 512
            || self.names.len() >= 32
            || self.names.contains(&(si, index))
        {
            self.result.runtime = true;
            return;
        }
        self.result.indirect_definition = true;
        self.name_expansions += 1;
        self.names.push((si, index));
        self.formula(si, &wb.names[index].2);
        self.names.pop();
    }

    fn walk(&mut self, node: &Node, si: usize, dr: i64, dc: i64) {
        let wb = self.wb;
        // Only position/layout/visibility-observing functions or dynamic indirections
        // can change result when all referenced cell/range inputs remain identical.
        self.result.structural_sensitive |= match node {
            Node::Func(name, _) => {
                let id = name.id();
                id.is_position_or_layout()
                    || (!id.is_known_builtin() && !super::functions::listed_builtin(name.as_str()))
            }
            Node::Ref(r) => r.book.is_some() || r.sheet2.is_some(),
            Node::Range(a, b) => {
                a.book.is_some() || b.book.is_some() || a.sheet2.is_some() || b.sheet2.is_some()
            }
            Node::Implicit(_) | Node::Spill(_) | Node::Name(_) | Node::ScopedName { .. } => true,
            _ => false,
        };
        match node {
            Node::Ref(re) | Node::Spill(re) => {
                let r = resolve_row(re, dr).max(0);
                let c = resolve_col(re, dc).max(0);
                if matches!(node, Node::Spill(_)) {
                    self.result.spill_op = true;
                    let idx = sheet_index(wb, re.sheet.as_deref(), si);
                    let anchor = crate::CellRef::new(r as u32, c as u32);
                    if re.book.is_some() || re.sheet2.is_some() {
                        // External / 3D spill ref always evaluates to #REF!
                        return;
                    } else if let Some(&(rows, cols)) =
                        wb.sheets.get(idx).and_then(|s| s.spill_shapes.get(&anchor))
                    {
                        if rows > 0 && cols > 0 {
                            self.result.rects.push(Rect {
                                sheet: idx,
                                r1: r,
                                c1: c,
                                r2: r + i64::from(rows) - 1,
                                c2: c + i64::from(cols) - 1,
                            });
                            return;
                        }
                        // Blocked `(0, 0)` anchor: fall through to register anchor point dep;
                        // the anchor itself has a `spill_watch` (or `untracked_spill_errors`).
                    } else {
                        let uncalculated_formula_anchor = wb
                            .sheets
                            .get(idx)
                            .and_then(|s| s.get(anchor.row, anchor.col))
                            .is_some_and(|cell| {
                                cell.formula.is_some() && cell.value == crate::Value::Empty
                            });
                        if uncalculated_formula_anchor {
                            // Anchor formula has never been evaluated yet so its spill shape is unknown.
                            self.result.runtime = true;
                        } else {
                            // Anchor currently has no spill shape (scalar / empty): track as dynamic
                            // cell + anchor point dep instead of forcing whole-workbook full recalc.
                            self.result.volatile_ref = true;
                        }
                    }
                }
                if re.book.is_some() {
                    return;
                }
                if let (Some(a), Some(b)) = (re.sheet.as_deref(), re.sheet2.as_deref()) {
                    for sheet in sheet_span(wb, a, b) {
                        self.result.rects.push(Rect {
                            sheet,
                            r1: r,
                            c1: c,
                            r2: r,
                            c2: c,
                        });
                    }
                } else {
                    let sheet = sheet_index(wb, re.sheet.as_deref(), si);
                    self.result.rects.push(Rect {
                        sheet,
                        r1: r,
                        c1: c,
                        r2: r,
                        c2: c,
                    });
                }
            }
            Node::Range(a, b) => {
                if a.book.is_some() || b.book.is_some() {
                    return;
                }
                let ar = resolve_row(a, dr);
                let br = resolve_row(b, dr);
                let ac = resolve_col(a, dc);
                let bc = resolve_col(b, dc);
                let r1 = ar.min(br).max(0);
                let c1 = ac.min(bc).max(0);
                let r2 = ar.max(br).max(0);
                let c2 = ac.max(bc).max(0);
                if let (Some(x), Some(y)) = (a.sheet.as_deref(), a.sheet2.as_deref()) {
                    for sheet in sheet_span(wb, x, y) {
                        self.result.rects.push(Rect {
                            sheet,
                            r1,
                            c1,
                            r2,
                            c2,
                        });
                    }
                } else if let (Some(x), Some(y)) = (b.sheet.as_deref(), b.sheet2.as_deref()) {
                    for sheet in sheet_span(wb, x, y) {
                        self.result.rects.push(Rect {
                            sheet,
                            r1,
                            c1,
                            r2,
                            c2,
                        });
                    }
                } else {
                    let sheet = sheet_index(wb, a.sheet.as_deref().or(b.sheet.as_deref()), si);
                    self.result.rects.push(Rect {
                        sheet,
                        r1,
                        c1,
                        r2,
                        c2,
                    });
                }
            }
            Node::Name(n) => {
                if !self.bound.iter().rev().any(|v| v == n) {
                    self.name(si, n, false);
                }
            }
            Node::ScopedName {
                book: None,
                sheet: Some(sheet),
                name,
            } => {
                if let Some(target) = wb
                    .sheets
                    .iter()
                    .position(|s| s.name.eq_ignore_ascii_case(sheet))
                {
                    self.name(target, name, true);
                }
            }
            Node::Func(name, args) if name.id() == FuncId::Let && args.len() >= 3 => {
                let base = self.bound.len();
                for pair in args[..args.len() - 1].chunks_exact(2) {
                    if let Node::Name(n) = &pair[0] {
                        self.walk(&pair[1], si, dr, dc);
                        self.bound.push(n.clone());
                    }
                }
                self.walk(args.last().unwrap(), si, dr, dc);
                self.bound.truncate(base);
            }
            Node::Func(name, args) if name.id() == FuncId::Lambda && !args.is_empty() => {
                let base = self.bound.len();
                for p in &args[..args.len() - 1] {
                    if let Node::Name(n) = p {
                        self.bound.push(n.clone());
                    }
                }
                self.walk(args.last().unwrap(), si, dr, dc);
                self.bound.truncate(base);
            }
            Node::Func(name, args)
                if name.id() == FuncId::InternalCall
                    && matches!(args.first(), Some(Node::ScopedName { book: None, .. })) =>
            {
                let caller = std::mem::take(&mut self.bound);
                self.walk(&args[0], si, dr, dc);
                self.bound = caller;
                for arg in &args[1..] {
                    self.walk(arg, si, dr, dc);
                }
            }
            Node::Func(name, args) => {
                let id = name.id();
                if !wb.names.is_empty()
                    && !id.is_known_builtin()
                    && !super::functions::listed_builtin(name.as_str())
                    && !self.bound.iter().any(|local| local == name.as_str())
                    && let Some(index) = self.resolve_name(name.as_str(), si, false)
                {
                    let caller = std::mem::take(&mut self.bound);
                    self.definition(si, index);
                    self.bound = caller;
                }
                if id == FuncId::Offset {
                    self.result.structural_sensitive = true;
                    if let Some(rect) = static_offset_rect(wb, si, args, dr, dc) {
                        self.result.rects.push(rect);
                    } else {
                        self.result.volatile_ref = true;
                    }
                } else {
                    self.result.volatile_ref |= id.is_volatile_ref();
                }
                self.result.visibility |= id.is_visibility();
                if id.is_volatile() {
                    self.result.volatile = true;
                    if id.is_nondeterministic_volatile() {
                        self.result.nondet_volatile = true;
                    }
                    if id == FuncId::Today {
                        self.result.has_today = true;
                    } else {
                        self.result.has_other_volatile = true;
                    }
                }
                let sumif_rect = if id == FuncId::SumIf {
                    sumif_amount_rect_shifted(wb, si, args, dr, dc)
                } else {
                    None
                };
                for (i, a) in args.iter().enumerate() {
                    if i == 2
                        && let Some(rect) = sumif_rect
                    {
                        self.result.rects.push(rect);
                    } else {
                        self.walk(a, si, dr, dc);
                    }
                }
            }
            Node::Implicit(x) | Node::Unary(_, x) | Node::Percent(x) => self.walk(x, si, dr, dc),
            Node::Binary(_, l, r) => {
                self.walk(l, si, dr, dc);
                self.walk(r, si, dr, dc);
            }
            _ => {}
        }
    }
}

pub fn refs_of(wb: &crate::Workbook, si: usize, formula: &str) -> Vec<Rect> {
    dependencies(wb, (si, 0, 0), formula).rects
}

pub(super) fn parallel_inputs(wb: &crate::Workbook, key: CellKey, src: &str) -> Option<Vec<Rect>> {
    let info = dependencies(wb, key, src);
    (!info.runtime && !info.nondet_volatile && !info.volatile_ref).then_some(info.rects)
}

pub(super) fn parallel_inputs_arc(
    wb: &crate::Workbook,
    name_index: Option<&super::NameIndex>,
    has_tables: bool,
    key: CellKey,
    formula: &Arc<str>,
) -> Option<Vec<Rect>> {
    let info = dependencies_arc(wb, name_index, has_tables, key, formula);
    (!info.runtime && !info.nondet_volatile && !info.volatile_ref).then_some(info.rects)
}

pub(super) fn visit_parallel_inputs_arc(
    wb: &crate::Workbook,
    name_index: Option<&super::NameIndex>,
    has_tables: bool,
    key: CellKey,
    formula: &Arc<str>,
    mut visit: impl FnMut(Rect),
) -> bool {
    if !has_tables || !formula.as_bytes().contains(&b'[') {
        let fast = super::compiled::with_ref_summary_arc(
            formula,
            key.1,
            key.2,
            |summary, _node, dr, dc| match summary {
                super::compiled::TemplateRefSummary::PureNoRefs { nondet_volatile } => {
                    Some(!nondet_volatile)
                }
                super::compiled::TemplateRefSummary::SameSheet {
                    count,
                    nondet_volatile,
                    spans,
                } => {
                    if nondet_volatile {
                        return Some(false);
                    }
                    for span in &spans[..count as usize] {
                        let (r1, c1, r2, c2) = span.resolve(dr, dc);
                        visit(Rect {
                            sheet: key.0,
                            r1,
                            c1,
                            r2,
                            c2,
                        });
                    }
                    Some(true)
                }
                super::compiled::TemplateRefSummary::Complex => None,
            },
        );
        match fast {
            Ok(Some(ok)) => return ok,
            Err(_) => return true,
            Ok(None) => {}
        }
    }
    let info = dependencies_arc(wb, name_index, has_tables, key, formula);
    if info.runtime || info.nondet_volatile || info.volatile_ref {
        return false;
    }
    for r in info.rects {
        visit(r);
    }
    true
}

#[inline]
fn rect_hits_sorted_targets(
    sheet: usize,
    r1: i64,
    c1: i64,
    r2: i64,
    c2: i64,
    bboxes: &[Option<(i64, i64, i64, i64)>],
    sorted_targets: &[CellKey],
) -> bool {
    let Some(&(br1, bc1, br2, bc2)) = bboxes.get(sheet).and_then(|b| b.as_ref()) else {
        return false;
    };
    if r2 < br1 || r1 > br2 || c2 < bc1 || c1 > bc2 {
        return false;
    }
    let lo_r = r1.max(0) as u32;
    let hi_r = r2.max(0) as u32;
    let lo_c = c1.max(0) as u32;
    let hi_c = c2.max(0) as u32;
    let idx = sorted_targets.partition_point(|&(s, r, c)| (s, r, c) < (sheet, lo_r, lo_c));
    for &(s, r, c) in &sorted_targets[idx..] {
        if s != sheet || r > hi_r {
            break;
        }
        if c >= lo_c && c <= hi_c {
            return true;
        }
    }
    false
}

/// Fast zero-allocation pre-check for Pass-0 spill convergence: returns `false` if no formula
/// in `formulas` is dynamic (`INDIRECT`/`OFFSET`/runtime) or directly references any cell in
/// `sorted_targets`, allowing `recalc_selected` to skip a full 1M-formula `Deps::build_filtered`.
pub(super) fn any_formula_depends_on_sorted(
    wb: &crate::Workbook,
    selected: Option<&[bool]>,
    formulas: &[CellKey],
    sorted_targets: &[CellKey],
) -> bool {
    if formulas.is_empty() {
        return false;
    }
    if wb
        .sheets
        .iter()
        .enumerate()
        .filter(|(i, _)| selected.is_none_or(|s| s[*i]))
        .any(|(_, sh)| sh.spill_shapes.values().any(|&shape| shape == (0, 0)))
    {
        return true;
    }
    let mut bboxes: Vec<Option<(i64, i64, i64, i64)>> = vec![None; wb.sheets.len()];
    for &(si, r, c) in sorted_targets {
        if let Some(slot) = bboxes.get_mut(si) {
            let (ri, ci) = (i64::from(r), i64::from(c));
            *slot = Some(match *slot {
                Some((r1, c1, r2, c2)) => (r1.min(ri), c1.min(ci), r2.max(ri), c2.max(ci)),
                None => (ri, ci, ri, ci),
            });
        }
    }
    let has_tables = wb.sheets.iter().any(|s| !s.tables.is_empty());
    let name_index = super::NameIndex::build_if_needed(wb);
    let check_one = |&(si, row, col): &CellKey| -> bool {
        let Some(f_arc) = wb
            .sheets
            .get(si)
            .and_then(|s| s.formula_arc_of(row, col))
        else {
            return false;
        };
        if !has_tables || !f_arc.as_bytes().contains(&b'[') {
            let fast = super::compiled::with_ref_summary_arc(
                f_arc,
                row,
                col,
                |summary, _node, dr, dc| match summary {
                    super::compiled::TemplateRefSummary::PureNoRefs { .. } => Some(false),
                    super::compiled::TemplateRefSummary::SameSheet { count, spans, .. } => {
                        if bboxes.get(si).is_none_or(Option::is_none) {
                            return Some(false);
                        }
                        for span in &spans[..count as usize] {
                            let (r1, c1, r2, c2) = span.resolve(dr, dc);
                            if rect_hits_sorted_targets(si, r1, c1, r2, c2, &bboxes, sorted_targets)
                            {
                                return Some(true);
                            }
                        }
                        Some(false)
                    }
                    super::compiled::TemplateRefSummary::Complex => None,
                },
            );
            match fast {
                Ok(Some(hit)) => return hit,
                Err(_) => return false,
                Ok(None) => {}
            }
        }
        let info = dependencies_arc(wb, name_index.as_ref(), has_tables, (si, row, col), f_arc);
        info.runtime
            || info.volatile_ref
            || info.rects.iter().any(|r| {
                rect_hits_sorted_targets(r.sheet, r.r1, r.c1, r.r2, r.c2, &bboxes, sorted_targets)
            })
    };

    const CHUNK: usize = 1024;
    if formulas.len() < CHUNK * 2 {
        return formulas.iter().any(check_one);
    }
    let workers = super::pool::max_workers()
        .min(formulas.len().div_ceil(CHUNK))
        .clamp(1, 16);
    if workers < 2 {
        return formulas.iter().any(check_one);
    }
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let hit = AtomicBool::new(false);
    let next_chunk = AtomicUsize::new(0);
    let total_chunks = formulas.len().div_ceil(CHUNK);
    super::pool::run_parallel(workers, |_| {
        while !hit.load(Ordering::Relaxed) {
            let ci = next_chunk.fetch_add(1, Ordering::Relaxed);
            if ci >= total_chunks {
                break;
            }
            let start = ci * CHUNK;
            let end = (start + CHUNK).min(formulas.len());
            if formulas[start..end].iter().any(&check_one) {
                hit.store(true, Ordering::Relaxed);
                break;
            }
        }
    });
    hit.load(Ordering::Relaxed)
}

fn dependencies(wb: &crate::Workbook, key: CellKey, formula: &str) -> Dependencies {
    let has_tables = wb.sheets.iter().any(|s| !s.tables.is_empty());
    let mut walker = Walker {
        wb,
        name_index: None,
        has_tables,
        row: key.1,
        col: key.2,
        bound: Vec::new(),
        names: Vec::new(),
        name_expansions: 0,
        result: Dependencies::default(),
    };
    walker.formula(key.0, formula);
    walker.result
}

fn dependencies_arc(
    wb: &crate::Workbook,
    name_index: Option<&super::NameIndex>,
    has_tables: bool,
    key: CellKey,
    formula: &Arc<str>,
) -> Dependencies {
    let mut walker = Walker {
        wb,
        name_index,
        has_tables,
        row: key.1,
        col: key.2,
        bound: Vec::new(),
        names: Vec::new(),
        name_expansions: 0,
        result: Dependencies::default(),
    };
    walker.formula_arc(key.0, formula);
    walker.result
}

impl Deps {
    pub fn build(wb: &crate::Workbook) -> Deps {
        Self::build_filtered(wb, None)
    }

    pub(crate) fn build_filtered(wb: &crate::Workbook, selected: Option<&[bool]>) -> Deps {
        let mut jobs: Vec<(CellKey, &Arc<str>, bool)> = Vec::new();
        for (si, sheet) in wb.sheets.iter().enumerate() {
            if selected.is_some_and(|mask| !mask.get(si).copied().unwrap_or(false)) {
                continue;
            }
            for (cell_ref, cell) in sheet.formula_cells() {
                if let Some(f) = cell.formula.as_ref() {
                    let is_spill_err = super::is_cached_spill_error(&cell.value);
                    jobs.push(((si, cell_ref.row, cell_ref.col), f, is_spill_err));
                }
            }
        }
        let mut deps = Deps {
            refs: RefMap::with_capacity(jobs.len()),
            formula_ids: CowMap::with_capacity(jobs.len()),
            slots: SlotTable::with_capacity(jobs.len()),
            ..Default::default()
        };
        let has_tables = wb.sheets.iter().any(|s| !s.tables.is_empty());
        let name_index = super::NameIndex::build_if_needed(wb);
        let name_ix_ref = name_index.as_ref();
        let mut blocked_spills: Vec<CellKey> = Vec::new();
        let workers = if jobs.len() >= 2_048 {
            super::pool::max_workers()
                .min(jobs.len().div_ceil(512))
                .min(16)
        } else {
            1
        };
        if workers <= 1 {
            for &(key, f, is_spill_err) in &jobs {
                let mut info = dependencies_arc(wb, name_ix_ref, has_tables, key, f);
                info.cached_spill_error = is_spill_err;
                if is_spill_err {
                    blocked_spills.push(key);
                }
                deps.insert_building(key, info);
            }
        } else {
            use std::sync::atomic::{AtomicUsize, Ordering};
            const CHUNK: usize = 512;
            let num_chunks = jobs.len().div_ceil(CHUNK);
            let next_chunk = AtomicUsize::new(0);
            let outputs: Vec<Mutex<Vec<(CellKey, Dependencies)>>> =
                (0..num_chunks).map(|_| Mutex::new(Vec::new())).collect();
            let jobs_slice = jobs.as_slice();
            super::pool::run_parallel(workers, |_| {
                loop {
                    let ci = next_chunk.fetch_add(1, Ordering::Relaxed);
                    if ci >= num_chunks {
                        break;
                    }
                    let start = ci * CHUNK;
                    let end = (start + CHUNK).min(jobs_slice.len());
                    let mut batch = Vec::with_capacity(end - start);
                    for &(key, f, is_spill_err) in &jobs_slice[start..end] {
                        let mut info = dependencies_arc(wb, name_ix_ref, has_tables, key, f);
                        info.cached_spill_error = is_spill_err;
                        batch.push((key, info));
                    }
                    *outputs[ci].lock().unwrap() = batch;
                }
            });
            for chunk_mutex in outputs {
                for (key, info) in chunk_mutex.into_inner().unwrap() {
                    if info.cached_spill_error {
                        blocked_spills.push(key);
                    }
                    deps.insert_building(key, info);
                }
            }
        }
        deps.index_workbook_spills(wb, selected, &blocked_spills);
        deps.seal_points();
        deps
    }

    /// Incrementally refresh spill watches for specific anchor keys without rebuilding the full graph.
    pub(crate) fn refresh_spills(
        &mut self,
        wb: &crate::Workbook,
        keys: impl IntoIterator<Item = CellKey>,
    ) {
        for key in keys {
            let is_spill_err = wb
                .sheets
                .get(key.0)
                .and_then(|s| s.get(key.1, key.2))
                .is_some_and(|c| super::is_cached_spill_error(&c.value));
            self.index_cell_spill(wb, key, is_spill_err);
        }
    }

    /// Use only immediately after a completed whole-workbook calculation.
    /// A graph built from a fast-opened OOXML file does not validate its <v> caches.
    pub fn build_after_recalc(wb: &crate::Workbook) -> Self {
        let mut graph = Self::build(wb);
        graph.computed_baseline = !wb.calc_manual && !wb.calc_pending;
        graph
    }

    /// Build a dependency graph and optionally certify an imported/saved workbook's cached
    /// `<v>` values when all formula cells carry cached values and either no volatile formulas
    /// exist or only `TODAY()` formulas exist whose cached values already match today's date!
    pub fn build_with_cached_baseline(wb: &crate::Workbook, trust_cached_values: bool) -> Self {
        let mut graph = Self::build(wb);
        if trust_cached_values && !wb.calc_manual && !wb.calc_pending {
            let today_valid = if graph.volatile.is_empty() && graph.volatile_refs.is_empty() {
                true
            } else if graph.volatile_refs.is_empty()
                && graph.volatile.len() == graph.today_only_cells.len()
                && graph.today_only_cells.len() <= 256
            {
                let today_serial = super::functions::now_serial_uncached_in(wb.date_system).floor();
                graph.today_only_cells.iter().all(|&(si, r, c)| {
                    let Some(cell) = wb.sheets.get(si).and_then(|s| s.get(r, c)) else {
                        return false;
                    };
                    if cell.value == crate::Value::Empty {
                        return false;
                    }
                    if let Some(f) = cell.formula.as_deref() {
                        let raw = f.trim().trim_start_matches('=').trim();
                        if raw.eq_ignore_ascii_case("TODAY()") {
                            return cell.value == crate::Value::Number(today_serial);
                        }
                    }
                    super::from_fvalue(&super::eval_cell(wb, si, r, c)) == cell.value
                })
            } else {
                false
            };
            if today_valid {
                let all_cached = wb.sheets.iter().all(|sh| {
                    sh.formula_cells()
                        .all(|(_, cell)| cell.value != crate::Value::Empty)
                });
                graph.computed_baseline = all_cached;
            }
        }
        graph
    }

    pub fn mark_computed_baseline(&mut self, wb: &crate::Workbook) {
        self.computed_baseline = !wb.calc_manual && !wb.calc_pending;
    }

    fn seal_points(&mut self) {
        for col in self.points_by_col.base_mut_flushed().values_mut() {
            col.seal();
        }
    }

    #[inline]
    fn alloc_slot(&mut self, key: CellKey) -> u32 {
        self.slots.alloc(key)
    }

    fn register_area(&mut self, rect: CompactRect, formula_id: u32) {
        if let Some(&area_id) = self.area_map.get(&rect) {
            self.areas
                .get_mut(area_id as usize)
                .push_subscriber(formula_id);
            return;
        }
        let area_id = u32::try_from(self.areas.len()).expect("more than 2^32 shared areas");
        self.areas.push(SharedArea::new(rect, formula_id));
        self.area_map.insert(rect, area_id);
        let (shift, bin) = row_bucket(rect.r1, rect.r2);
        if rect.c2 - rect.c1 > 512 {
            self.wide_sheets.insert(rect.sheet);
            let bucket_vec = self.wide_by_row.get_or_default_mut((rect.sheet, shift, bin));
            Arc::make_mut(bucket_vec).push(WideInterval {
                area_id,
                r1: rect.r1,
                r2: rect.r2,
                c1: rect.c1,
                c2: rect.c2,
            });
        } else {
            for c in rect.c1..=rect.c2 {
                self.range_cols.insert((rect.sheet, c));
                let bucket_vec = self.by_col.get_or_default_mut((rect.sheet, c, shift, bin));
                Arc::make_mut(bucket_vec).push(AreaInterval {
                    area_id,
                    first: rect.r1,
                    last: rect.r2,
                });
            }
        }
    }

    fn insert_spill_watch(&mut self, key: CellKey, rect: CompactRect) {
        if let Some((old_id, _)) = self.spill_watches.remove(&key)
            && self.slots.tombstone(old_id)
        {
            self.tombstones += 1;
        }
        let watch_id = self.alloc_slot(key);
        self.register_area(rect, watch_id);
        self.spill_watches.insert(key, (watch_id, rect));
    }

    fn index_cell_spill(&mut self, wb: &crate::Workbook, key: CellKey, is_spill_err: bool) {
        self.untracked_spill_errors.remove(&key);
        if let Some((old_id, _)) = self.spill_watches.remove(&key)
            && self.slots.tombstone(old_id)
        {
            self.tombstones += 1;
        }
        let anchor = crate::CellRef::new(key.1, key.2);
        if let Some(&(rows, cols)) = wb
            .sheets
            .get(key.0)
            .and_then(|s| s.spill_shapes.get(&anchor))
            && (rows > 1 || cols > 1)
        {
            if let Some(cr) = CompactRect::from_rect(Rect {
                sheet: key.0,
                r1: i64::from(key.1),
                c1: i64::from(key.2),
                r2: (i64::from(key.1) + i64::from(rows) - 1).min(1_048_575),
                c2: (i64::from(key.2) + i64::from(cols) - 1).min(16_383),
            }) {
                self.insert_spill_watch(key, cr);
            }
            return;
        }
        if !is_spill_err {
            return;
        }
        match super::probe_blocked_spill_shape(wb, key) {
            super::SpillProbe::Footprint(rows, cols) => {
                if let Some(cr) = CompactRect::from_rect(Rect {
                    sheet: key.0,
                    r1: i64::from(key.1),
                    c1: i64::from(key.2),
                    r2: (i64::from(key.1) + i64::from(rows) - 1).min(1_048_575),
                    c2: (i64::from(key.2) + i64::from(cols) - 1).min(16_383),
                }) {
                    self.insert_spill_watch(key, cr);
                }
            }
            super::SpillProbe::NotSpillAnchor => {}
            super::SpillProbe::Unresolved => {
                self.untracked_spill_errors.insert(key);
            }
        }
    }

    fn index_workbook_spills(
        &mut self,
        wb: &crate::Workbook,
        selected: Option<&[bool]>,
        blocked_spills: &[CellKey],
    ) {
        for (si, sheet) in wb.sheets.iter().enumerate() {
            if selected.is_some_and(|mask| !mask.get(si).copied().unwrap_or(false)) {
                continue;
            }
            for (&anchor, &(rows, cols)) in &sheet.spill_shapes {
                if rows > 1 || cols > 1 {
                    let key = (si, anchor.row, anchor.col);
                    if let Some(cr) = CompactRect::from_rect(Rect {
                        sheet: si,
                        r1: i64::from(anchor.row),
                        c1: i64::from(anchor.col),
                        r2: (i64::from(anchor.row) + i64::from(rows) - 1).min(1_048_575),
                        c2: (i64::from(anchor.col) + i64::from(cols) - 1).min(16_383),
                    }) {
                        self.insert_spill_watch(key, cr);
                    }
                }
            }
        }
        for &key in blocked_spills {
            self.index_cell_spill(wb, key, true);
        }
    }

    fn insert_building(&mut self, key: CellKey, info: Dependencies) {
        self.insert_inner(key, info, true);
    }

    fn insert(&mut self, key: CellKey, info: Dependencies) {
        self.insert_inner(key, info, false);
    }

    fn insert_inner(&mut self, key: CellKey, info: Dependencies, bulk_building: bool) {
        if info.indirect_definition {
            self.indirect_definitions.insert(key);
        }
        if info.structural_sensitive {
            self.structural_sensitive.insert(key);
        }
        if info.visibility {
            self.visibility.insert(key);
        }
        self.has_spill_op_refs |= info.spill_op;
        if info.runtime {
            self.runtime_refs.insert(key);
        }
        if info.volatile_ref {
            self.volatile_refs.insert(key);
        }
        if info.volatile {
            self.volatile.insert(key);
            if info.has_today && !info.has_other_volatile && !info.volatile_ref {
                self.today_only_cells.insert(key);
            }
        }
        let compact: Vec<CompactRect> = info
            .rects
            .into_iter()
            .filter_map(CompactRect::from_rect)
            .collect();
        if compact.is_empty() {
            return;
        }
        let id = self.alloc_slot(key);
        self.formula_ids.insert(key, id);
        for &r in &compact {
            if r.is_point() {
                let col_entry = self.points_by_col.get_or_default_mut((r.sheet, r.c1));
                if bulk_building {
                    col_entry.push_bulk(r.r1, id);
                } else {
                    col_entry.push_unsorted(r.r1, id);
                }
            } else {
                self.register_area(r, id);
            }
        }
        self.rect_count += compact.len();
        self.refs.insert(key, FormulaRefs::from_compact(compact));
    }

    fn maybe_compact_tombstones(&mut self) {
        if self.tombstones < 4_096 || self.tombstones * 2 <= self.slots.len() {
            return;
        }
        let slots = &self.slots;
        let points_base = self.points_by_col.base_mut_flushed();
        for col in points_base.values_mut() {
            Arc::make_mut(&mut col.sorted).retain(|&(_, id)| slots.is_live(id));
            Arc::make_mut(&mut col.unsorted).retain(|&(_, id)| slots.is_live(id));
        }
        points_base.retain(|_, col| !col.sorted.is_empty() || !col.unsorted.is_empty());
        self.points_by_col.len = points_base.len();
        for area in self.areas.as_mut_slice_flushed() {
            area.retain_live(slots);
        }
        self.tombstones = 0;
    }

    pub fn update_cell(&mut self, wb: &crate::Workbook, key: CellKey) {
        if let Some(old_refs) = self.refs.remove(&key) {
            self.rect_count -= old_refs.len();
        }
        if let Some(old_id) = self.formula_ids.remove(&key)
            && self.slots.tombstone(old_id)
        {
            self.tombstones += 1;
        }
        self.indirect_definitions.remove(&key);
        self.structural_sensitive.remove(&key);
        self.runtime_refs.remove(&key);
        self.volatile_refs.remove(&key);
        self.visibility.remove(&key);
        self.volatile.remove(&key);
        self.today_only_cells.remove(&key);
        self.untracked_spill_errors.remove(&key);
        if let Ok(mut wg) = self.dynamic_watches.write() {
            wg.remove(key);
        }
        if let Some((old_watch_id, _)) = self.spill_watches.remove(&key)
            && self.slots.tombstone(old_watch_id)
        {
            self.tombstones += 1;
        }
        self.maybe_compact_tombstones();

        if let Some(cell) = wb.sheets.get(key.0).and_then(|s| s.get(key.1, key.2))
            && let Some(f) = cell.formula.as_ref()
        {
            let is_spill_err = super::is_cached_spill_error(&cell.value);
            let has_tables = wb.sheets.iter().any(|s| !s.tables.is_empty());
            let mut info = dependencies_arc(wb, None, has_tables, key, f);
            info.cached_spill_error = is_spill_err;
            self.insert(key, info);
            self.index_cell_spill(wb, key, is_spill_err);
        }
    }

    /// Supports incremental structural recalculation even when `wb.names`, `sheet.tables`,
    /// or dynamic array spills exist: affected spill anchors are re-seeded and re-indexed
    /// without forcing a full-sheet recalculation.
    pub fn supports_structural(&self, wb: &crate::Workbook) -> bool {
        self.computed_baseline
            && !self.requires_full_recalc()
            && !wb.calc_iterate
            && wb.sheets.iter().all(|s| s.arrays.is_empty())
    }

    pub fn can_reuse_calculated_sheets(&self, wb: &crate::Workbook) -> bool {
        self.computed_baseline && !wb.calc_iterate && !wb.calc_manual && !wb.calc_pending
    }

    fn structural_sheets(&self, edited: usize, count: usize) -> Vec<bool> {
        let mut readers = vec![HashSet::new(); count];
        for (formula, refs) in self.refs.iter() {
            for r in refs.as_slice() {
                let target = r.sheet as usize;
                if formula.0 != target && target < count {
                    readers[target].insert(formula.0);
                }
            }
        }
        let mut pending = vec![edited];
        pending.extend(
            self.indirect_definitions
                .iter()
                .chain(self.volatile.iter())
                .chain(self.volatile_refs.iter())
                .chain(self.runtime_refs.iter())
                .chain(self.untracked_spill_errors.iter())
                .chain(self.spill_watches.keys())
                .map(|k| k.0),
        );
        let mut selected = vec![false; count];
        while let Some(si) = pending.pop() {
            if si < count && !selected[si] {
                selected[si] = true;
                pending.extend(&readers[si]);
            }
        }
        selected
    }

    pub fn recalc_structural_sheets(
        &self,
        wb: &mut crate::Workbook,
        edited: usize,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Option<Self> {
        if !self.can_reuse_calculated_sheets(wb) {
            return None;
        }
        let selected = self.structural_sheets(edited, wb.sheets.len());
        if !super::recalc_selected(wb, Some(&selected), Some(cancel), None) {
            return None;
        }
        let mut next = Self {
            computed_baseline: true,
            ..Default::default()
        };
        let mut blocked_spills = Vec::new();
        let mut keep = |key: CellKey| {
            if selected[key.0] {
                return;
            }
            let rects: Vec<Rect> = self
                .refs
                .get(&key)
                .map(|r| r.iter().collect())
                .unwrap_or_default();
            let info = Dependencies {
                rects,
                volatile: self.volatile.contains(&key),
                volatile_ref: self.volatile_refs.contains(&key),
                structural_sensitive: self.structural_sensitive.contains(&key),
                visibility: self.visibility.contains(&key),
                indirect_definition: self.indirect_definitions.contains(&key),
                ..Default::default()
            };
            next.insert_building(key, info);
        };
        for &key in self.refs.keys() {
            keep(key);
        }
        for &key in self.structural_sensitive.iter() {
            if !self.refs.contains_key(&key) {
                keep(key);
            }
        }
        let has_tables = wb.sheets.iter().any(|s| !s.tables.is_empty());
        let name_index = super::NameIndex::build_if_needed(wb);
        for (si, sheet) in wb.sheets.iter().enumerate().filter(|(si, _)| selected[*si]) {
            for (k, cell) in sheet.formula_cells() {
                if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    return None;
                }
                let key = (si, k.row, k.col);
                let is_spill_err = super::is_cached_spill_error(&cell.value);
                let mut info = dependencies_arc(
                    wb,
                    name_index.as_ref(),
                    has_tables,
                    key,
                    cell.formula.as_ref()?,
                );
                info.cached_spill_error = is_spill_err;
                if is_spill_err {
                    blocked_spills.push(key);
                }
                next.insert_building(key, info);
            }
        }
        next.index_workbook_spills(wb, None, &blocked_spills);
        next.seal_points();
        Some(next)
    }

    /// Reindex a broad edit on the worker. A cached scalar remains valid when
    /// every point reference still denotes the very same cell and every multi-cell
    /// range lies on an unedited sheet or strictly before `shift.at`.
    pub fn recalc_structural(
        &self,
        wb: &mut crate::Workbook,
        sheet: usize,
        op: &crate::structural::Structural,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> Option<Self> {
        if !self.supports_structural(wb) {
            return None;
        }
        let mut next = Self {
            computed_baseline: true,
            ..Default::default()
        };
        let mut seeds = Vec::new();
        let shift = op.reference_shift();
        let relocate = |(si, r, c): CellKey| {
            if si == sheet {
                crate::sync::shift_structural_coord(r, c, op).map(|(r, c)| (si, r, c))
            } else {
                Some((si, r, c))
            }
        };
        let has_tables = wb.sheets.iter().any(|s| !s.tables.is_empty());
        let name_index = super::NameIndex::build_if_needed(wb);
        let mut visited_old: FxHashSet<CellKey> = FxHashSet::default();
        let mut visited_new: FxHashSet<CellKey> = FxHashSet::default();
        let all_old = self
            .refs
            .keys()
            .chain(self.structural_sensitive.iter())
            .chain(self.volatile.iter())
            .chain(self.volatile_refs.iter());
        for old in all_old {
            if !visited_old.insert(*old) {
                continue;
            }
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                return None;
            }
            let Some(key) = relocate(*old) else { continue };
            visited_new.insert(key);
            let old_compact = self.refs.get(old).map(FormulaRefs::as_slice).unwrap_or(&[]);
            if !self.structural_sensitive.contains(old)
                && !self.volatile_refs.contains(old)
                && key == *old
                && !old_compact.iter().any(|r| {
                    r.sheet as usize == sheet
                        && (match shift.axis {
                            crate::refs::Axis::Row => r.r2,
                            crate::refs::Axis::Col => r.c2,
                        }) >= shift.at
                })
            {
                next.insert_building(
                    key,
                    Dependencies {
                        rects: old_compact.iter().copied().map(CompactRect::to_rect).collect(),
                        volatile: self.volatile.contains(old),
                        volatile_ref: self.volatile_refs.contains(old),
                        visibility: self.visibility.contains(old),
                        indirect_definition: self.indirect_definitions.contains(old),
                        ..Default::default()
                    },
                );
                continue;
            }
            let cell = wb.sheets[key.0].get(key.1, key.2)?;
            let formula = cell.formula.as_ref()?;
            let mut info = dependencies_arc(wb, name_index.as_ref(), has_tables, key, formula);
            info.cached_spill_error = super::is_cached_spill_error(&cell.value);
            let stable = cell.value != crate::Value::Empty
                && !self.structural_sensitive.contains(old)
                && !info.structural_sensitive
                && !info.runtime
                && !info.volatile_ref
                && info.rects.len() == old_compact.len()
                && old_compact.iter().zip(&info.rects).all(|(r, n)| {
                    if r.is_point() {
                        n.r1 == n.r2
                            && n.c1 == n.c2
                            && relocate((r.sheet as usize, r.r1, r.c1)).is_some_and(
                                |(s, row, col)| {
                                    s == n.sheet && i64::from(row) == n.r1 && i64::from(col) == n.c1
                                },
                            )
                    } else {
                        let unchanged_rect = r.to_rect() == *n;
                        let untouched_by_axis = (r.sheet as usize != sheet)
                            || (match shift.axis {
                                crate::refs::Axis::Row => r.r2,
                                crate::refs::Axis::Col => r.c2,
                            }) < shift.at;
                        unchanged_rect && untouched_by_axis
                    }
                });
            if !stable {
                seeds.push(key);
            }
            next.insert_building(key, info);
        }

        // Row insertion inside a Table may fill calculated-column formulas into the newly
        // inserted rows (see `Sheet::apply_table_row_fills`). Index and evaluate them too.
        if let crate::structural::Structural::InsertRows { at, n } = *op
            && n > 0
            && let Some(sh) = wb.sheets.get(sheet)
            && !sh.tables.is_empty()
        {
            let r_end = at.saturating_add(n - 1);
            for t in &sh.tables {
                let (_, c1, _, c2) = t.range;
                for (cref, cell) in sh.iter_rect((at, c1, r_end, c2)) {
                    if let Some(f) = cell.formula.as_ref() {
                        let key = (sheet, cref.row, cref.col);
                        if visited_new.insert(key) {
                            let mut info =
                                dependencies_arc(wb, name_index.as_ref(), has_tables, key, f);
                            info.cached_spill_error = super::is_cached_spill_error(&cell.value);
                            seeds.push(key);
                            next.insert_building(key, info);
                        }
                    }
                }
            }
        }

        let mut blocked_spills = Vec::new();
        for (si, sh) in wb.sheets.iter().enumerate() {
            for (&anchor, &(h, w)) in &sh.spill_shapes {
                let key = (si, anchor.row, anchor.col);
                let touches_shift = si == sheet
                    && (h == 0
                        || w == 0
                        || match shift.axis {
                            crate::refs::Axis::Row => {
                                anchor.row.saturating_add(h.saturating_sub(1)) >= shift.at
                            }
                            crate::refs::Axis::Col => {
                                anchor.col.saturating_add(w.saturating_sub(1)) >= shift.at
                            }
                        });
                if touches_shift {
                    seeds.push(key);
                }
                if h == 0 || w == 0 {
                    blocked_spills.push(key);
                }
            }
        }
        for (&old_anchor, &(_, watch_rect)) in self.spill_watches.iter() {
            if watch_rect.sheet as usize == sheet {
                let end = match shift.axis {
                    crate::refs::Axis::Row => watch_rect.r2,
                    crate::refs::Axis::Col => watch_rect.c2,
                };
                if end >= shift.at
                    && let Some(new_anchor) = relocate(old_anchor)
                {
                    seeds.push(new_anchor);
                }
            }
        }
        next.index_workbook_spills(wb, None, &blocked_spills);
        next.seal_points();
        if !super::recalc_dirty_cancelable(wb, &next, &seeds, cancel) {
            return None;
        }
        let mut post_blocked = Vec::new();
        for (si, sh) in wb.sheets.iter().enumerate() {
            for (&anchor, &(h, w)) in &sh.spill_shapes {
                if h == 0 || w == 0 {
                    post_blocked.push((si, anchor.row, anchor.col));
                }
            }
        }
        next.index_workbook_spills(wb, None, &post_blocked);
        if !next.supports_structural(wb) {
            return Some(Self::build_after_recalc(wb));
        }
        Some(next)
    }

    /// Identify old and new formula slots to reindex after an ordinary row/column
    /// edit. Large changes are left to the caller's background rebuild.
    pub fn structural_updates(
        &self,
        wb: &crate::Workbook,
        sheet: usize,
        op: &crate::structural::Structural,
        limit: usize,
    ) -> Option<Vec<CellKey>> {
        if !self.supports_structural(wb) {
            return None;
        }
        let shift = op.reference_shift();
        let mut slots = HashSet::new();
        for key in self
            .refs
            .keys()
            .chain(self.structural_sensitive.iter())
            .chain(self.volatile.iter())
            .chain(self.volatile_refs.iter())
        {
            let &(si, row, col) = key;
            let moved = if si == sheet {
                crate::sync::shift_structural_coord(row, col, op).map(|(r, c)| (si, r, c))
            } else {
                Some(*key)
            };
            if moved == Some(*key)
                && !self.structural_sensitive.contains(key)
                && !self.volatile_refs.contains(key)
                && !self.refs.get(key).is_some_and(|refs| {
                    refs.as_slice().iter().any(|r| {
                        r.sheet as usize == sheet
                            && (match shift.axis {
                                crate::refs::Axis::Row => r.r2,
                                crate::refs::Axis::Col => r.c2,
                            }) >= shift.at
                    })
                })
            {
                continue;
            }
            slots.insert(*key);
            slots.extend(moved);
            if slots.len() > limit {
                return None;
            }
        }
        // Include newly filled calculated-column rows if inserting rows into a Table.
        if let crate::structural::Structural::InsertRows { at, n } = *op
            && n > 0
            && let Some(sh) = wb.sheets.get(sheet)
            && !sh.tables.is_empty()
        {
            for t in &sh.tables {
                let (r1, c1, r2, c2) = t.range;
                let data_start = if t.header_row { r1 + 1 } else { r1 };
                let data_end = if t.totals_row {
                    r2.saturating_sub(1)
                } else {
                    r2
                };
                let max_insert = if t.totals_row { r2 } else { r2 + 1 };
                if at <= r1 || at > max_insert || data_start > data_end {
                    continue;
                }
                let source = if at > data_start { at - 1 } else { data_start };
                if source < data_start || source > data_end {
                    continue;
                }
                for col in c1..=c2 {
                    if sh
                        .get(source, col)
                        .is_some_and(|cell| cell.formula.is_some())
                    {
                        for k in 0..n {
                            slots.insert((sheet, at + k, col));
                            if slots.len() > limit {
                                return None;
                            }
                        }
                    }
                }
            }
        }
        Some(slots.into_iter().collect())
    }

    pub fn closure(&self, seeds: &[CellKey]) -> HashSet<CellKey> {
        self.collect(seeds, false, usize::MAX, None, None).unwrap()
    }

    pub(super) fn closure_excluding_watches(
        &self,
        seeds: &[CellKey],
        anchors: &HashSet<CellKey>,
    ) -> HashSet<CellKey> {
        let excluded: FxHashSet<u32> = anchors
            .iter()
            .filter_map(|k| self.spill_watches.get(k).map(|(wid, _)| *wid))
            .collect();
        let ex_opt = (!excluded.is_empty()).then_some(&excluded);
        self.collect(seeds, false, usize::MAX, None, ex_opt).unwrap()
    }

    pub fn edit_closure(&self, seeds: &[CellKey]) -> HashSet<CellKey> {
        self.collect(seeds, true, usize::MAX, None, None).unwrap()
    }

    pub fn edit_closure_limited(
        &self,
        seeds: &[CellKey],
        limit: usize,
    ) -> Option<HashSet<CellKey>> {
        self.collect(seeds, true, limit, None, None)
    }

    #[allow(dead_code)]
    pub(super) fn edit_closure_cancelable(
        &self,
        seeds: &[CellKey],
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> Option<HashSet<CellKey>> {
        self.collect(seeds, true, usize::MAX, cancel, None)
    }

    pub(super) fn edit_closure_vec_cancelable(
        &self,
        seeds: &[CellKey],
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> Option<Vec<CellKey>> {
        if self.slots.len() >= 512 {
            self.collect_vec_unbounded(seeds, true, cancel)
        } else {
            let set = self.collect(seeds, true, usize::MAX, cancel, None)?;
            Some(set.into_iter().collect())
        }
    }

    /// Returns `true` when `key` is a pure `=TODAY()` cell whose cached value already equals
    /// today's date serial, so unrelated edits on the same calendar day don't cascade 100k+
    /// downstream aging formulas unnecessarily.
    #[inline]
    fn is_unchanged_today(
        &self,
        wb: Option<(&crate::Workbook, f64)>,
        key: CellKey,
    ) -> bool {
        let Some((wb, today_serial)) = wb else {
            return false;
        };
        if !self.today_only_cells.contains(&key) {
            return false;
        }
        let Some(cell) = wb.sheets.get(key.0).and_then(|s| s.get(key.1, key.2)) else {
            return false;
        };
        if cell.value != crate::Value::Number(today_serial) {
            return false;
        }
        cell.formula.as_deref().is_some_and(|f| {
            f.trim()
                .trim_start_matches('=')
                .trim()
                .eq_ignore_ascii_case("TODAY()")
        })
    }

    pub(crate) fn edit_closure_vec_for_wb(
        &self,
        wb: &crate::Workbook,
        seeds: &[CellKey],
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> Option<Vec<CellKey>> {
        let today = if !self.today_only_cells.is_empty() {
            Some((wb, super::functions::now_serial_uncached_in(wb.date_system).floor()))
        } else {
            None
        };
        self.collect_vec_unbounded_inner(seeds, true, today, cancel)
    }

    /// High-throughput unbounded BFS using a compact L2-resident `[u64]` slot bitset (`125 KB` for 1M
    /// formulas instead of a 32 MB SipHash `HashSet<CellKey>`) and a cached per-column cursor.
    fn collect_vec_unbounded(
        &self,
        seeds: &[CellKey],
        edit: bool,
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> Option<Vec<CellKey>> {
        self.collect_vec_unbounded_inner(seeds, edit, None, cancel)
    }

    fn collect_vec_unbounded_inner(
        &self,
        seeds: &[CellKey],
        edit: bool,
        skip_today: Option<(&crate::Workbook, f64)>,
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> Option<Vec<CellKey>> {
        let mut seen_bits = vec![0u64; self.slots.len().div_ceil(64)];
        let mut dirty_vec: Vec<CellKey> = Vec::new();
        let mut frontier: Vec<CellKey> = seeds.to_vec();
        if edit && !seeds.is_empty() {
            for &vkey in self.volatile.iter() {
                if self.is_unchanged_today(skip_today, vkey) {
                    continue;
                }
                if let Some(&vid) = self.formula_ids.get(&vkey) {
                    let idx = vid as usize;
                    if idx < self.slots.len() {
                        seen_bits[idx >> 6] |= 1u64 << (idx & 63);
                    }
                }
                dirty_vec.push(vkey);
                frontier.push(vkey);
            }
        }
        let mut triggered_areas: FxHashSet<u32> = FxHashSet::default();
        let has_dyn = edit && !seeds.is_empty() && (!self.volatile_refs.is_empty() || !self.runtime_refs.is_empty());
        let mut seen_dyn: FxHashSet<CellKey> = FxHashSet::default();
        let mut next = Vec::new();
        let mut cached_sc: Option<(u32, u32)> = None;
        let mut cached_points: Option<&PointColumn> = None;
        let mut point_cursor = 0usize;
        let mut wave_idx = 0usize;

        while !frontier.is_empty() {
            if wave_idx & 255 == 0
                && cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed))
            {
                return None;
            }
            wave_idx += 1;
            if frontier.len() > 1 {
                frontier.sort_unstable_by_key(|&(s, r, c)| (s, c, r));
                frontier.dedup();
            }
            if has_dyn {
                let mut sorted_rc = frontier.clone();
                sorted_rc.sort_unstable();
                for dkey in self.dynamic_cells_touching(&sorted_rc) {
                    if let Some(&id) = self.formula_ids.get(&dkey) {
                        let idx = id as usize;
                        let word = idx >> 6;
                        let bit = 1u64 << (idx & 63);
                        if word < seen_bits.len() {
                            if (seen_bits[word] & bit) != 0 {
                                continue;
                            }
                            seen_bits[word] |= bit;
                        }
                        dirty_vec.push(dkey);
                        next.push(dkey);
                    } else if seen_dyn.insert(dkey) {
                        dirty_vec.push(dkey);
                        next.push(dkey);
                    }
                }
            }
            let mut add = |id: u32| -> bool {
                let idx = id as usize;
                let word = idx >> 6;
                let bit = 1u64 << (idx & 63);
                if word < seen_bits.len() {
                    if (seen_bits[word] & bit) != 0 {
                        return true;
                    }
                    seen_bits[word] |= bit;
                }
                if let Some(key) = self.slots.get(id) {
                    dirty_vec.push(key);
                    next.push(key);
                }
                true
            };
            for column in frontier.chunk_by(|a, b| a.0 == b.0 && a.2 == b.2) {
                let (sheet, _, col) = column[0];
                let Ok(sheet_u32) = u32::try_from(sheet) else {
                    continue;
                };
                let sc = (sheet_u32, col);
                if cached_sc != Some(sc) {
                    cached_sc = Some(sc);
                    cached_points = self.points_by_col.get(&sc);
                    point_cursor = 0;
                }
                if let Some(points) = cached_points {
                    let _ = points.query_sorted_rows_with_cursor(column, &mut point_cursor, &mut add);
                }
                if self.range_cols.contains(&sc) {
                    for shift in ROW_SHIFTS {
                        for rows in column.chunk_by(|a, b| bucket(a.1, shift) == bucket(b.1, shift))
                        {
                            let bin = bucket(rows[0].1, shift);
                            if let Some(entries) = self.by_col.get(&(sheet_u32, col, shift, bin)) {
                                for entry in entries.iter() {
                                    if triggered_areas.contains(&entry.area_id) {
                                        continue;
                                    }
                                    let p = rows.partition_point(|k| k.1 < entry.first);
                                    if p < rows.len() && rows[p].1 <= entry.last {
                                        triggered_areas.insert(entry.area_id);
                                        if let Some(area) = self.areas.get(entry.area_id as usize) {
                                            let _ = area.visit_subscribers(&mut add);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                if self.wide_sheets.contains(&sheet_u32) {
                    for shift in ROW_SHIFTS {
                        for rows in column.chunk_by(|a, b| bucket(a.1, shift) == bucket(b.1, shift))
                        {
                            let bin = bucket(rows[0].1, shift);
                            if let Some(entries) = self.wide_by_row.get(&(sheet_u32, shift, bin)) {
                                for entry in entries.iter() {
                                    if col < entry.c1
                                        || col > entry.c2
                                        || triggered_areas.contains(&entry.area_id)
                                    {
                                        continue;
                                    }
                                    let p = rows.partition_point(|k| k.1 < entry.r1);
                                    if p < rows.len() && rows[p].1 <= entry.r2 {
                                        triggered_areas.insert(entry.area_id);
                                        if let Some(area) = self.areas.get(entry.area_id as usize) {
                                            let _ = area.visit_subscribers(&mut add);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            frontier.clear();
            std::mem::swap(&mut frontier, &mut next);
        }
        Some(dirty_vec)
    }

    fn collect(
        &self,
        seeds: &[CellKey],
        edit: bool,
        limit: usize,
        cancel: Option<&std::sync::atomic::AtomicBool>,
        skip_first_wave_ids: Option<&FxHashSet<u32>>,
    ) -> Option<HashSet<CellKey>> {
        let mut dirty = HashSet::new();
        let mut frontier: Vec<CellKey> = seeds.to_vec();
        if edit && !seeds.is_empty() {
            if self.volatile.len() > limit {
                return None;
            }
            dirty.extend(self.volatile.iter().copied());
            frontier.extend(self.volatile.iter().copied());
        }
        let mut triggered_areas: FxHashSet<u32> = FxHashSet::default();
        let has_dyn = edit && !seeds.is_empty() && (!self.volatile_refs.is_empty() || !self.runtime_refs.is_empty());
        let mut next = Vec::new();
        let mut first_wave = true;
        let mut cached_sc: Option<(u32, u32)> = None;
        let mut cached_points: Option<&PointColumn> = None;
        let mut point_cursor = 0usize;
        while !frontier.is_empty() {
            if cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed)) {
                return None;
            }
            if frontier.len() > 1 {
                frontier.sort_unstable_by_key(|&(s, r, c)| (s, c, r));
                frontier.dedup();
            }
            if has_dyn {
                let mut sorted_rc = frontier.clone();
                sorted_rc.sort_unstable();
                for dkey in self.dynamic_cells_touching(&sorted_rc) {
                    if dirty.insert(dkey) {
                        next.push(dkey);
                        if dirty.len() > limit {
                            return None;
                        }
                    }
                }
            }
            let skip_now = if first_wave {
                skip_first_wave_ids
            } else {
                None
            };
            let mut add = |id: u32| -> bool {
                if skip_now.is_some_and(|ex| ex.contains(&id)) {
                    return true;
                }
                let Some(key) = self.slots.get(id) else {
                    return true;
                };
                if dirty.insert(key) {
                    next.push(key);
                }
                dirty.len() <= limit
            };
            for column in frontier.chunk_by(|a, b| a.0 == b.0 && a.2 == b.2) {
                let (sheet, _, col) = column[0];
                let Ok(sheet_u32) = u32::try_from(sheet) else {
                    continue;
                };
                let sc = (sheet_u32, col);
                if cached_sc != Some(sc) {
                    cached_sc = Some(sc);
                    cached_points = self.points_by_col.get(&sc);
                    point_cursor = 0;
                }

                // 1. O(log N) / O(1) cursor-assisted sorted point lookup on (sheet, col)
                if let Some(points) = cached_points
                    && !points.query_sorted_rows_with_cursor(column, &mut point_cursor, &mut add)
                {
                    return None;
                }

                // 2. Deduplicated multi-cell range intervals (skipped completely if column has none)
                if self.range_cols.contains(&(sheet_u32, col)) {
                    for shift in ROW_SHIFTS {
                        for rows in column.chunk_by(|a, b| bucket(a.1, shift) == bucket(b.1, shift))
                        {
                            let bin = bucket(rows[0].1, shift);
                            if let Some(entries) = self.by_col.get(&(sheet_u32, col, shift, bin)) {
                                for entry in entries.iter() {
                                    if triggered_areas.contains(&entry.area_id) {
                                        continue;
                                    }
                                    let p = rows.partition_point(|k| k.1 < entry.first);
                                    if p < rows.len() && rows[p].1 <= entry.last {
                                        triggered_areas.insert(entry.area_id);
                                        if let Some(area) = self.areas.get(entry.area_id as usize)
                                            && !area.visit_subscribers(&mut add)
                                        {
                                            return None;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                // 3. Wide ranges (> 512 cols) indexed symmetrically by row bucket
                if self.wide_sheets.contains(&sheet_u32) {
                    for shift in ROW_SHIFTS {
                        for rows in column.chunk_by(|a, b| bucket(a.1, shift) == bucket(b.1, shift))
                        {
                            let bin = bucket(rows[0].1, shift);
                            if let Some(entries) = self.wide_by_row.get(&(sheet_u32, shift, bin)) {
                                for entry in entries.iter() {
                                    if col < entry.c1
                                        || col > entry.c2
                                        || triggered_areas.contains(&entry.area_id)
                                    {
                                        continue;
                                    }
                                    let p = rows.partition_point(|k| k.1 < entry.r1);
                                    if p < rows.len() && rows[p].1 <= entry.r2 {
                                        triggered_areas.insert(entry.area_id);
                                        if let Some(area) = self.areas.get(entry.area_id as usize)
                                            && !area.visit_subscribers(&mut add)
                                        {
                                            return None;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            frontier.clear();
            std::mem::swap(&mut frontier, &mut next);
            first_wave = false;
        }
        Some(dirty)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Cell, Sheet, Value, Workbook, formula};

    fn formula(wb: &mut Workbook, si: usize, row: u32, col: u32, text: &str) {
        wb.sheets[si].set(
            row,
            col,
            Cell {
                value: Value::Number(0.0),
                formula: Some(text.into()),
            },
        );
    }

    #[test]
    fn edit_seeds_include_volatile_cells_and_their_dependents() {
        let mut wb = Workbook::new_blank();
        formula(&mut wb, 0, 0, 0, "=TODAY()");
        formula(&mut wb, 0, 0, 1, "=A1+1");
        formula(&mut wb, 0, 0, 2, "=7");
        let d = Deps::build(&wb);
        assert!(!d.requires_full_recalc());
        let seed = [(0, 5, 0)];
        assert!(d.closure(&seed).is_empty());
        assert_eq!(d.edit_closure(&seed), HashSet::from([(0, 0, 0), (0, 0, 1)]));
        assert_eq!(formula::recalc_dirty(&mut wb, &d, &seed), 2);
        let Value::Number(today) = wb.sheets[0].value(0, 0) else {
            panic!("expected Excel date")
        };
        assert!(today > 1.0);
        assert_eq!(wb.sheets[0].value(0, 1), Value::Number(today + 1.0));
    }

    #[test]
    fn names_follow_sheet_scope_and_let_sequential_binding() {
        let mut wb = Workbook::new_blank();
        wb.sheets.push(Sheet::new("Report"));
        wb.names.extend([
            ("Rate".to_owned(), None, "Sheet1!A1".to_owned()),
            ("Rate".to_owned(), Some(1), "Report!B1".to_owned()),
            ("Alias".to_owned(), None, "Rate".to_owned()),
        ]);
        formula(&mut wb, 1, 0, 2, "=LET(rate,Alias,f,LAMBDA(x,x+rate),f(3))");
        formula(&mut wb, 0, 0, 2, "=Alias");
        formula(&mut wb, 0, 0, 3, "=Report!Rate");
        let d = Deps::build(&wb);
        assert!(!d.requires_full_recalc());
        assert_eq!(
            d.closure(&[(1, 0, 1)]),
            HashSet::from([(1, 0, 2), (0, 0, 3)])
        );
        assert_eq!(d.closure(&[(0, 0, 0)]), HashSet::from([(0, 0, 2)]));
        formula(&mut wb, 0, 0, 4, "=LET(place,\"A1\",INDIRECT(place))");
        let d = Deps::build(&wb);
        assert!(!d.requires_full_recalc());
        assert!(d.edit_closure(&[(1, 9, 9)]).contains(&(0, 0, 4)));
        wb.names
            .push(("LeftAlias".to_owned(), None, "RightAlias".to_owned()));
        wb.names
            .push(("RightAlias".to_owned(), None, "LeftAlias".to_owned()));
        let d = dependencies(&wb, (0, 0, 0), "=LeftAlias");
        assert!(d.runtime);
    }

    #[test]
    fn undefined_name_and_parse_error_do_not_degrade_to_full_recalc() {
        let mut wb = Workbook::new_blank();
        formula(&mut wb, 0, 0, 1, "=A1+1");
        formula(&mut wb, 0, 0, 2, "=UndefinedName+1");
        formula(&mut wb, 0, 0, 3, "=C1*2");
        formula(&mut wb, 0, 0, 4, "=SUM(1,"); // syntax error
        let d = Deps::build(&wb);
        assert!(!d.requires_full_recalc());
        let closure = d.edit_closure(&[(0, 0, 0)]);
        assert!(closure.contains(&(0, 0, 1)));
        assert!(closure.contains(&(0, 0, 2)));
        assert!(closure.contains(&(0, 0, 3)));
        assert!(!closure.contains(&(0, 0, 4)));
    }

    #[test]
    fn cow_make_mut_update_preserves_shared_snapshot() {
        let mut wb = Workbook::new_blank();
        formula(&mut wb, 0, 0, 1, "=A1+1");
        formula(&mut wb, 0, 1, 1, "=A2+1");
        let mut deps = Arc::new(Deps::build(&wb));
        let snap = Arc::clone(&deps);
        formula(&mut wb, 0, 0, 1, "=A3+5");
        Arc::make_mut(&mut deps).update_cell(&wb, (0, 0, 1));
        assert_eq!(snap.closure(&[(0, 0, 0)]), HashSet::from([(0, 0, 1)]));
        assert!(snap.closure(&[(0, 2, 0)]).is_empty());
        assert!(deps.closure(&[(0, 0, 0)]).is_empty());
        assert_eq!(deps.closure(&[(0, 2, 0)]), HashSet::from([(0, 0, 1)]));
        assert_eq!(deps.closure(&[(0, 1, 0)]), HashSet::from([(0, 1, 1)]));
    }

    #[test]
    fn index_updates_shared_buckets_wide_ranges_and_duplicate_refs() {
        let mut wb = Workbook::new_blank();
        formula(&mut wb, 0, 0, 0, "=SUM(B31:B34,B33,B33,AA1:XFD90)");
        formula(&mut wb, 0, 1, 0, "=B33");
        let mut d = Deps::build(&wb);
        assert_eq!(d.closure(&[(0, 32, 1)]).len(), 2);
        assert!(d.closure(&[(0, 33, 1)]).contains(&(0, 0, 0)));
        assert_eq!(d.closure(&[(0, 34, 1)]).len(), 0);
        assert_eq!(d.closure(&[(0, 42, 999)]).len(), 1);
        formula(&mut wb, 0, 0, 0, "=SUM(C1:C2)");
        d.update_cell(&wb, (0, 0, 0));
        assert_eq!(d.closure(&[(0, 32, 1)]), HashSet::from([(0, 1, 0)]));
        assert!(d.closure(&[(0, 42, 999)]).is_empty());
        assert_eq!(d.closure(&[(0, 1, 2)]), HashSet::from([(0, 0, 0)]));
        assert_eq!(d.rect_count, 2);
    }

    #[test]
    fn interactive_defers_large_static_closure_and_worker_honours_cancellation() {
        use formula::InteractiveRecalc::*;
        use std::sync::atomic::AtomicBool;
        let mut wb = Workbook::new_blank();
        for r in 1..1300 {
            formula(&mut wb, 0, r, 0, &format!("=A{}+1", r));
        }
        let d = Deps::build(&wb);
        let seeds = [(0, 0, 0)];
        assert!(d.edit_closure_limited(&seeds, 96).is_none());
        assert_eq!(
            formula::recalc_interactive_plan(&mut wb, &d, &seeds, true),
            BackgroundDirty
        );
        assert_eq!(
            formula::recalc_interactive_plan(&mut wb, &d, &seeds, false),
            BackgroundFull
        );
        assert!(!formula::recalc_dirty_cancelable(
            &mut wb,
            &d,
            &seeds,
            &AtomicBool::new(true)
        ));
        assert!(formula::recalc_dirty_cancelable(
            &mut wb,
            &d,
            &seeds,
            &AtomicBool::new(false)
        ));
        assert_eq!(wb.sheets[0].value(1299, 0), Value::Number(1299.0));
    }

    #[test]
    fn parallel_build_matches_cross_sheet_row_closure() {
        let mut wb = Workbook::new_blank();
        wb.sheets.push(Sheet::new("Report"));
        for r in 0..8192 {
            wb.sheets[0].set(
                r,
                0,
                Cell {
                    value: Value::Number(2.0),
                    formula: None,
                },
            );
            formula(&mut wb, 1, r, 0, &format!("=Sheet1!A{}", r + 1));
        }
        let d = Deps::build(&wb);
        assert_eq!(d.rect_count, 8192);
        assert_eq!(
            d.closure(&[(0, 999, 0), (0, 8191, 0)]),
            HashSet::from([(1, 999, 0), (1, 8191, 0)])
        );
    }

    #[test]
    #[ignore = "manual dependency-chain timing, run with --release --ignored --nocapture"]
    fn dependency_chain_timing() {
        for n in [10_000, 20_000] {
            let mut wb = Workbook::new_blank();
            for r in 1..=n {
                formula(&mut wb, 0, r, 0, &format!("=A{}+1", r));
            }
            let at = std::time::Instant::now();
            let d = Deps::build(&wb);
            let build = at.elapsed();
            let at = std::time::Instant::now();
            assert_eq!(d.closure(&[(0, 0, 0)]).len(), n as usize);
            eprintln!("{n} formulas: build {build:?}, closure {:?}", at.elapsed());
        }
    }
}

#[cfg(test)]
mod structural_tests {
    use super::*;
    use crate::{
        Cell, Sheet, Value, Workbook,
        structural::{Structural, apply_structural_wb},
        table::Table,
    };

    #[test]
    fn incremental_axis_edits_match_full_and_reindex_cross_sheet_readers() {
        for op in [
            Structural::InsertRows { at: 3, n: 1 },
            Structural::DeleteRows { at: 3, n: 1 },
            Structural::InsertCols { at: 3, n: 1 },
            Structural::DeleteCols { at: 3, n: 1 },
        ] {
            let mut wb = Workbook::new_blank();
            wb.sheets.push(Sheet::new("Report"));
            wb.sheets[0].set(
                3,
                3,
                Cell {
                    value: Value::Number(7.0),
                    formula: None,
                },
            );
            for (si, r, c, f) in [
                (0, 0, 0, "=2+2"),
                (0, 4, 4, "=ROW()+COLUMN()"),
                (1, 0, 0, "=Sheet1!D4"),
                (1, 0, 1, "=A1+2"),
                (1, 0, 2, "=SUM(Sheet1!A1:E5)"),
                (1, 0, 3, "=SUM(A10:B10)"),
            ] {
                wb.sheets[si].set(
                    r,
                    c,
                    Cell {
                        value: Value::Empty,
                        formula: Some(f.into()),
                    },
                );
            }
            crate::formula::recalc_all(&mut wb);
            assert!(
                Deps::build(&wb).structural_updates(&wb, 0, &op, 50).is_none(),
                "building edges alone cannot certify cached formula values"
            );
            let mut deps = Deps::build_after_recalc(&wb);
            let old = deps.clone();
            let keys = deps.structural_updates(&wb, 0, &op, 50).unwrap();
            assert!(keys.contains(&(1, 0, 0)));
            assert!(!keys.contains(&(1, 0, 3)));
            apply_structural_wb(&mut wb, 0, &op);
            for &k in &keys {
                deps.update_cell(&wb, k);
            }
            let mut background = wb.clone();
            let broad = old
                .recalc_structural(&mut background, 0, &op, &false.into())
                .unwrap();
            let mut full = wb.clone();
            crate::formula::recalc_all(&mut full);
            crate::formula::recalc_dirty(&mut wb, &deps, &keys);
            for (si, sheet) in full.sheets.iter().enumerate() {
                for (pos, cell) in sheet.iter() {
                    assert_eq!(
                        background.sheets[si].value(pos.row, pos.col),
                        cell.value,
                        "background {op:?} at {pos:?}"
                    );
                    assert_eq!(
                        wb.sheets[si].value(pos.row, pos.col),
                        cell.value,
                        "{op:?} at {pos:?}"
                    );
                }
            }
            let rebuilt = Deps::build(&wb);
            for r in 0..7 {
                for c in 0..7 {
                    assert_eq!(
                        deps.edit_closure(&[(0, r, c)]),
                        rebuilt.edit_closure(&[(0, r, c)])
                    );
                    assert_eq!(
                        broad.edit_closure(&[(0, r, c)]),
                        rebuilt.edit_closure(&[(0, r, c)])
                    );
                }
            }
        }
    }

    #[test]
    fn incremental_structural_supports_defined_names_and_tables() {
        for op in [
            Structural::InsertRows { at: 2, n: 1 },
            Structural::DeleteRows { at: 1, n: 1 },
        ] {
            let mut wb = Workbook::new_blank();
            wb.sheets.push(Sheet::new("Report"));
            wb.sheets[0].set(1, 0, Cell { value: Value::Number(4.0), formula: None });
            wb.sheets[0].set(2, 0, Cell { value: Value::Number(7.0), formula: None });
            wb.sheets[0].set(1, 1, Cell { value: Value::Empty, formula: Some("=A2*10".into()) });
            wb.sheets[0].set(2, 1, Cell { value: Value::Empty, formula: Some("=A3*10".into()) });
            wb.sheets[0].tables.push(Table {
                name: "Data".into(),
                display: "Data".into(),
                range: (0, 0, 2, 1),
                columns: vec!["Amount".into(), "Scaled".into()],
                column_ids: vec![1, 2],
                header_row: true,
                ..Default::default()
            });
            wb.names
                .push(("AmountCells".into(), None, "Sheet1!$A$2:$A$3".into()));
            wb.sheets[1].set(
                0,
                0,
                Cell {
                    value: Value::Empty,
                    formula: Some("=SUM(AmountCells)+SUM(Data[Amount])+SUM(Data[Scaled])".into()),
                },
            );
            wb.sheets[1].set(
                0,
                1,
                Cell {
                    value: Value::Empty,
                    formula: Some("=A1*2".into()),
                },
            );
            crate::formula::recalc_all(&mut wb);
            let deps = Deps::build_after_recalc(&wb);
            assert!(deps.supports_structural(&wb));
            let mut inc = wb.clone();
            apply_structural_wb(&mut inc, 0, &op);
            let next_deps = deps
                .recalc_structural(&mut inc, 0, &op, &false.into())
                .expect("recalc_structural should handle names and tables");
            let mut full = wb.clone();
            apply_structural_wb(&mut full, 0, &op);
            crate::formula::recalc_all(&mut full);
            for (si, sh) in full.sheets.iter().enumerate() {
                for (pos, cell) in sh.iter() {
                    assert_eq!(
                        inc.sheets[si].value(pos.row, pos.col),
                        cell.value,
                        "{op:?} sheet {si} at {pos:?}"
                    );
                }
            }
            let rebuilt = Deps::build_after_recalc(&inc);
            for r in 0..5 {
                assert_eq!(
                    next_deps.edit_closure(&[(0, r, 0)]),
                    rebuilt.edit_closure(&[(0, r, 0)]),
                    "{op:?} closure at row {r}"
                );
            }
        }
    }
}

#[cfg(test)]
mod structural_lambda_tests {
    use super::*;
    use crate::{
        Sheet, Value, Workbook,
        formula::{self, FValue},
        structural::{Structural, apply_structural_wb},
    };

    #[test]
    fn scoped_axis_recalc_recovers_a_callable_from_another_sheets_cached_display_value() {
        let mut wb = Workbook::new_blank();
        wb.sheets.push(Sheet::new("Functions"));
        wb.sheets.push(Sheet::new("Transfer"));
        wb.sheets.push(Sheet::new("Summary"));
        for (si, r, c, src) in [
            (1, 0, 0, "=LAMBDA(n,n*3)"),
            (2, 0, 0, "=Functions!A1"),
            (0, 2, 3, "=MAP(A3:B3,Transfer!A1)"),
            (3, 0, 0, "=SUM(Sheet1!D3#)"),
        ] {
            formula::set_formula(wb.sheet_mut(si), r, c, src, FValue::Empty);
        }
        for (col, value) in [(0, 5.), (1, 7.)] {
            wb.sheet_mut(0).set(
                2,
                col,
                crate::Cell {
                    value: Value::Number(value),
                    formula: None,
                },
            );
        }
        formula::recalc_all_force(&mut wb);
        assert_eq!(wb.sheet(3).value(0, 0), Value::Number(36.));
        let graph = Deps::build_after_recalc(&wb);
        let op = Structural::InsertRows { at: 1, n: 1 };
        apply_structural_wb(&mut wb, 0, &op);
        let mut full = wb.clone();
        formula::recalc_all_force(&mut full);
        graph
            .recalc_structural_sheets(&mut wb, 0, &false.into())
            .unwrap();
        assert_eq!(wb.sheet(3).value(0, 0), full.sheet(3).value(0, 0));
        assert_eq!(wb.sheet(0).value(3, 4), Value::Number(21.));
    }

    #[test]
    fn dirty_levels_preserve_display_text_and_resolve_external_callable_chains() {
        let mut wb = Workbook::new_blank();
        wb.sheets.push(Sheet::new("Functions"));
        formula::set_formula(wb.sheet_mut(1), 0, 0, "=LAMBDA(n,n*3)", FValue::Empty);
        formula::set_formula(wb.sheet_mut(1), 1, 0, "=A1", FValue::Empty);
        formula::set_formula(wb.sheet_mut(1), 2, 0, "=7", FValue::Empty);
        let mut seeds = Vec::new();
        for r in 0..2050 {
            wb.sheet_mut(0).set(
                r,
                0,
                crate::Cell {
                    value: Value::Number(1.),
                    formula: None,
                },
            );
            let n = r + 1;
            formula::set_formula(
                wb.sheet_mut(0),
                r,
                1,
                &format!("=SUM(MAP(A{n}:A{n},Functions!$A$2))+LEN(Functions!$A$3)"),
                FValue::Empty,
            );
            seeds.push((0, r, 0));
        }
        formula::recalc_all_force(&mut wb);
        // A genuine text cache using the closure display marker cannot be replaced
        // by evaluating its formula (this cell is outside the edit closure).
        wb.sheet_mut(1)
            .set_formula_cache(2, 0, Value::Text("#CALC!".into()));
        let deps = Deps::build(&wb);
        for &(s, r, c) in &seeds {
            wb.sheet_mut(s).set(
                r,
                c,
                crate::Cell {
                    value: Value::Number(5.),
                    formula: None,
                },
            );
        }
        formula::recalc_dirty(&mut wb, &deps, &seeds);
        for r in [0, 130, 2049] {
            assert_eq!(wb.sheet(0).value(r, 1), Value::Number(21.));
        }
        assert_eq!(wb.sheet(1).value(2, 0), Value::Text("#CALC!".into()));

        // Imported display markers may be stale. Cyclic forwarded candidates
        // must terminate and keep their previous scalar cache.
        for (r, f) in [(0, "=A2"), (1, "=A1")] {
            formula::set_formula(wb.sheet_mut(1), r, 0, f, FValue::Text("#CALC!".into()));
        }
        formula::set_formula(wb.sheet_mut(0), 0, 3, "=LEN(Functions!A1)", FValue::Empty);
        let keys = HashSet::from([(0, 0, 3)]);
        let mut calc = formula::Calc::new(&wb);
        calc.scope = Some(&keys);
        assert_eq!(calc.cell_value(0, 0, 3), FValue::Number(6.));
    }
}
