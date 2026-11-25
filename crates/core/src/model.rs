use crate::store::{CellView, ColSlab, ColSlice, Store};
use std::borrow::Cow;
use std::collections::HashMap;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CellRef {
    pub row: u32, // 0-based
    pub col: u32, // 0-based
}

impl CellRef {
    pub fn new(row: u32, col: u32) -> Self {
        Self { row, col }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub enum Value {
    #[default]
    Empty,
    Number(f64),
    Text(std::sync::Arc<str>),
    Bool(bool),
    /// 真正的错误值（`=1/0` 算出来的），与「用户输入的文本 `#DIV/0!`」是两回事。
    Error(crate::formula::ErrKind),
}

/// 驻留字符串池：在单次流式加载、排序或聚合中将重复文本去重为共享 `Arc<str>`，
/// 命中已有文本时零堆分配（直接 `HashSet<Arc<str>>::get(&str)` 后递增引用计数）。
#[derive(Clone, Debug, Default)]
pub struct StringPool {
    table: std::collections::HashSet<std::sync::Arc<str>>,
}

impl StringPool {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_capacity(cap: usize) -> Self {
        Self {
            table: std::collections::HashSet::with_capacity(cap),
        }
    }

    #[inline]
    pub fn empty_arc() -> std::sync::Arc<str> {
        static EMPTY: std::sync::OnceLock<std::sync::Arc<str>> = std::sync::OnceLock::new();
        EMPTY.get_or_init(|| "".into()).clone()
    }

    pub fn intern(&mut self, s: &str) -> std::sync::Arc<str> {
        if s.is_empty() {
            return Self::empty_arc();
        }
        if let Some(existing) = self.table.get(s) {
            return existing.clone();
        }
        let arc: std::sync::Arc<str> = s.into();
        self.table.insert(arc.clone());
        arc
    }

    pub fn intern_owned(&mut self, s: String) -> std::sync::Arc<str> {
        if s.is_empty() {
            return Self::empty_arc();
        }
        if let Some(existing) = self.table.get(s.as_str()) {
            return existing.clone();
        }
        let arc: std::sync::Arc<str> = s.into();
        self.table.insert(arc.clone());
        arc
    }

    pub fn intern_arc(&mut self, s: std::sync::Arc<str>) -> std::sync::Arc<str> {
        if s.is_empty() {
            return Self::empty_arc();
        }
        if let Some(existing) = self.table.get(s.as_ref()) {
            return existing.clone();
        }
        self.table.insert(s.clone());
        s
    }

    pub fn len(&self) -> usize {
        self.table.len()
    }

    pub fn is_empty(&self) -> bool {
        self.table.is_empty()
    }

    pub fn clear(&mut self) {
        self.table.clear();
    }
}

/// 零堆分配栈缓冲数字格式化回调：整数走纯整数除法，普通小数走 64B 栈缓冲并去除尾零。
#[inline]
pub fn with_formatted_number<R>(n: f64, f: impl FnOnce(&str) -> R) -> R {
    use std::fmt::Write;
    if n.is_nan() || n.is_infinite() {
        return f("#NUM!");
    }
    if n.fract() == 0.0 && n.abs() < 1e15 {
        let mut buf = [0u8; 24];
        return f(crate::style::format_i64_into(n as i64, &mut buf));
    }
    let mut stack = crate::style::StackFmt::<64>::new();
    if write!(stack, "{:.10}", n).is_ok() {
        stack.trim_decimal_zeros_from(0);
        let s = stack.as_str();
        if s == "-0" {
            return f("0");
        }
        return f(s);
    }
    let s = format!("{:.10}", n);
    let trimmed = s.trim_end_matches('0').trim_end_matches('.');
    f(if trimmed == "-0" { "0" } else { trimmed })
}

/// 数字显示：整数不带小数点，小数最多 10 位并去掉尾零
#[inline]
pub fn format_number(n: f64) -> String {
    with_formatted_number(n, str::to_owned)
}

/// 线程局部短字符串直接映射驻留池（2048 槽，仅驻留 `<= 128 B` 文本），
/// 让运行期 `parse_input`、查找替换、分列等高频路径自动共享同一 `Arc<str>` 分配。
#[inline]
pub fn intern_short_str(s: &str) -> std::sync::Arc<str> {
    if s.is_empty() {
        return StringPool::empty_arc();
    }
    if s.len() > 128 {
        return s.into();
    }
    const SLOTS: usize = 2048;
    thread_local! {
        static CACHE: std::cell::RefCell<Box<[Option<std::sync::Arc<str>>]>> =
            std::cell::RefCell::new(vec![None; SLOTS].into_boxed_slice());
    }
    let mut h = 0xcbf29ce484222325u64 ^ (s.len() as u64);
    for &b in s.as_bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100000001b3);
    }
    let idx = ((h ^ (h >> 32)) as usize) & (SLOTS - 1);
    CACHE.with(|cell| {
        let mut slots = cell.borrow_mut();
        if let Some(existing) = &slots[idx]
            && existing.as_ref() == s
        {
            return existing.clone();
        }
        let arc: std::sync::Arc<str> = s.into();
        slots[idx] = Some(arc.clone());
        arc
    })
}

impl Value {
    pub(crate) fn same_stored_value(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Number(a), Self::Number(b)) => a.to_bits() == b.to_bits(),
            (a, b) => a == b,
        }
    }

    /// 显示用文本（数字去掉多余的 .0）
    pub fn display(&self) -> String {
        match self {
            Value::Empty => String::new(),
            Value::Number(n) => format_number(*n),
            Value::Text(s) => s.to_string(),
            Value::Bool(b) => if *b { "TRUE".into() } else { "FALSE".into() },
            Value::Error(e) => e.text().to_string(),
        }
    }

    /// 零堆分配访问显示字符串（`Text`/`Bool`/`Error`/`Empty` 直接借用，`Number` 走栈缓冲）
    #[inline]
    pub fn with_display_str<R>(&self, f: impl FnOnce(&str) -> R) -> R {
        match self {
            Value::Empty => f(""),
            Value::Number(n) => with_formatted_number(*n, f),
            Value::Text(s) => f(s.as_ref()),
            Value::Bool(b) => f(if *b { "TRUE" } else { "FALSE" }),
            Value::Error(e) => f(e.text()),
        }
    }

    /// 用户输入解析：识别数字、日期/时间、百分比、千分位、货币、TRUE/FALSE，前导撇号强制文本（短文本自动驻留去重）
    pub fn parse_input(s: &str) -> Value {
        crate::input_parse::parse_cell_input(s, "", DateSystem::D1900).value
    }

    pub fn is_empty(&self) -> bool {
        matches!(self, Value::Empty)
    }

    #[inline]
    pub fn text(s: impl Into<std::sync::Arc<str>>) -> Self {
        Value::Text(s.into())
    }

    #[inline]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Text(s) => Some(s),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cell {
    pub value: Value,
    /// Immutable source, shared across calculation snapshots, undo and page-column copies.
    pub formula: Option<std::sync::Arc<str>>,
}

impl Cell {
    /// Formula caches are calculated state. Compare only the editable input.
    pub fn same_input(&self, other: &Self) -> bool {
        if self.formula.is_some() || other.formula.is_some() {
            return self.formula == other.formula;
        }
        self.value.same_stored_value(&other.value)
    }

    pub fn is_blank(&self) -> bool {
        self.value.is_empty() && self.formula.is_none()
    }
}

/// Rarely edited per-cell metadata shared by save, calculation and undo snapshots.
#[derive(Clone, Debug)]
pub struct CellMetadata<K, V>(std::sync::Arc<HashMap<K, V>>);

impl<K, V> Default for CellMetadata<K, V> {
    fn default() -> Self { Self(Default::default()) }
}

impl<K, V> From<HashMap<K, V>> for CellMetadata<K, V> {
    fn from(map: HashMap<K, V>) -> Self { Self(std::sync::Arc::new(map)) }
}

impl<K: Eq + std::hash::Hash, V> FromIterator<(K, V)> for CellMetadata<K, V> {
    fn from_iter<T: IntoIterator<Item = (K, V)>>(iter: T) -> Self {
        Self(std::sync::Arc::new(iter.into_iter().collect()))
    }
}

impl<K, V> CellMetadata<K, V> {
    pub fn ptr_eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }

    pub fn clear(&mut self) {
        if let Some(unique) = std::sync::Arc::get_mut(&mut self.0) {
            unique.clear();
        } else if !self.0.is_empty() {
            self.0 = Default::default();
        }
    }
}

impl<K, V> std::ops::Deref for CellMetadata<K, V> {
    type Target = HashMap<K, V>;
    fn deref(&self) -> &Self::Target { &self.0 }
}

impl<K: Clone, V: Clone> std::ops::DerefMut for CellMetadata<K, V> {
    fn deref_mut(&mut self) -> &mut Self::Target { std::sync::Arc::make_mut(&mut self.0) }
}

impl<K: Clone, V: Clone> IntoIterator for CellMetadata<K, V> {
    type Item = (K, V);
    type IntoIter = std::collections::hash_map::IntoIter<K, V>;
    fn into_iter(self) -> Self::IntoIter { std::sync::Arc::unwrap_or_clone(self.0).into_iter() }
}

impl<K: Eq + std::hash::Hash, V: PartialEq> PartialEq for CellMetadata<K, V> {
    fn eq(&self, other: &Self) -> bool { std::sync::Arc::ptr_eq(&self.0, &other.0) || self.0 == other.0 }
}

impl<K: Eq + std::hash::Hash, V: PartialEq> PartialEq<HashMap<K, V>> for CellMetadata<K, V> {
    fn eq(&self, other: &HashMap<K, V>) -> bool { &*self.0 == other }
}

impl<K: Eq + std::hash::Hash, V: Eq> Eq for CellMetadata<K, V> {}

/// Copy-on-write set wrapper for sparse cell/coordinate sets (`spilled`, `ignored_errors`).
#[derive(Clone, Debug)]
pub struct CellSet<T>(std::sync::Arc<std::collections::HashSet<T>>);

impl<T> Default for CellSet<T> {
    fn default() -> Self { Self(Default::default()) }
}

impl<T> From<std::collections::HashSet<T>> for CellSet<T> {
    fn from(set: std::collections::HashSet<T>) -> Self { Self(std::sync::Arc::new(set)) }
}

impl<T: Eq + std::hash::Hash> FromIterator<T> for CellSet<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        Self(std::sync::Arc::new(iter.into_iter().collect()))
    }
}

impl<T> CellSet<T> {
    pub fn new() -> Self { Self::default() }

    pub fn ptr_eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }

    pub fn clear(&mut self) {
        if let Some(unique) = std::sync::Arc::get_mut(&mut self.0) {
            unique.clear();
        } else if !self.0.is_empty() {
            self.0 = Default::default();
        }
    }

    #[inline]
    pub fn remove_if_present(&mut self, value: &T) -> bool
    where
        T: Eq + std::hash::Hash + Clone,
    {
        if self.0.is_empty() || !self.0.contains(value) {
            return false;
        }
        std::sync::Arc::make_mut(&mut self.0).remove(value)
    }
}

impl<T> std::ops::Deref for CellSet<T> {
    type Target = std::collections::HashSet<T>;
    fn deref(&self) -> &Self::Target { &self.0 }
}

impl<T: Clone> std::ops::DerefMut for CellSet<T> {
    fn deref_mut(&mut self) -> &mut Self::Target { std::sync::Arc::make_mut(&mut self.0) }
}

impl<T: Eq + std::hash::Hash> PartialEq for CellSet<T> {
    fn eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0) || self.0 == other.0
    }
}

impl<T: Eq + std::hash::Hash> PartialEq<std::collections::HashSet<T>> for CellSet<T> {
    fn eq(&self, other: &std::collections::HashSet<T>) -> bool {
        &*self.0 == other
    }
}

impl<T: Eq + std::hash::Hash> Eq for CellSet<T> {}

impl<T: Clone> IntoIterator for CellSet<T> {
    type Item = T;
    type IntoIter = std::collections::hash_set::IntoIter<T>;
    fn into_iter(self) -> Self::IntoIter {
        std::sync::Arc::unwrap_or_clone(self.0).into_iter()
    }
}

impl<'a, T> IntoIterator for &'a CellSet<T> {
    type Item = &'a T;
    type IntoIter = std::collections::hash_set::Iter<'a, T>;
    fn into_iter(self) -> Self::IntoIter { self.0.iter() }
}

/// Copy-on-write vector wrapper for rarely mutated heavy lists (such as `Sheet::charts`).
#[derive(Clone, Debug)]
pub struct SharedList<T>(std::sync::Arc<Vec<T>>);

impl<T> Default for SharedList<T> {
    fn default() -> Self { Self(Default::default()) }
}

impl<T> From<Vec<T>> for SharedList<T> {
    fn from(vec: Vec<T>) -> Self { Self(std::sync::Arc::new(vec)) }
}

impl<T> FromIterator<T> for SharedList<T> {
    fn from_iter<I: IntoIterator<Item = T>>(iter: I) -> Self {
        Self(std::sync::Arc::new(iter.into_iter().collect()))
    }
}

impl<T> SharedList<T> {
    pub fn new() -> Self { Self::default() }

    pub fn ptr_eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0)
    }

    pub fn clear(&mut self) {
        if let Some(unique) = std::sync::Arc::get_mut(&mut self.0) {
            unique.clear();
        } else if !self.0.is_empty() {
            self.0 = Default::default();
        }
    }
}

impl<T: Clone> SharedList<T> {
    pub fn push(&mut self, value: T) {
        std::sync::Arc::make_mut(&mut self.0).push(value);
    }
}

impl<T> std::ops::Deref for SharedList<T> {
    type Target = Vec<T>;
    fn deref(&self) -> &Self::Target { &self.0 }
}

impl<T: Clone> std::ops::DerefMut for SharedList<T> {
    fn deref_mut(&mut self) -> &mut Self::Target { std::sync::Arc::make_mut(&mut self.0) }
}

impl<T> AsRef<[T]> for SharedList<T> {
    fn as_ref(&self) -> &[T] { &self.0 }
}

impl<T: PartialEq> PartialEq for SharedList<T> {
    fn eq(&self, other: &Self) -> bool {
        std::sync::Arc::ptr_eq(&self.0, &other.0) || self.0 == other.0
    }
}

impl<T: PartialEq> PartialEq<Vec<T>> for SharedList<T> {
    fn eq(&self, other: &Vec<T>) -> bool { self.0.as_slice() == other.as_slice() }
}

impl<T: PartialEq> PartialEq<[T]> for SharedList<T> {
    fn eq(&self, other: &[T]) -> bool { self.0.as_slice() == other }
}

impl<T: PartialEq> PartialEq<&[T]> for SharedList<T> {
    fn eq(&self, other: &&[T]) -> bool { self.0.as_slice() == *other }
}

impl<T: Eq> Eq for SharedList<T> {}

impl<T: Clone> IntoIterator for SharedList<T> {
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;
    fn into_iter(self) -> Self::IntoIter {
        std::sync::Arc::unwrap_or_clone(self.0).into_iter()
    }
}

impl<'a, T> IntoIterator for &'a SharedList<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;
    fn into_iter(self) -> Self::IntoIter { self.0.iter() }
}

impl<'a, T: Clone> IntoIterator for &'a mut SharedList<T> {
    type Item = &'a mut T;
    type IntoIter = std::slice::IterMut<'a, T>;
    fn into_iter(self) -> Self::IntoIter { std::sync::Arc::make_mut(&mut self.0).iter_mut() }
}

/// Page-level 256-row bitmap journal (`cleared` and `style_edits`).
/// Stores 1 bit per cell per `(page, col)` (`[u64; 4]` = 32 bytes for 256 rows),
/// shrinking 1,000,000 cleared/styled cells from ~48 MB `HashSet<CellRef>` to ~750 KB.
#[derive(Clone, Debug, Default)]
pub struct CellJournal {
    pages: std::sync::Arc<HashMap<(u32, u32), [u64; 4]>>,
    len: usize,
}

impl PartialEq for CellJournal {
    fn eq(&self, other: &Self) -> bool {
        self.len == other.len
            && (std::sync::Arc::ptr_eq(&self.pages, &other.pages) || self.pages == other.pages)
    }
}

impl Eq for CellJournal {}

impl<const N: usize> From<[CellRef; N]> for CellJournal {
    fn from(keys: [CellRef; N]) -> Self {
        keys.into_iter().collect()
    }
}

impl FromIterator<CellRef> for CellJournal {
    fn from_iter<T: IntoIterator<Item = CellRef>>(iter: T) -> Self {
        let mut out = Self::default();
        out.extend(iter);
        out
    }
}

impl Extend<CellRef> for CellJournal {
    fn extend<T: IntoIterator<Item = CellRef>>(&mut self, iter: T) {
        let mut iter = iter.into_iter().peekable();
        if iter.peek().is_none() {
            return;
        }
        let pages = std::sync::Arc::make_mut(&mut self.pages);
        for key in iter {
            let p = key.row / crate::store::PAGE_ROWS;
            let off = (key.row % crate::store::PAGE_ROWS) as usize;
            let mask = 1u64 << (off % 64);
            let entry = pages.entry((p, key.col)).or_default();
            if entry[off / 64] & mask == 0 {
                entry[off / 64] |= mask;
                self.len += 1;
            }
        }
    }
}

impl<'a> Extend<&'a CellRef> for CellJournal {
    fn extend<T: IntoIterator<Item = &'a CellRef>>(&mut self, iter: T) {
        self.extend(iter.into_iter().copied());
    }
}

impl CellJournal {
    #[inline]
    pub fn len(&self) -> usize { self.len }

    #[inline]
    pub fn is_empty(&self) -> bool { self.len == 0 }

    #[inline]
    pub(crate) fn page_masks(&self) -> &HashMap<(u32, u32), [u64; 4]> {
        &self.pages
    }

    #[inline]
    pub fn contains(&self, key: &CellRef) -> bool {
        let Some(bits) = self.pages.get(&(key.row / crate::store::PAGE_ROWS, key.col)) else {
            return false;
        };
        let off = (key.row % crate::store::PAGE_ROWS) as usize;
        (bits[off / 64] & (1u64 << (off % 64))) != 0
    }

    pub fn insert(&mut self, key: CellRef) -> bool {
        if self.contains(&key) {
            return false;
        }
        let p = key.row / crate::store::PAGE_ROWS;
        let off = (key.row % crate::store::PAGE_ROWS) as usize;
        let entry = std::sync::Arc::make_mut(&mut self.pages).entry((p, key.col)).or_default();
        entry[off / 64] |= 1u64 << (off % 64);
        self.len += 1;
        true
    }

    /// Batch-mark an entire rectangle `[r1..=r2, c1..=c2]` using 64-bit word masks.
    pub fn insert_rect(&mut self, r1: u32, c1: u32, r2: u32, c2: u32) {
        if r1 > r2 || c1 > c2 {
            return;
        }
        let first_p = r1 / crate::store::PAGE_ROWS;
        let last_p = r2 / crate::store::PAGE_ROWS;
        let pages = std::sync::Arc::make_mut(&mut self.pages);
        for p in first_p..=last_p {
            let base = p * crate::store::PAGE_ROWS;
            let lo = r1.saturating_sub(base) as usize;
            let hi = (r2 - base).min(crate::store::PAGE_ROWS - 1) as usize;
            let mut mask = [0u64; 4];
            if lo == 0 && hi == 255 {
                mask = [u64::MAX; 4];
            } else {
                for r in lo..=hi {
                    mask[r / 64] |= 1u64 << (r % 64);
                }
            }
            for col in c1..=c2 {
                let entry = pages.entry((p, col)).or_default();
                for w in 0..4 {
                    let added = mask[w] & !entry[w];
                    if added != 0 {
                        entry[w] |= added;
                        self.len += added.count_ones() as usize;
                    }
                }
            }
        }
    }

    pub fn remove(&mut self, key: &CellRef) -> bool {
        if !self.contains(key) {
            return false;
        }
        let page_key = (key.row / crate::store::PAGE_ROWS, key.col);
        let off = (key.row % crate::store::PAGE_ROWS) as usize;
        let pages = std::sync::Arc::make_mut(&mut self.pages);
        if let Some(bits) = pages.get_mut(&page_key) {
            bits[off / 64] &= !(1u64 << (off % 64));
            if *bits == [0; 4] {
                pages.remove(&page_key);
            }
            self.len -= 1;
            return true;
        }
        false
    }

    pub fn clear(&mut self) {
        if let Some(unique) = std::sync::Arc::get_mut(&mut self.pages) {
            unique.clear();
        } else if !self.pages.is_empty() {
            self.pages = Default::default();
        }
        self.len = 0;
    }

    pub(crate) fn remove_if_present(&mut self, key: &CellRef) {
        let _ = self.remove(key);
    }

    /// Bitwise OR a 256-row mask directly into page `(p, col)` without decoding coordinates.
    pub(crate) fn or_page_mask(&mut self, p: u32, col: u32, mask: [u64; 4]) {
        if mask == [0; 4] {
            return;
        }
        let pages = std::sync::Arc::make_mut(&mut self.pages);
        let entry = pages.entry((p, col)).or_default();
        for w in 0..4 {
            let added = mask[w] & !entry[w];
            if added != 0 {
                entry[w] |= added;
                self.len += added.count_ones() as usize;
            }
        }
    }

    /// Bitwise clear a 256-row mask from page `(p, col)` without decoding coordinates.
    pub(crate) fn clear_page_mask(&mut self, p: u32, col: u32, mask: [u64; 4]) {
        if self.len == 0 || mask == [0; 4] || !self.pages.contains_key(&(p, col)) {
            return;
        }
        let pages = std::sync::Arc::make_mut(&mut self.pages);
        if let Some(entry) = pages.get_mut(&(p, col)) {
            for w in 0..4 {
                let removed = entry[w] & mask[w];
                if removed != 0 {
                    entry[w] &= !removed;
                    self.len -= removed.count_ones() as usize;
                }
            }
            if *entry == [0; 4] {
                pages.remove(&(p, col));
            }
        }
    }

    /// Word-level retention across `(page, col)` bitmasks without per-cell decoding.
    pub(crate) fn retain_page_masks(&mut self, mut f: impl FnMut(u32, u32, &mut [u64; 4])) {
        if self.len == 0 {
            return;
        }
        let pages = std::sync::Arc::make_mut(&mut self.pages);
        let mut new_len = 0usize;
        pages.retain(|&(pi, col), bits| {
            f(pi, col, bits);
            let cnt = (bits[0].count_ones()
                + bits[1].count_ones()
                + bits[2].count_ones()
                + bits[3].count_ones()) as usize;
            new_len += cnt;
            cnt > 0
        });
        self.len = new_len;
    }

    pub fn payload_bytes(&self) -> usize {
        self.pages.capacity() * (std::mem::size_of::<(u32, u32)>() + std::mem::size_of::<[u64; 4]>() + 1)
    }

    /// Page-wise bitwise OR union with `other` (zero per-cell decoding).
    pub fn union_from(&mut self, other: &Self) {
        if other.is_empty() {
            return;
        }
        if self.is_empty() {
            *self = other.clone();
            return;
        }
        if std::sync::Arc::ptr_eq(&self.pages, &other.pages) {
            return;
        }
        let pages = std::sync::Arc::make_mut(&mut self.pages);
        for (&key, words) in other.pages.iter() {
            let entry = pages.entry(key).or_default();
            for w in 0..4 {
                let added = words[w] & !entry[w];
                if added != 0 {
                    entry[w] |= added;
                    self.len += added.count_ones() as usize;
                }
            }
        }
    }

    pub fn iter(&self) -> impl Iterator<Item = CellRef> + '_ {
        self.pages.iter().flat_map(|(&(p, col), words)| {
            let base = p * crate::store::PAGE_ROWS;
            words.iter().enumerate().flat_map(move |(w, &bits)| {
                let mut bits = bits;
                std::iter::from_fn(move || {
                    if bits == 0 {
                        return None;
                    }
                    let tz = bits.trailing_zeros();
                    bits &= bits - 1;
                    Some(CellRef::new(base + w as u32 * 64 + tz, col))
                })
            })
        })
    }

    pub fn drain(&mut self) -> impl Iterator<Item = CellRef> {
        let old = std::mem::take(self);
        std::sync::Arc::unwrap_or_clone(old.pages).into_iter().flat_map(|((p, col), words)| {
            let base = p * crate::store::PAGE_ROWS;
            words.into_iter().enumerate().flat_map(move |(w, mut bits)| {
                std::iter::from_fn(move || {
                    if bits == 0 {
                        return None;
                    }
                    let tz = bits.trailing_zeros();
                    bits &= bits - 1;
                    Some(CellRef::new(base + w as u32 * 64 + tz, col))
                })
            })
        })
    }

    pub fn retain(&mut self, mut f: impl FnMut(&CellRef) -> bool) {
        if self.is_empty() {
            return;
        }
        let mut new_len = 0usize;
        std::sync::Arc::make_mut(&mut self.pages).retain(|&(p, col), words| {
            let base = p * crate::store::PAGE_ROWS;
            let mut keep_page = false;
            for (w, word) in words.iter_mut().enumerate() {
                let mut bits = *word;
                let mut kept = 0u64;
                while bits != 0 {
                    let tz = bits.trailing_zeros();
                    let bit = 1u64 << tz;
                    let cref = CellRef::new(base + w as u32 * 64 + tz, col);
                    if f(&cref) {
                        kept |= bit;
                        new_len += 1;
                    }
                    bits &= bits - 1;
                }
                *word = kept;
                keep_page |= kept != 0;
            }
            keep_page
        });
        self.len = new_len;
    }
}

impl<'a> IntoIterator for &'a CellJournal {
    type Item = CellRef;
    type IntoIter = Box<dyn Iterator<Item = CellRef> + 'a>;
    fn into_iter(self) -> Self::IntoIter {
        Box::new(self.iter())
    }
}

impl<'a, K, V> IntoIterator for &'a CellMetadata<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = std::collections::hash_map::Iter<'a, K, V>;
    fn into_iter(self) -> Self::IntoIter { self.0.iter() }
}

#[derive(Clone, Debug)]
pub struct Sheet {
    pub name: String,
    /// Runtime identity survives snapshots, renames and reordering. Save
    /// acknowledgements must never identify a sheet by its current index/name.
    pub identity: u64,
    /// Immutable exact-match indexes shared by calculation snapshots of this sheet.
    pub(crate) shared_lookup: crate::formula::lookup_cache::SharedLookup,
    /// Stable worksheet part in the loaded OOXML package. Renaming/reordering a
    /// sheet must retain its original relationships and other unmodeled content.
    /// A duplicated/new sheet has no source part.
    pub source_part: Option<String>,
    /// The source has implicit row/cell coordinates or out-of-range column
    /// dimensions that require normalization before umya reads a temporary copy.
    pub(crate) source_needs_dom_repair: bool,
    /// 分页列式存储（见 store.rs）：页内按列连续，扫描快、每格只占 40 字节
    cells: Store,
    /// Cell storage revision represented by the current OOXML source part.
    /// Recalculation can change formula caches without marking a user edit.
    pub(crate) saved_cells: Option<crate::store::CellVersion>,
    /// Copy-on-write value snapshot matching a verified worksheet ZIP entry.
    pub(crate) source_cells: Option<crate::xlsx::SourceCells>,
    display_style_revision: u64,
    coordinate_revision: u64,
    /// 被清空过的单元格：保存时要真的从文件里删掉，否则旧值会残留
    pub cleared: CellJournal,
    /// 被动态数组公式溢出占用的单元格（重算前要清掉）
    pub spilled: CellSet<CellRef>,
    /// 动态数组锚点 → 本次重算写回的精确形状 (rows, cols)。1×1 也记录；
    /// 跨表只重算时用它识别可信锚点，不能靠 spilled 邻接尾格猜形状。
    pub spill_shapes: CellMetadata<CellRef, (u32, u32)>,
    /// 老式 CSE 数组公式：锚点 → 覆盖区域（保存时要写回 t="array" ref）
    pub arrays: Vec<(CellRef, (u32, u32, u32, u32))>,
    /// Excel 表格对象（ListObject）
    pub tables: Vec<crate::table::Table>,
    /// 图片（位置 + 部件或内存字节；编辑后由保存写回 drawing）
    pub pictures: Vec<crate::pic::Picture>,
    /// 图片被增删改过，保存时要重写 drawing
    pub pictures_dirty: bool,
    /// 正在从文件加载（加载期间的 set 不算编辑）
    pub loading: bool,
    /// 这张表被改过没有：没改过就整段沿用原文件的 XML，别让 umya 重写
    pub edited: bool,
    /// 格式动过没有（动过就不能整段沿用基准的样式表）
    pub styles_dirty: bool,
    /// 合并单元格区域 (r1, c1, r2, c2)（CoW 共享 + 单调版本号 + 内置二分索引）
    pub merges: crate::merges::Merges,
    /// 列宽（只有被设置过的列才有条目）
    pub cols: crate::layout::Axis,
    /// 行高
    pub rows: crate::layout::Axis,
    /// 有样式的单元格 → 样式表索引（只存有样式的格子，稀疏）
    pub styles: crate::store::CellStyles,
    /// 整列默认样式（列号 -> 样式表索引，稀疏）
    pub col_styles: CellMetadata<u32, u16>,
    /// 整行默认样式（行号 -> 样式表索引，稀疏）
    pub row_styles: CellMetadata<u32, u16>,
    /// 用户改过样式的格子（保存时只需要回写这些，不必重写整表样式）
    pub style_edits: CellJournal,
    /// 整列样式被改过
    pub col_styles_dirty: bool,
    /// 整行样式被改过
    pub row_styles_dirty: bool,
    /// 合并区域被改过，保存时要同步
    pub merges_dirty: bool,
    /// 条件格式
    pub cf: crate::cf::CfSheet,
    /// 条件格式被改过，保存时要同步
    pub cf_dirty: bool,
    /// 数据验证
    pub dv: Vec<crate::dv::DataValidation>,
    /// 数据验证被改过
    pub dv_dirty: bool,
    /// 批注
    pub notes: CellMetadata<CellRef, crate::notes::Note>,
    /// 批注被改过
    pub notes_dirty: bool,
    /// 冻结窗格：(冻结行数, 冻结列数)
    pub freeze: Option<(u32, u32)>,
    /// 冻结设置被改过
    pub freeze_dirty: bool,
    /// 自动筛选
    pub filter: Option<crate::sort_filter::Filter>,
    /// 被筛选隐藏的行（渲染时按高度 0 处理）
    pub hidden_rows: crate::layout::HiddenIndices,
    /// 用户手动隐藏的行（和筛选隐藏分开，筛选重算不会清掉）
    pub hidden_rows_user: crate::layout::HiddenIndices,
    /// 用户手动隐藏的列
    pub hidden_cols: crate::layout::HiddenIndices,
    /// 列宽/行高/隐藏状态被改过：保存时要重写 <cols> 和行属性
    pub layout_dirty: bool,
    /// 方案管理器里的方案（写进 <scenarios>）
    pub scenarios: Vec<crate::scenarios::Scenario>,
    pub scenario_settings: crate::scenarios::ScenarioSettings,
    /// 方案列表被改过
    pub scenarios_dirty: bool,
    /// 被用户「忽略错误」的单元格（写进 <ignoredErrors>）
    pub ignored_errors: CellSet<(u32, u32)>,
    /// 忽略列表被改过
    pub ignored_errors_dirty: bool,
    /// 分级显示设置：明细在下方 / 在右侧 / 显示分级符号
    pub outline_summary_below: bool,
    pub outline_summary_right: bool,
    pub outline_symbols: bool,
    /// 分级设置被改过
    pub outline_pr_dirty: bool,
    /// 分级显示（组合）级别：行/列 → 1..=7；不在表里表示不在任何分级里
    pub row_levels: std::collections::HashMap<u32, u8>,
    pub col_levels: std::collections::HashMap<u32, u8>,
    /// 筛选状态被改过
    pub filter_dirty: bool,
    /// 页面设置
    pub page: crate::page::PageSetup,
    /// 页面设置被改过
    pub page_dirty: bool,
    /// 图表（CoW 共享避免渲染与快照每帧深拷）
    pub charts: SharedList<crate::charts::Chart>,
    /// 图表增删改/锚点/引用被改过：保存时同步写回 chart/drawing 部件
    pub charts_dirty: bool,
    /// 在内存中删除、待保存时从 zip 摘除的 `(drawing_part, chart_part)` 列表
    pub deleted_charts: Vec<(String, String)>,
    /// 工作表标签颜色（RGB）
    pub tab_color: Option<[u8; 3]>,
    /// 标签颜色被改过：保存时要写 <sheetPr><tabColor>
    pub tab_color_dirty: bool,
    /// 表格对象被改过（新建/删除/转换）：保存时要同步 table 部件
    pub tables_dirty: bool,
    /// 工作表保护（None = 未保护）
    pub protection: Option<crate::protection::SheetProtection>,
    /// 保护状态被改过
    pub protect_dirty: bool,
    /// 单元格富文本（有记录时优先于纯文本绘制）
    pub rich: CellMetadata<(u32, u32), Vec<crate::richtext::RichRun>>,
    /// 富文本被改过
    pub rich_dirty: bool,
    /// 单元格超链接
    pub hyperlinks: CellMetadata<(u32, u32), crate::hyperlinks::Hyperlink>,
    pub hyperlinks_dirty: bool,
    /// 单元格迷你图（Excel 的 x14 sparklineGroups）
    pub sparklines: Vec<crate::sparkline::Sparkline>,
    /// 迷你图被改过：保存时要重写工作表 extLst
    pub sparklines_dirty: bool,
    /// 文本框（drawing 里的 xdr:sp）
    pub textboxes: Vec<crate::textbox::TextBox>,
    /// 文本框被改过：保存时重写 drawing 部件
    pub textboxes_dirty: bool,
    /// 工作表被隐藏（workbook.xml 的 `state="hidden"`）
    pub hidden: bool,
    pub hidden_dirty: bool,
}

impl Sheet {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            identity: next_sheet_identity(),
            shared_lookup: Default::default(),
            source_part: None,
            source_needs_dom_repair: false,
            cells: Store::default(),
            saved_cells: None,
            source_cells: None,
            display_style_revision: 0,
            coordinate_revision: 0,
            cleared: CellJournal::default(),
            spilled: CellSet::default(),
            spill_shapes: CellMetadata::default(),
            arrays: Vec::new(),
            tables: Vec::new(),
            pictures: Vec::new(),
            pictures_dirty: false,
            loading: false,
            edited: false,
            styles_dirty: false,
            merges: crate::merges::Merges::default(),
            cols: crate::layout::Axis::new(crate::layout::DEFAULT_COL_W),
            rows: crate::layout::Axis::new(crate::layout::DEFAULT_ROW_H),
            styles: crate::store::CellStyles::default(),
            col_styles: Default::default(),
            row_styles: Default::default(),
            style_edits: CellJournal::default(),
            col_styles_dirty: false,
            row_styles_dirty: false,
            merges_dirty: false,
            cf: crate::cf::CfSheet::default(),
            cf_dirty: false,
            dv: Vec::new(),
            dv_dirty: false,
            notes: Default::default(),
            notes_dirty: false,
            freeze: None,
            freeze_dirty: false,
            filter: None,
            hidden_rows: Default::default(),
            hidden_rows_user: Default::default(),
            hidden_cols: Default::default(),
            layout_dirty: false,
            scenarios: Vec::new(),
            scenario_settings: Default::default(),
            scenarios_dirty: false,
            ignored_errors: CellSet::default(),
            ignored_errors_dirty: false,
            outline_summary_below: true,
            outline_summary_right: true,
            outline_symbols: true,
            outline_pr_dirty: false,
            row_levels: std::collections::HashMap::new(),
            col_levels: std::collections::HashMap::new(),
            filter_dirty: false,
            page: crate::page::PageSetup::default(),
            page_dirty: false,
            charts: SharedList::default(),
            charts_dirty: false,
            deleted_charts: Vec::new(),
            tab_color: None,
            tab_color_dirty: false,
            tables_dirty: false,
            protection: None,
            protect_dirty: false,
            rich: Default::default(),
            rich_dirty: false,
            hyperlinks: Default::default(),
            hyperlinks_dirty: false,
            sparklines: Vec::new(),
            sparklines_dirty: false,
            textboxes: Vec::new(),
            textboxes_dirty: false,
            hidden: false,
            hidden_dirty: false,
        }
    }

    /// A copy inserted as a new sheet must not inherit the original identity.
    pub fn renew_identity(&mut self) {
        self.identity = next_sheet_identity();
        self.source_part = None;
        self.source_needs_dom_repair = false;
        self.saved_cells = None;
        self.source_cells = None;
    }

    #[inline]
    pub fn get(&self, r: u32, c: u32) -> Option<Cow<'_, Cell>> {
        self.cells.get(r, c)
    }

    #[inline]
    pub fn get_view(&self, r: u32, c: u32) -> Option<CellView<'_>> {
        self.cells.get_view(r, c)
    }

    #[inline]
    pub fn get_f64(&self, r: u32, c: u32) -> Option<f64> {
        self.cells.get_f64(r, c)
    }

    #[inline]
    #[allow(dead_code)]
    pub(crate) fn get_f64_plain(&self, r: u32, c: u32) -> Option<f64> {
        self.cells.get_f64_plain(r, c)
    }

    #[inline]
    pub fn formula_of(&self, r: u32, c: u32) -> Option<&str> {
        self.cells.formula_of(r, c)
    }

    #[inline]
    pub(crate) fn formula_arc_of(&self, r: u32, c: u32) -> Option<&std::sync::Arc<str>> {
        self.cells.formula_arc_of(r, c)
    }

    pub fn get_mut(&mut self, r: u32, c: u32) -> Option<&mut Cell> {
        self.cells.get_mut(r, c)
    }

    /// Update the formula of an existing cell without promoting sparse slabs via `get_mut`.
    pub(crate) fn set_formula_in_place(&mut self, r: u32, c: u32, formula: std::sync::Arc<str>) -> bool {
        let changed = self.cells.set_formula_in_place(r, c, formula);
        if changed && !self.loading {
            self.edited = true;
        }
        changed
    }

    /// Strip formulas (keeping cached values and auto-demoting `Mixed` -> `Numbers`/`Values` slabs)
    /// across rectangle `[r1..=r2, c1..=c2]`. Returns count of cells whose formula was cleared.
    #[allow(dead_code)]
    pub(crate) fn strip_formulas_in_rect(&mut self, r1: u32, c1: u32, r2: u32, c2: u32) -> usize {
        let cleared = self.cells.strip_formulas_in_rect(r1, c1, r2, c2, &mut self.cleared);
        if cleared > 0 && !self.loading {
            self.edited = true;
        }
        cleared
    }

    pub(crate) fn foreign_formula_cells<'a>(&'a self, sheet_name: &'a str) -> impl Iterator<Item = (CellRef, &'a Cell)> {
        let quoted = format!("'{}'", sheet_name.replace('\'', "''"));
        self.cells.foreign_formula_cells().filter(move |(_, cell)| {
            // Every qualified local reference accepted by the rewriter ends in
            // one of these spellings before '!'. It handles final token boundaries,
            // strings and external books itself, so a false positive stays harmless.
            let formula = cell.formula.as_deref().unwrap();
            formula.match_indices('!').any(|(end, _)| {
                [sheet_name.as_bytes(), quoted.as_bytes()].iter().any(|name| {
                    end >= name.len() && formula.as_bytes()[end-name.len()..end].eq_ignore_ascii_case(name)
                })
            })
        })
    }

    pub fn has_formulas(&self) -> bool { self.cells.has_formulas() }

    pub fn formula_count(&self) -> usize { self.cells.formula_count() }

    #[inline]
    pub fn value_ref(&self, r: u32, c: u32) -> Cow<'_, Value> {
        self.cells.value_ref(r, c)
    }

    #[inline]
    pub fn value(&self, r: u32, c: u32) -> Value {
        self.cells.value(r, c)
    }

    pub fn contains(&self, r: u32, c: u32) -> bool {
        self.cells.contains(r, c)
    }

    pub fn set(&mut self, r: u32, c: u32, cell: Cell) {
        if !self.loading {
            self.edited = true;
        }
        let key = CellRef::new(r, c);
        // 往溢出区里写东西 = 用户覆盖：从这个集合里摘掉，重算时才会判成 #SPILL!，
        // 而不是被 clear_spills 当成旧溢出值删掉（那会静默吞掉用户输入）。
        // 溢出写回自己是先 set 再 insert，顺序不受影响。
        self.spilled.remove_if_present(&key);
        if cell.is_blank() {
            if self.cells.remove(r, c).is_some() {
                self.cleared.insert(key);
            }
            return;
        }
        self.cleared.remove_if_present(&key);
        self.cells.set(r, c, cell);
    }

    pub fn remove(&mut self, r: u32, c: u32) -> Option<Cell> {
        if !self.loading {
            self.edited = true;
        }
        let key = CellRef::new(r, c);
        let out = self.cells.remove(r, c);
        // 删掉的格子要登记，否则保存时会把基准文件里的旧值又写回去
        // （拖动移动、剪切这类「先删后写」的操作全靠这个）
        if out.is_some() {
            self.cleared.insert(key);
        }
        out
    }

    pub fn len(&self) -> usize {
        self.cells.len()
    }

    /// 诊断用：转发到 Store::stats（页数 / 列基调槽数 / 槽位里的 Cell 数）。
    pub fn cells_stats(&self) -> (usize, usize, usize) {
        self.cells.stats()
    }

    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    pub fn clear(&mut self) {
        self.cells.clear();
    }

    /// 按行、列升序遍历所有单元格
    pub(crate) fn cells(&self) -> &Store { &self.cells }

    pub(crate) fn replace_cell_store(&mut self, cells: Store) { self.cells = cells; }

    pub fn formula_cells(&self) -> impl Iterator<Item = (CellRef, &Cell)> {
        self.cells.formula_cells()
    }

    pub(crate) fn remap_cell_axis(&mut self, rows: bool, at: u32, map: impl Fn(u32) -> Option<u32> + Copy) {
        self.cells.remap_axis(rows, at, map, &mut self.cleared);
        let old_styles = self.styles.clone();
        self.styles.remap_axis(rows, at, map);
        old_styles.diff_into_journal(&self.styles, &mut self.style_edits);
        // Removing an old cell on disk also removes its format. Reapply any
        // explicitly styled blank which now occupies that same position.
        self.styles.intersect_journal_into(&self.cleared, &mut self.style_edits);
        if !self.style_edits.is_empty() {
            self.styles_dirty = true;
            self.display_style_revision = next_sheet_identity();
        }
        self.edited = true;
    }

    pub(crate) fn permute_cell_band(&mut self, rows: bool, lo: u32, hi: u32, map: impl Fn(u32) -> u32 + Copy) {
        if lo >= hi {
            return;
        }
        self.cells.permute_band(rows, lo, hi, map, &mut self.cleared);
        let old_styles = self.styles.clone();
        self.styles.permute_band(rows, lo, hi, map);
        old_styles.diff_into_journal(&self.styles, &mut self.style_edits);
        self.styles.intersect_journal_into(&self.cleared, &mut self.style_edits);
        if !self.style_edits.is_empty() {
            self.styles_dirty = true;
            self.display_style_revision = next_sheet_identity();
        }
        self.edited = true;
    }

    pub(crate) fn changed_cells(&self, other: &Sheet) -> impl Iterator<Item = CellRef> {
        self.cells.changed_keys(&other.cells)
    }

    pub fn cell_revision(&self) -> u64 { self.cells.revision() }

    pub fn cell_version(&self) -> &crate::store::CellVersion { &self.cells.version }

    pub fn same_cells_in(&self, prior: &Self, rects: &[(u32,u32,u32,u32)], formulas: bool) -> bool {
        self.cells.same_cells_in(&prior.cells, rects, formulas)
    }

    pub fn same_display_styles_in(&self, prior: &Self, rects: &[(u32,u32,u32,u32)],
        current_styles: &crate::style::StyleTable, prior_styles: &crate::style::StyleTable) -> bool {
        let equal = |a, b| current_styles.get(a).format == prior_styles.get(b).format;
        let rows = self.row_styles.keys().chain(prior.row_styles.keys())
            .filter(|&&r| rects.is_empty() || rects.iter().any(|&(r1,_,r2,_)| r1 <= r && r <= r2));
        let cols = self.col_styles.keys().chain(prior.col_styles.keys())
            .filter(|&&c| rects.is_empty() || rects.iter().any(|&(_,c1,_,c2)| c1 <= c && c <= c2));
        let same_override = |a: Option<&u16>, b: Option<&u16>| match (a,b) {
            (Some(&a),Some(&b)) => equal(a,b),
            (None,None) => true,
            _ => false, // An added row override may mask a column's format.
        };
        rows.into_iter().all(|r| same_override(self.row_styles.get(r), prior.row_styles.get(r)))
            && cols.into_iter().all(|c| same_override(self.col_styles.get(c), prior.col_styles.get(c)))
            && self.styles.changed_keys(&prior.styles).chain(prior.styles.changed_keys(&self.styles)).all(|k| {
                (!rects.is_empty() && !rects.iter().any(|&(r1,c1,r2,c2)| r1 <= k.row && k.row <= r2 && c1 <= k.col && k.col <= c2))
                    || equal(self.style_of(k.row,k.col), prior.style_of(k.row,k.col))
            })
    }

    pub(crate) fn cell_data_needs_save(&self) -> bool {
        self.edited || !self.cleared.is_empty() || match &self.saved_cells {
            Some(saved) => saved != self.cell_version(),
            None => !self.spilled.is_empty(),
        }
    }

    pub(crate) fn has_pending_sheet_metadata(&self) -> bool {
        self.styles_dirty || self.row_styles_dirty || self.col_styles_dirty || !self.style_edits.is_empty()
            || self.pictures_dirty || self.merges_dirty || self.cf_dirty || self.dv_dirty
            || self.notes_dirty || self.freeze_dirty || self.layout_dirty || self.scenarios_dirty
            || self.ignored_errors_dirty || self.outline_pr_dirty || self.filter_dirty || self.page_dirty
            || self.charts_dirty || self.tab_color_dirty || self.tables_dirty || self.protect_dirty
            || self.rich_dirty || self.hyperlinks_dirty || self.sparklines_dirty || self.textboxes_dirty
            || self.hidden_dirty
    }

    pub fn set_formula_cache(&mut self, r: u32, c: u32, value: Value) {
        if self.get_view(r, c).is_some_and(|cell| cell.value().as_ref() != &value) {
            self.cells.set_formula_cache(r, c, value);
        }
    }

    pub fn display_style_revision(&self) -> u64 { self.display_style_revision }

    pub fn coordinate_revision(&self) -> u64 { self.coordinate_revision }

    pub(crate) fn coordinates_shifted(&mut self) {
        self.coordinate_revision = next_sheet_identity();
        self.display_style_revision = self.coordinate_revision;
    }

    pub(crate) fn stored_cells(&self) -> impl Iterator<Item = (CellRef, CellView<'_>)> {
        self.cells.stored_cells()
    }

    /// Retain only the data and number formats needed for background text search.
    pub fn search_snapshot(&self) -> Self {
        let mut snapshot = Self::new(&self.name);
        snapshot.cells = self.cells.clone();
        snapshot.identity = self.identity;
        snapshot.coordinate_revision = self.coordinate_revision;
        snapshot.display_style_revision = self.display_style_revision;
        snapshot.styles = self.styles.clone();
        snapshot.row_styles = self.row_styles.clone();
        snapshot.col_styles = self.col_styles.clone();
        snapshot
    }

    /// Inputs used by selection statistics. Cell contents share their COW column
    /// storage with the live sheet, and unrelated workbook metadata stays there.
    pub fn selection_stats_snapshot(&self) -> Self {
        self.selection_stats_snapshot_with_filter(self.hidden_rows.is_empty())
    }

    pub fn selection_stats_snapshot_with_filter(&self, with_filter: bool) -> Self {
        let mut snapshot = Self::new(&self.name);
        snapshot.cells = self.cells.clone();
        snapshot.rows = self.rows.clone();
        snapshot.hidden_rows = self.hidden_rows.clone();
        snapshot.hidden_rows_user = self.hidden_rows_user.clone();
        if with_filter {
            snapshot.filter = self.filter.clone();
        }
        snapshot
    }

    /// Only copy the fields read by the row/column reorder planner. The
    /// worker journals its own changes separately from concurrent save receipts.
    pub fn reorder_snapshot(&self) -> Self {
        let mut snapshot = self.search_snapshot();
        snapshot.identity = self.identity;
        snapshot.merges = self.merges.clone();
        snapshot.arrays = self.arrays.clone();
        snapshot.spilled = self.spilled.clone();
        snapshot
    }

    pub fn matches_reorder_source(&self, source: &Self) -> bool {
        self.identity == source.identity && self.cell_version() == source.cell_version()
            && self.name == source.name && self.styles == source.styles
            && self.row_styles == source.row_styles && self.col_styles == source.col_styles
            && self.merges == source.merges && self.arrays == source.arrays && self.spilled == source.spilled
    }

    /// CSV writes cells, while a sort also depends on cached values and formats.
    /// Changed storage slabs can be shared with the worker even on very large sheets.
    pub fn csv_inputs_match(&self, before: &Self, keys: &[(u32, u32)]) -> bool {
        let hit = |k: CellRef| keys.binary_search(&(k.row, k.col)).is_ok();
        let intersects = |&(r1,c1,r2,c2): &(u32,u32,u32,u32)| {
            keys[keys.partition_point(|&(r,_)| r < r1)..].iter()
                .take_while(|&&(r,_)| r <= r2).any(|&(_,c)| (c1..=c2).contains(&c))
        };
        self.merges.iter().filter(|r| intersects(r)).eq(before.merges.iter().filter(|r| intersects(r)))
            && self.arrays.iter().filter(|(_,r)| intersects(r)).eq(before.arrays.iter().filter(|(_,r)| intersects(r)))
            && !self.spilled.symmetric_difference(&before.spilled).any(|k| hit(*k))
            && (self.cell_version() == before.cell_version() ||
                self.changed_cells(before).chain(before.changed_cells(self)).filter(|k| hit(*k)).all(|k| {
                    self.get_view(k.row,k.col).zip(before.get_view(k.row,k.col)).is_some_and(|(a,b)| a.same_input(&b))
                }))
    }

    /// Rectangle-based CSV preflight validation: O(merges + arrays + spilled + changed_pages)
    /// without materializing millions of `(row, col)` coordinates.
    pub fn csv_inputs_match_rects(&self, before: &Self, rects: &[(u32, u32, u32, u32)]) -> bool {
        let hit = |k: CellRef| {
            rects
                .iter()
                .any(|&(r1, c1, r2, c2)| (r1..=r2).contains(&k.row) && (c1..=c2).contains(&k.col))
        };
        let intersects = |&(a, b, c, d): &(u32, u32, u32, u32)| {
            rects
                .iter()
                .any(|&(r1, c1, r2, c2)| a <= r2 && c >= r1 && b <= c2 && d >= c1)
        };
        self.merges
            .iter()
            .filter(|r| intersects(r))
            .eq(before.merges.iter().filter(|r| intersects(r)))
            && self
                .arrays
                .iter()
                .filter(|(_, r)| intersects(r))
                .eq(before.arrays.iter().filter(|(_, r)| intersects(r)))
            && !self.spilled.symmetric_difference(&before.spilled).any(|k| hit(*k))
            && (self.cell_version() == before.cell_version()
                || self
                    .changed_cells(before)
                    .chain(before.changed_cells(self))
                    .filter(|k| hit(*k))
                    .all(|k| {
                        self.get_view(k.row, k.col)
                            .zip(before.get_view(k.row, k.col))
                            .is_some_and(|(a, b)| a.same_input(&b))
                    }))
    }

    pub fn same_sort_region(&self, before: &Self, rect: (u32,u32,u32,u32)) -> bool {
        let (r1,c1,r2,c2) = rect;
        let inside = |k: CellRef| (r1..=r2).contains(&k.row) && (c1..=c2).contains(&k.col);
        let intersects = |&(a,b,c,d): &(u32,u32,u32,u32)| a <= r2 && c >= r1 && b <= c2 && d >= c1;
        let axis_matches = |live: &CellMetadata<u32,u16>, old: &CellMetadata<u32,u16>, first: u32, last: u32| {
            let range = first..=last;
            live.keys().chain(old.keys()).filter(|&k| range.contains(k))
                .all(|k| live.get(k) == old.get(k))
        };
        self.same_cells_in(before, &[rect], true) && self.same_cells_in(before, &[rect], false)
            && self.merges.iter().filter(|r| intersects(r)).eq(before.merges.iter().filter(|r| intersects(r)))
            && self.arrays.iter().filter(|(_,r)| intersects(r)).eq(before.arrays.iter().filter(|(_,r)| intersects(r)))
            && !self.spilled.symmetric_difference(&before.spilled).any(|k| inside(*k))
            && axis_matches(&self.row_styles, &before.row_styles, r1, r2)
            && axis_matches(&self.col_styles, &before.col_styles, c1, c2)
            && !self.styles.changed_keys(&before.styles).chain(before.styles.changed_keys(&self.styles)).any(inside)
    }

    /// Carry concurrent edits outside a validated operation into its prepared COW pages.
    /// Live save journals already own those edits. Only the worker's writes need new entries.
    pub fn rebase_row_changes(&self, before: &Sheet, prepared: &mut Sheet, in_target: impl Fn(CellRef) -> bool) {
        if self.cell_version() != before.cell_version() {
            for k in self.changed_cells(before).filter(|k| !in_target(*k)) {
                if let Some(cell) = self.get_view(k.row, k.col) {
                    prepared.cells.set(k.row, k.col, cell.to_cell());
                }
            }
            for k in before.changed_cells(self).filter(|k| !in_target(*k) && !self.contains(k.row, k.col)) {
                prepared.cells.remove(k.row,k.col);
            }
        }
        for k in self.spilled.symmetric_difference(&before.spilled).filter(|k| !in_target(**k)) {
            if self.spilled.contains(k) { prepared.spilled.insert(*k); }
            else { prepared.spilled.remove_if_present(k); }
        }
        if prepared.styles_dirty {
            for k in self.styles.changed_keys(&before.styles) {
                if let Some(&id) = self.styles.get(&k) { prepared.styles.insert(k,id); }
            }
            for k in before.styles.changed_keys(&self.styles).filter(|k| self.styles.get(k).is_none()) {
                prepared.styles.remove(&k);
            }
        }
    }

    /// Merge concurrent edits into staged replacement pages. Changed target values
    /// survive and are excluded from the undo/broadcast transaction. A whole-row
    /// format change or a large intervening edit is cheaper to restage off-thread.
    pub fn rebase_replacement(&self, before: &Sheet, result: &mut Sheet,
        targets: &[(u32, u32)], value_mode: bool, live_styles: &crate::style::StyleTable,
        old_styles: &crate::style::StyleTable) -> Option<Vec<(u32,u32)>> {
        let hit = |k: CellRef| targets.binary_search(&(k.row, k.col)).is_ok();
        let mut conflicts = Vec::new();
        if value_mode && self.display_style_revision != before.display_style_revision {
            if self.row_styles != before.row_styles || self.col_styles != before.col_styles { return None; }
            for k in self.styles.changed_keys(&before.styles).chain(before.styles.changed_keys(&self.styles)) {
                if hit(k) && live_styles.get(self.style_of(k.row,k.col)).format != old_styles.get(before.style_of(k.row,k.col)).format {
                    conflicts.push((k.row,k.col));
                    result.set(k.row,k.col,self.get(k.row,k.col).map(Cow::into_owned).unwrap_or_default());
                }
            }
        }
        if self.cell_version() != before.cell_version() {
            let changes = self.changed_cells(before).chain(before.changed_cells(self)
                .filter(|k| !self.contains(k.row, k.col)));
            for (n, k) in changes.enumerate() {
                if n >= 4096 { return None; }
                if hit(k) {
                    // A changed formula cache is still the same formula-mode hit. Restage
                    // to capture its new cached value in both sides of the undo record.
                    if !value_mode && self.formula_of(k.row, k.col) == before.formula_of(k.row, k.col) {
                        return None;
                    }
                    conflicts.push((k.row,k.col));
                }
                result.set(k.row, k.col, self.get(k.row, k.col).map(Cow::into_owned).unwrap_or_default());
            }
        }
        conflicts.sort_unstable();
        conflicts.dedup();
        result.spilled = self.spilled.iter().copied().filter(|k|
            !hit(*k) || conflicts.binary_search(&(k.row,k.col)).is_ok()).collect();
        Some(conflicts)
    }

    /// Install a validated reorder and merge just the worker's new edits into
    /// the live save journals, which may have been acknowledged in the meantime.
    pub fn take_row_changes(&mut self, result: &mut Self) {
        std::mem::swap(&mut self.cells, &mut result.cells);
        std::mem::swap(&mut self.spilled, &mut result.spilled);
        self.cleared.retain(|key| !self.cells.contains(key.row, key.col));
        if self.cleared.is_empty() {
            self.cleared = std::mem::take(&mut result.cleared);
        } else {
            self.cleared.union_from(&result.cleared);
            result.cleared.clear();
        }
        if result.styles_dirty {
            std::mem::swap(&mut self.styles, &mut result.styles);
            self.styles_dirty = true;
            self.display_style_revision = next_sheet_identity();
            if self.style_edits.is_empty() {
                self.style_edits = std::mem::take(&mut result.style_edits);
            } else {
                self.style_edits.union_from(&result.style_edits);
                result.style_edits.clear();
            }
        }
        self.edited = true;
    }

    pub(crate) fn cf_pages(&self, rect: (u32, u32, u32, u32)) -> Vec<crate::cf::PageSlice> {
        self.cells.cf_pages(rect)
    }

    pub(crate) fn column_pages(&self, col: u32) -> Vec<(u32, std::sync::Arc<ColSlab>)> {
        self.cells.column_pages(col)
    }

    /// Install only the fields written by formula evaluation. UI metadata and
    /// save receipts belong to the live sheet even if calculation was in flight.
    pub fn take_calculation(&mut self, result: &mut Sheet) {
        if self.cell_version() != result.cell_version() {
            // Evaluation can remove old spill cells. Their deletion journal
            // belongs to the new cell version. For an unchanged version, the
            // live journal may already have been acknowledged by a save.
            std::mem::swap(&mut self.cells, &mut result.cells);
            std::mem::swap(&mut self.cleared, &mut result.cleared);
        }
        std::mem::swap(&mut self.spilled, &mut result.spilled);
        std::mem::swap(&mut self.spill_shapes, &mut result.spill_shapes);
    }

    pub fn iter(&self) -> impl Iterator<Item = (CellRef, Cow<'_, Cell>)> {
        self.cells.iter()
    }

    /// Occupied cells in a rectangle, ordered by row and then column.
    pub fn iter_rect(&self, rect: (u32, u32, u32, u32)) -> impl Iterator<Item = (CellRef, Cow<'_, Cell>)> {
        self.cells.iter_rect(rect)
    }

    pub fn iter_views(&self) -> impl Iterator<Item = (CellRef, CellView<'_>)> {
        self.cells.iter_views()
    }

    pub fn iter_rect_views(&self, rect: (u32, u32, u32, u32)) -> impl Iterator<Item = (CellRef, CellView<'_>)> {
        self.cells.iter_rect_views(rect)
    }

    pub fn values(&self) -> impl Iterator<Item = Cow<'_, Cell>> {
        self.cells.iter_values()
    }

    /// 抢救副本没有基准：把「已加载但没改过」的内容全部标脏，
    /// 让布局/保护/页面设置/批注/图表等补丁把它们重新写出来。
    pub fn mark_all_dirty(&mut self) {
        self.styles_dirty = true;
        self.col_styles_dirty = true;
        self.row_styles_dirty = true;
        self.merges_dirty = true;
        self.cf_dirty = true;
        self.dv_dirty = true;
        self.notes_dirty = true;
        self.freeze_dirty = true;
        self.layout_dirty = true;
        self.scenarios_dirty = true;
        self.ignored_errors_dirty = true;
        self.outline_pr_dirty = true;
        self.filter_dirty = true;
        self.page_dirty = true;
        self.charts_dirty = true;
        self.tab_color_dirty = true;
        self.tables_dirty = true;
        self.protect_dirty = true;
        self.hidden_dirty = true;
    }

    #[inline]
    pub fn style_of(&self, r: u32, c: u32) -> u16 {
        if let Some(&id) = self.styles.get(&CellRef::new(r, c)) {
            return id;
        }
        if !self.row_styles.is_empty()
            && let Some(&id) = self.row_styles.get(&r)
        {
            return id;
        }
        if !self.col_styles.is_empty()
            && let Some(&id) = self.col_styles.get(&c)
        {
            return id;
        }
        0
    }

    /// 显式写在单元格上的样式索引（不回退到整行/整列）
    pub fn explicit_style_of(&self, r: u32, c: u32) -> Option<u16> {
        self.styles.get(&CellRef::new(r, c)).copied()
    }

    /// An existing OOXML value cell with no s uses cellXfs[0]. An absent cell
    /// at the same address would inherit the row or column style on entry.
    pub(crate) fn apply_loaded_xf_zero(&mut self, xf_zero: u16) {
        for (key, _) in self.cells.iter() {
            if (self.row_styles.contains_key(&key.row) || self.col_styles.contains_key(&key.col))
                && self.styles.get(&key).is_none()
            {
                self.styles.insert(key, xf_zero);
            }
        }
    }

    /// 用户改样式（标记脏，保存时回写）
    pub fn set_style(&mut self, r: u32, c: u32, id: u16) {
        let explicit = (id != 0 || self.row_styles.contains_key(&r) || self.col_styles.contains_key(&c)).then_some(id);
        if self.explicit_style_of(r, c) == explicit { return; }
        self.display_style_revision = next_sheet_identity();
        if !self.loading {
            self.edited = true;
            self.styles_dirty = true;
        }
        let key = CellRef::new(r, c);
        if let Some(id) = explicit {
            self.styles.insert(key, id);
        } else {
            self.styles.remove(&key);
        }
        self.style_edits.insert(key);
    }

    /// 从文件载入样式（不算用户修改）
    pub fn set_style_loaded(&mut self, r: u32, c: u32, id: u16) {
        self.display_style_revision = next_sheet_identity();
        let key = CellRef::new(r, c);
        if id == 0 {
            if self.row_styles.contains_key(&r) || self.col_styles.contains_key(&c) {
                self.styles.insert(key, 0);
            } else {
                self.styles.remove(&key);
            }
        } else {
            self.styles.insert(key, id);
        }
    }

    /// 用户设置整列样式（稀疏）
    pub fn set_col_style(&mut self, c: u32, id: u16) {
        self.display_style_revision = next_sheet_identity();
        if !self.loading {
            self.edited = true;
            self.styles_dirty = true;
            self.col_styles_dirty = true;
        }
        if id == 0 {
            if self.col_styles.contains_key(&c) {
                self.col_styles.remove(&c);
            }
        } else {
            self.col_styles.insert(c, id);
        }
    }

    /// 从文件载入整列样式（不算用户修改）
    pub fn set_col_style_loaded(&mut self, c: u32, id: u16) {
        self.display_style_revision = next_sheet_identity();
        if id == 0 {
            if self.col_styles.contains_key(&c) {
                self.col_styles.remove(&c);
            }
        } else {
            self.col_styles.insert(c, id);
        }
    }

    /// 用户设置整行样式（稀疏）
    pub fn set_row_style(&mut self, r: u32, id: u16) {
        self.display_style_revision = next_sheet_identity();
        if !self.loading {
            self.edited = true;
            self.styles_dirty = true;
            self.row_styles_dirty = true;
        }
        if id == 0 {
            if self.row_styles.contains_key(&r) {
                self.row_styles.remove(&r);
            }
        } else {
            self.row_styles.insert(r, id);
        }
    }

    /// 从文件载入整行样式（不算用户修改）
    pub fn set_row_style_loaded(&mut self, r: u32, id: u16) {
        self.display_style_revision = next_sheet_identity();
        if id == 0 {
            if self.row_styles.contains_key(&r) {
                self.row_styles.remove(&r);
            }
        } else {
            self.row_styles.insert(r, id);
        }
    }

    /// 合并一个矩形区域（与区域内已有合并冲突的先拆掉）
    pub fn merge_range(&mut self, r1: u32, c1: u32, r2: u32, c2: u32) {
        if self.merges.merge_range(r1, c1, r2, c2) {
            self.merges_dirty = true;
        }
    }

    /// 批量合并多个矩形区域（一次去冲突与排序，避免多次 O(M log M) 重排）
    pub fn merge_ranges(&mut self, rects: impl IntoIterator<Item = (u32, u32, u32, u32)>) {
        if self.merges.merge_ranges(rects) {
            self.merges_dirty = true;
        }
    }

    /// 拆掉与给定区域相交的所有合并
    pub fn unmerge_range(&mut self, r1: u32, c1: u32, r2: u32, c2: u32) {
        if self.merges.unmerge_range(r1, c1, r2, c2) {
            self.merges_dirty = true;
        }
    }

    /// 命中合并区域时返回该区域（含锚点）；基于惰性区间索引，查询复杂度 O(log M)
    pub fn merge_at(&self, r: u32, c: u32) -> Option<(u32, u32, u32, u32)> {
        self.merges.at(r, c)
    }

    #[inline]
    pub fn merges_revision(&self) -> u64 {
        self.merges.revision()
    }

    #[inline]
    pub fn max_merge_row(&self) -> Option<u32> {
        self.merges.max_row()
    }

    #[inline]
    pub fn max_merge_col(&self) -> Option<u32> {
        self.merges.max_col()
    }

    /// 批量写入矩形区域样式：整页全覆盖时自动压缩为 `Uniform(id)` 共享页；
    /// `id == 0` 走页级 `remove_rect_recording`，仅对命中整行/整列样式的行列写回显式 `0`，
    /// 避免全选清除样式时遍历 1.7×10¹⁰ 个空坐标。
    pub fn set_style_rect(&mut self, r1: u32, c1: u32, r2: u32, c2: u32, id: u16) {
        let (r1, r2) = (r1.min(r2), r1.max(r2));
        let (c1, c2) = (c1.min(c2), c1.max(c2));
        self.display_style_revision = next_sheet_identity();
        if !self.loading {
            self.edited = true;
            self.styles_dirty = true;
        }
        if id != 0 {
            self.styles.insert_rect(r1, c1, r2, c2, id);
            self.style_edits.insert_rect(r1, c1, r2, c2);
        } else {
            self.styles
                .remove_rect_recording(r1, c1, r2, c2, &mut self.style_edits);
            if !self.row_styles.is_empty() {
                let hit_rows: Vec<u32> = self
                    .row_styles
                    .keys()
                    .copied()
                    .filter(|&r| r >= r1 && r <= r2)
                    .collect();
                for r in hit_rows {
                    self.styles.insert_rect(r, c1, r, c2, 0);
                    self.style_edits.insert_rect(r, c1, r, c2);
                }
            }
            if !self.col_styles.is_empty() {
                let hit_cols: Vec<u32> = self
                    .col_styles
                    .keys()
                    .copied()
                    .filter(|&c| c >= c1 && c <= c2)
                    .collect();
                for c in hit_cols {
                    self.styles.insert_rect(r1, c, r2, c, 0);
                    self.style_edits.insert_rect(r1, c, r2, c);
                }
            }
        }
    }

    /// 批量清空矩形区域单元格内容（跳过空页并整块释放完整列页），返回被清空的非空格数。
    pub fn clear_cells_rect(&mut self, r1: u32, c1: u32, r2: u32, c2: u32) -> usize {
        let (r1, r2) = (r1.min(r2), r1.max(r2));
        let (c1, c2) = (c1.min(c2), c1.max(c2));
        let removed = self.cells.clear_rect(r1, c1, r2, c2, &mut self.cleared);
        if removed > 0 && !self.loading {
            self.edited = true;
        }
        removed
    }

    /// 仅在 `[c1..=c2]` 列带内将 `>= at` 行的单元格与显式样式整块下移或删除上移。
    pub(crate) fn shift_col_band_rows(&mut self, c1: u32, c2: u32, at: u32, n: u32, delete: bool) {
        self.cells
            .shift_col_band_rows(c1, c2, at, n, delete, &mut self.cleared);
        let styles_before = self.style_edits.len();
        self.styles
            .shift_col_band_rows(c1, c2, at, n, delete, &mut self.style_edits);
        if self.style_edits.len() != styles_before {
            self.display_style_revision = next_sheet_identity();
            if !self.loading {
                self.styles_dirty = true;
            }
        }
        if !self.loading {
            self.edited = true;
        }
    }

    #[inline]
    pub(crate) fn cells_mut(&mut self) -> &mut Store {
        &mut self.cells
    }

    #[inline]
    pub(crate) fn cells_and_cleared_mut(&mut self) -> (&mut Store, &mut CellJournal) {
        (&mut self.cells, &mut self.cleared)
    }

    #[inline]
    pub(crate) fn bump_display_style_revision(&mut self) {
        self.display_style_revision = next_sheet_identity();
    }

    /// 已用区域 (r1, c1, r2, c2)，0-based
    pub fn used_bounds(&self) -> Option<(u32, u32, u32, u32)> {
        self.cells.used_bounds()
    }

    /// 单列连续访问（范围求值的快路径）
    pub fn column_span(&self, r1: u32, r2: u32, c: u32) -> Vec<Cow<'_, Cell>> {
        self.cells.column_span(r1, r2, c)
    }

    /// 单列按页零分配类型化切片迭代：返回 `(起始行号, ColSlice<'_>)`
    pub fn column_typed_slices(&self, r1: u32, r2: u32, c: u32) -> impl Iterator<Item = (u32, ColSlice<'_>)> + '_ {
        self.cells.column_typed_slices(r1, r2, c)
    }

    /// 按行列升序归并输出单元格视图与显式样式（`xlsx_fresh` 顺序写盘零拷贝零重复哈希查找）
    pub fn iter_cells_with_explicit_style(&self) -> impl Iterator<Item = (CellRef, CellView<'_>, Option<u16>)> + '_ {
        self.cells.iter_with_explicit_style(&self.styles)
    }

    /// 按行列升序并进遍历所有「非空单元格或显式样式格」
    pub fn iter_with_styles(&self) -> impl Iterator<Item = (CellRef, Option<CellView<'_>>, Option<u16>)> + '_ {
        self.cells.iter_merged_with_styles(&self.styles)
    }

    /// 稀疏行内非空边界跳跃（供 `Ctrl+←/→` 等导航跳过大段空白列，复杂度 O(字长位扫描)）
    pub fn next_occupied_in_row(&self, row: u32, from_col: u32, forward: bool) -> Option<u32> {
        self.cells.next_occupied_in_row(row, from_col, forward)
    }

    /// 稀疏列内非空边界跳跃（供 `Ctrl+↑/↓` 等导航跳过百万空行，复杂度 O(页数)）
    pub fn next_occupied_in_col(&self, col: u32, from_row: u32, forward: bool) -> Option<u32> {
        self.cells.next_occupied_in_col(col, from_row, forward)
    }

    /// 判断两张快照的指定 256 行页指针是否完全未变（供 AI 增量观测与后台缓存跳过整页）
    pub fn same_page(&self, other: &Self, page_idx: u32) -> bool {
        self.cells.same_page(&other.cells, page_idx)
    }

    /// 存储层实际占用字节（调试用）
    pub fn allocated_bytes(&self) -> usize {
        self.cells.allocated_bytes()
    }

    /// 存储层去重净有效字节（共享 `Arc` 页/列块只计一次）
    pub fn unique_payload_bytes(&self) -> usize {
        self.cells.unique_payload_bytes()
    }

    /// 诊断用：返回 `(页数, 已分配非空列块数, 槽位总容量)`
    pub fn store_stats(&self) -> (usize, usize, usize) {
        self.cells.stats()
    }
}

fn next_sheet_identity() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

impl Default for Sheet {
    fn default() -> Self { Self::new("") }
}

/// Imported pivot definitions are replayed from this immutable baseline.
/// Keeping it independent of the last saved file makes the structural journal
/// repeatable, including undo after a successful save.
#[derive(Clone, Debug, Default)]
pub struct PivotCacheBase {
    pub definitions: Vec<(String, Vec<u8>)>,
    pub sheets: Vec<(u64, String)>,
}

/// Marks pivot attachments produced by this open document. Cloning a save or
/// undo snapshot preserves ownership; opening the file creates a new session,
/// so previously persisted attachments become imported parts.
#[derive(Clone, Debug)]
pub struct PivotSession(String);

impl PivotSession {
    pub fn token(&self) -> &str { &self.0 }
}

impl Default for PivotSession {
    fn default() -> Self {
        let time = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default().as_nanos();
        Self(format!("{time:x}-{:x}-{:x}", std::process::id(), next_sheet_identity()))
    }
}

/// 日期体系：workbookPr/@date1904。1904 体系里序列 0 = 1904-01-01，
/// 同一日历日期比 1900 体系小 1462（且没有 1900 闰年 bug）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum DateSystem {
    #[default]
    D1900,
    D1904,
}

impl DateSystem {
    pub fn is_1904(self) -> bool {
        matches!(self, DateSystem::D1904)
    }
    /// 1904 体系相对 1900 的偏移（天）
    pub const OFFSET: f64 = 1462.0;

    pub fn name(self) -> &'static str {
        match self {
            DateSystem::D1900 => "1900",
            DateSystem::D1904 => "1904",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Workbook {
    /// 打开时有关键部件没读全（C14）。这类簿子不能用原文件当保存基准，
    /// 也不能就地覆盖——原文件里的东西我们根本没读到。
    pub load_incomplete: bool,
    /// 文件里是否带 VBA 宏（只保留，不执行）
    pub has_vba: bool,
    /// 自定义名称（名称管理器里的东西）：(名字, 作用表, 公式文本)
    pub names: Vec<(String, Option<usize>, String)>,
    /// 外部数据源（导入的 CSV、跨表取数），刷新用
    pub sources: Vec<crate::external::Source>,
    /// 跨工作簿引用（xl/externalLinks）；`[N]` 里的 N 就是这里的 index - 1
    pub links: Vec<crate::xlinks::ExternalBook>,
    /// 名称被改过
    pub names_dirty: bool,
    /// 结构改动记录：保存时用来搬透视表缓存里的源区域
    /// (表名, 0=行 1=列, at, n, 是否删除)
    pub pivot_shifts: Vec<(String, u8, u32, u32, bool)>,
    pub pivot_cache_base: std::sync::Arc<PivotCacheBase>,
    pub pivot_session: PivotSession,
    /// 保存时要挂上的透视表部件：(表序号, 规格, 结果, 输出矩形, 值字段名)
    pub pivots_pending: Vec<(
        usize,
        crate::pivot::PivotSpec,
        crate::pivot::PivotResult,
        (u32, u32, u32, u32),
        String,
    )>,
    /// 多字段透视：保存时挂真 pivotTable 部件（C12.4）
    /// C12.4 计算字段：(工作表, 名称, 公式)，保存时写进透视缓存
    pub pivot_calc_fields: Vec<(usize, String, String)>,
    pub pivots_pending2: Vec<(
        usize,
        crate::pivot_multi::PivotSpec2,
        crate::pivot_multi::PivotGrid,
        (u32, u32, u32, u32),
    )>,
    /// 现代化一等公民数据透视表对象集合（包含导入还原与会话新建，支持跨表/样式/下钻/持久化）
    pub pivots: Vec<crate::pivot_engine::PivotTableModel>,
    /// 被用户修改或删除的导入透视表部件路径，保存时从基准包中干净剥离
    pub pivots_removed_parts: Vec<String>,
    /// 样式表（单元格只存索引，见 Sheet.styles）
    pub styles: crate::style::StyleTable,
    pub sheets: Vec<Sheet>,
    pub path: Option<PathBuf>,
    /// 原文件有打开密码时：解密到临时文件的路径（保存时当基准）
    pub decrypted: Option<PathBuf>,
    /// 日期体系（workbookPr/@date1904）
    pub date_system: DateSystem,
    /// 手动计算模式（workbook.xml calcPr/@calcMode="manual"）
    pub calc_manual: bool,
    /// 迭代计算（calcPr/@iterate）：环上的格子用上一轮值反复求值
    pub calc_iterate: bool,
    /// 迭代上限（calcPr/@maxIterations，Excel 默认 100）
    pub calc_max_iterations: u32,
    /// 迭代收敛阈值（calcPr/@maxChange，Excel 默认 0.001）
    pub calc_max_change: f64,
    /// 手动模式下已有编辑但尚未重算，状态栏显示“待计算”
    pub calc_pending: bool,
    /// 手动模式下具体哪些工作表还有未算编辑；计算单表时不能误清别的表
    pub calc_pending_sheets: std::collections::BTreeSet<usize>,
    /// 上次完整重算/同步缓存时各工作表的 `cell_revision`；用于保存时决定是否需要 `fullCalcOnLoad="1"`。
    pub recalc_cell_revs: Vec<u64>,
    /// 打开密码设置；另存时继续加密
    pub encryption: Option<crate::crypto::EncryptionSettings>,
    /// Credentials for the package at `path`, independent of the next save's password.
    pub source_encryption: Option<crate::crypto::EncryptionSettings>,
    pub dirty: bool,
    /// 用户在应用里改过图表的部件名（保存时不要按基准还原这些部件）
    pub chart_edits: Vec<String>,
}

impl Workbook {
    pub fn new_blank() -> Self {
        Self {
            load_incomplete: false,
            has_vba: false,
            names: Vec::new(),
            sources: Vec::new(),
            links: Vec::new(),
            pivot_shifts: Vec::new(),
            pivot_cache_base: Default::default(),
            pivot_session: Default::default(),
            names_dirty: false,
            pivots_pending: Vec::new(),
            pivot_calc_fields: Vec::new(),
            pivots_pending2: Vec::new(),
            pivots: Vec::new(),
            pivots_removed_parts: Vec::new(),
            sheets: vec![Sheet::new("Sheet1")],
            path: None,
            decrypted: None,
            encryption: None,
            source_encryption: None,
            date_system: DateSystem::D1900,
            calc_manual: false,
            calc_iterate: false,
            calc_max_iterations: 100,
            calc_max_change: 0.001,
            calc_pending: false,
            calc_pending_sheets: std::collections::BTreeSet::new(),
            recalc_cell_revs: Vec::new(),
            dirty: false,
            chart_edits: Vec::new(),
            styles: crate::style::StyleTable::new(),
        }
    }

    /// 日期序列 ↔ 日历日期都走这里，避免各处各写一套 1462
    pub fn date_to_serial(&self, y: i32, m: u32, d: u32) -> Option<f64> {
        crate::formula::date_to_serial_in(self.date_system, y, m, d)
    }
    pub fn serial_to_date(&self, serial: f64) -> Option<(i32, u32, u32)> {
        crate::formula::serial_to_date_in(self.date_system, serial)
    }

    pub fn sheet(&self, idx: usize) -> &Sheet {
        &self.sheets[idx.min(self.sheets.len() - 1)]
    }

    pub fn sheet_mut(&mut self, idx: usize) -> &mut Sheet {
        let i = idx.min(self.sheets.len() - 1);
        &mut self.sheets[i]
    }

    pub fn title(&self) -> String {
        self.path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "未命名.xlsx".to_string())
    }

    /// 标记所有公式缓存与当前单元格修订号一致（重算完成后调用），避免保存时多余写出 `fullCalcOnLoad="1"`。
    pub fn mark_recalc_synced(&mut self) {
        if !self.calc_manual && !self.calc_pending {
            self.recalc_cell_revs = self.sheets.iter().map(|s| s.cell_revision()).collect();
        }
    }

    /// 仅在公式缓存可能过期/缺失时要求 Excel 打开全量重算；若重算已完成且缓存有效则返回 `false`。
    pub fn needs_full_calc_on_load(&self) -> bool {
        if self.calc_manual || !self.sheets.iter().any(|s| s.has_formulas()) {
            return false;
        }
        if self.calc_pending || self.load_incomplete || self.names_dirty {
            return true;
        }
        if self.recalc_cell_revs.len() == self.sheets.len()
            && self
                .sheets
                .iter()
                .zip(&self.recalc_cell_revs)
                .all(|(s, &r)| s.cell_revision() == r)
        {
            return false;
        }
        true
    }
}

/// 0 -> A, 25 -> Z, 26 -> AA
pub fn col_label(mut col: u32) -> String {
    let mut out = String::new();
    loop {
        out.insert(0, (b'A' + (col % 26) as u8) as char);
        if col < 26 {
            break;
        }
        col = col / 26 - 1;
    }
    out
}

/// "A1" 风格地址
pub fn cell_name(row: u32, col: u32) -> String {
    format!("{}{}", col_label(col), row + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        assert_eq!(col_label(0), "A");
        assert_eq!(col_label(25), "Z");
        assert_eq!(col_label(26), "AA");
        assert_eq!(col_label(701), "ZZ");
        assert_eq!(col_label(702), "AAA");
        assert_eq!(cell_name(0, 0), "A1");
        assert_eq!(cell_name(9, 27), "AB10");
    }

    #[test]
    fn parse() {
        assert_eq!(Value::parse_input("42"), Value::Number(42.0));
        assert_eq!(Value::parse_input(" 3.5 "), Value::Number(3.5));
        assert_eq!(Value::parse_input("abc"), Value::Text("abc".into()));
        assert_eq!(Value::parse_input(""), Value::Empty);
        assert_eq!(Value::parse_input("TRUE"), Value::Bool(true));
    }

    #[test]
    fn display_numbers() {
        assert_eq!(Value::Number(42.0).display(), "42");
        assert_eq!(Value::Number(3.5).display(), "3.5");
        assert_eq!(Value::Number(0.1 + 0.2).display(), "0.3");
    }

    #[test]
    fn cell_set_remove_if_present_preserves_cow_when_absent() {
        let mut sh = Sheet::new("SpillCow");
        for r in 0..16u32 {
            sh.spilled.insert(CellRef::new(r, 1));
        }
        let snap = sh.clone();
        assert!(sh.spilled.ptr_eq(&snap.spilled));

        // Writing outside `spilled` must NOT break CoW sharing of `spilled`.
        sh.set(100, 0, Cell { value: Value::Number(42.0), formula: None });
        assert!(sh.spilled.ptr_eq(&snap.spilled));
        assert!(!sh.spilled.remove_if_present(&CellRef::new(999, 9)));
        assert!(sh.spilled.ptr_eq(&snap.spilled));

        // Writing onto a spilled cell must detach and remove that entry.
        sh.set(5, 1, Cell { value: Value::Number(7.0), formula: None });
        assert!(!sh.spilled.ptr_eq(&snap.spilled));
        assert!(!sh.spilled.contains(&CellRef::new(5, 1)));
        assert!(snap.spilled.contains(&CellRef::new(5, 1)));
    }
}

/// XML 属性/文本里的实体还原（够用即可）
pub fn xml_unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&#10;", "
")
        .replace("&amp;", "&")
}

/// 解析 XML 实体引用（quick-xml 把 &amp; 单独发成 GeneralRef 事件）
pub fn resolve_entity(name: &str) -> String {
    match name {
        "amp" => "&".into(),
        "lt" => "<".into(),
        "gt" => ">".into(),
        "quot" => "\"".into(),
        "apos" => "'".into(),
        _ => {
            if let Some(num) = name.strip_prefix("#x").or_else(|| name.strip_prefix("#X")) {
                if let Ok(v) = u32::from_str_radix(num, 16) {
                    return char::from_u32(v).map(|c| c.to_string()).unwrap_or_default();
                }
            }
            if let Some(num) = name.strip_prefix('#') {
                if let Ok(v) = num.parse::<u32>() {
                    return char::from_u32(v).map(|c| c.to_string()).unwrap_or_default();
                }
            }
            String::new()
        }
    }
}
