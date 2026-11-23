//! 数据工具：多级排序、删除重复、分列。
//!
//! 排序和去重先生成实际变更，再一次提交；删除重复保留第一次出现；
//! 分列把一格按分隔符拆到右边相邻的列，右边的原有内容会被推开。

use crate::model::{Cell, Sheet, Value};
use crate::undo::CellDelta;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

/// 文本排序方式（Excel 的“排序依据”）
#[derive(Clone, Debug, PartialEq, Default)]
pub enum SortMode {
    /// 码位/字典序（旧行为）
    #[default]
    Default,
    /// 中文拼音序：走系统 zh-CN 排序表，和 Excel 在中文 Windows 上一致
    Pinyin,
    /// 中文笔画序：`assets/stroke_order.bin`（Excel xlStroke 实测顺序）
    Stroke,
    /// 自定义序列（高/中/低）：列表里靠前的排前面，不在列表里的按拼音排在后面
    Custom(Vec<String>),
}

/// 一个排序键
#[derive(Clone, Debug, PartialEq)]
pub struct SortKey {
    pub col: u32,
    pub asc: bool,
    /// true 时大小写不敏感
    pub ignore_case: bool,
    pub mode: SortMode,
}

impl SortKey {
    /// 文本列默认：大小写不敏感、字典序
    pub fn text(col: u32, asc: bool) -> Self {
        Self {
            col,
            asc,
            ignore_case: true,
            mode: SortMode::Default,
        }
    }
}

/// Precompute the display, locale and stroke keys once per unique/needed row and sort level.
struct PreparedKey {
    number: Option<f64>,
    boolean: Option<bool>,
    empty: bool,
    text: Box<[u8]>,
    custom: Option<usize>,
    strokes: Box<[Option<u16>]>,
}

impl PreparedKey {
    fn new(
        value: &Value,
        spec: &SortKey,
        ranks: &HashMap<String, usize>,
        key_cache: &mut crate::collate::SortKeyCache,
    ) -> Self {
        let mut result = Self {
            number: None,
            boolean: None,
            empty: false,
            text: Box::default(),
            custom: None,
            strokes: Box::default(),
        };
        match value {
            Value::Number(n) => {
                result.number = Some(*n);
                return result;
            }
            Value::Empty => {
                result.empty = true;
                return result;
            }
            Value::Bool(b) => result.boolean = Some(*b),
            _ => {}
        }
        let owned_buf: String;
        let text: &str = match value {
            Value::Text(t) if !spec.ignore_case || (t.is_ascii() && !t.bytes().any(|b| b.is_ascii_uppercase())) => t.as_ref(),
            Value::Text(t) => {
                owned_buf = t.to_lowercase();
                &owned_buf
            }
            _ => {
                let disp = value.display();
                owned_buf = if spec.ignore_case {
                    disp.to_lowercase()
                } else {
                    disp
                };
                &owned_buf
            }
        };
        if matches!(spec.mode, SortMode::Stroke) {
            result.strokes = text.chars().map(crate::collate::stroke_rank).collect();
        }
        if matches!(spec.mode, SortMode::Custom(_)) {
            result.custom = ranks.get(&text.trim().to_ascii_lowercase()).copied();
        }
        result.text = if matches!(spec.mode, SortMode::Default) {
            text.as_bytes().into()
        } else {
            key_cache.get_or_compute(text).into()
        };
        result
    }

    fn compare(&self, other: &Self, ascending: bool) -> Ordering {
        if self.empty || other.empty {
            return self.empty.cmp(&other.empty);
        }
        let order = match (self.number, other.number) {
            (Some(a), Some(b)) => a.partial_cmp(&b).unwrap_or(Ordering::Equal),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            _ => match (self.boolean, other.boolean) {
                (Some(a), Some(b)) => a.cmp(&b),
                _ => match (self.custom, other.custom) {
                    (Some(x), Some(y)) => x.cmp(&y),
                    (Some(_), None) => Ordering::Less,
                    (None, Some(_)) => Ordering::Greater,
                    (None, None) => self.compare_text(other),
                },
            },
        };
        if ascending { order } else { order.reverse() }
    }

    fn compare_text(&self, other: &Self) -> Ordering {
        for (a, b) in self.strokes.iter().zip(other.strokes.iter()) {
            match (a, b) {
                (Some(x), Some(y)) if x != y => return x.cmp(y),
                (Some(_), Some(_)) => {}
                _ => return self.text.cmp(&other.text),
            }
        }
        self.strokes
            .len()
            .cmp(&other.strokes.len())
            .then_with(|| self.text.cmp(&other.text))
    }
}

/// Ordered cells to change, suitable for one undo action. Every value is read
/// before mutation, including destinations that were empty before the operation.
pub type LinePermutation = ((u32,u32,u32,u32), bool, Vec<u32>);

#[derive(Debug, Default)]
pub struct RowChanges {
    pub permutation: Option<LinePermutation>,
    pub deltas: Vec<CellDelta>,
    pub count: usize,
}

/// Dense numeric sorts retain one row-major value snapshot and the permutation.
/// Deltas for audit, undo and collaboration are reconstructed in destination order.
pub struct NumericSort {
    pub sheet: usize,
    first_row: u32,
    first_col: u32,
    width: usize,
    values: Vec<f64>,
    styles: Vec<u16>,
    order: Vec<u32>,
}

impl NumericSort {
    /// The general planner handles mixed types, formulas, sparse ranges and column sorts.
    pub fn plan(
        sheet: &Sheet, index: usize, region: (u32, u32, u32, u32), key: u32, asc: bool,
    ) -> Result<Option<Self>, String> {
        let (r1, c1, r2, c2) = region;
        if r1 >= r2 || c1 > c2 || !(c1..=c2).contains(&key) { return Ok(None); }
        let width = (c2 - c1 + 1) as usize;
        let area = width as u64 * u64::from(r2 - r1 + 1);
        if area <= 100_000 || area > sheet.len() as u64 { return Ok(None); }
        crate::sort_filter::validate_sort_region(sheet, region)?;
        let mut values = Vec::new();
        let mut styles = Vec::new();
        let has_styles = !sheet.styles.is_empty() || !sheet.row_styles.is_empty() || !sheet.col_styles.is_empty();
        for r in r1..=r2 {
            for c in c1..=c2 {
                let Some(Cell { value: Value::Number(n), formula: None }) = sheet.get(r, c) else {
                    return Ok(None);
                };
                if !n.is_finite() { return Ok(None); }
                values.push(*n);
                if has_styles { styles.push(sheet.style_of(r, c)); }
            }
        }
        let mut order: Vec<_> = (0..=r2 - r1).collect();
        let key_col = (key - c1) as usize;
        order.sort_by(|&a, &b| {
            let left = values[a as usize * width + key_col];
            let right = values[b as usize * width + key_col];
            let cmp = left.partial_cmp(&right).expect("numeric sort has finite keys");
            if asc { cmp } else { cmp.reverse() }
        });
        Ok(Some(Self { sheet: index, first_row: r1, first_col: c1, width, values, styles, order }))
    }

    pub fn moved_rows(&self) -> usize {
        self.order.iter().enumerate().filter(|&(dst, src)| dst != *src as usize).count()
    }

    pub(crate) fn bytes(&self) -> usize {
        self.values.capacity() * size_of::<f64>() + self.styles.capacity() * size_of::<u16>()
            + self.order.capacity() * size_of::<u32>()
    }

    pub fn deltas(&self) -> impl DoubleEndedIterator<Item = CellDelta> + '_ {
        self.order.iter().enumerate().filter(|&(dst, src)| dst != *src as usize)
            .flat_map(move |(dst, &src)| (0..self.width).filter_map(move |col| {
                let (a, b) = (dst * self.width + col, src as usize * self.width + col);
                let (before_style, after_style) = if self.styles.is_empty() {
                    (0, 0)
                } else {
                    (self.styles[a], self.styles[b])
                };
                if self.values[a] == self.values[b] && before_style == after_style { return None; }
                let number = |i: usize| Cell { value: Value::Number(self.values[i]), formula: None };
                Some(CellDelta {
                    sheet: self.sheet, row: self.first_row + dst as u32, col: self.first_col + col as u32,
                    before: number(a), after: number(b), before_style, after_style,
                })
            }))
    }
}

/// Numeric regions use packed f64s. Other dense regions share the original cell
/// slabs and translate each affected formula when its delta is requested.
pub enum CompactSort {
    Numeric(NumericSort),
    Mixed { source: Box<Sheet>, index: usize, region: (u32,u32,u32,u32), order: Vec<u32>, horizontal: bool, bytes: usize },
}

impl CompactSort {
    pub fn plan(source: &Sheet, index: usize, region: (u32,u32,u32,u32), key: u32, asc: bool,
                horizontal: bool) -> Result<Option<Self>, String> {
        let (r1,c1,r2,c2) = region;
        if r1 > r2 || c1 > c2 { return Ok(None); }
        let area = u64::from(r2-r1+1) * u64::from(c2-c1+1);
        if area <= 100_000 { return Ok(None); }
        if !horizontal && let Some(n) = NumericSort::plan(source,index,region,key,asc)? {
            return Ok(Some(Self::Numeric(n)));
        }
        if !Self::retain_mixed_sheet(source,region) { return Ok(None); }
        let order = crate::sort_filter::sort_order(source,region,key,asc,horizontal)?;
        if order.is_empty() { return Ok(None); }
        Ok(Some(Self::mixed(source,index,(region,horizontal,order))))
    }

    fn retain_mixed_sheet(source: &Sheet, (r1,c1,r2,c2): (u32,u32,u32,u32)) -> bool {
        let area = u64::from(r2-r1+1) * u64::from(c2-c1+1);
        let stored = (source.len() + source.styles.len()) as u64;
        area > 100_000 && area <= stored.saturating_mul(4) && stored <= area.saturating_mul(4)
    }

    fn mixed(source: &Sheet, index: usize, (region,horizontal,order): LinePermutation) -> Self {
        let source = Box::new(source.search_snapshot());
        // This snapshot outlives the worker as undo history. Account for heap text
        // and styles as well as the page buffers before handing it to the UI.
        let bytes = source.allocated_bytes() + order.capacity()*size_of::<u32>()
            + source.styles.len() * size_of::<(crate::CellRef,u16)>()
            + (source.row_styles.capacity() + source.col_styles.capacity()) * size_of::<(u32,u16)>()
            + source.iter().map(|(_,c)| crate::undo::cell_heap_bytes(c)).sum::<usize>();
        Self::Mixed { source, index, region, order, horizontal, bytes }
    }

    /// Absolute source line for each destination, in the same order as deltas().
    pub fn permutation(&self) -> ((u32,u32,u32,u32), bool, Vec<u32>) {
        match self {
            Self::Numeric(n) => ((n.first_row, n.first_col, n.first_row + n.order.len() as u32 - 1,
                                  n.first_col + n.width as u32 - 1), false,
                                  n.order.iter().map(|line| n.first_row + line).collect()),
            Self::Mixed { region, horizontal, order, .. } => (*region, *horizontal, order.clone()),
        }
    }

    pub fn moved_lines(&self) -> usize {
        match self {
            Self::Numeric(n) => n.moved_rows(),
            Self::Mixed { order, region, horizontal, .. } => {
                let first = if *horizontal { region.1 } else { region.0 };
                order.iter().enumerate().filter(|&(i,k)| first+i as u32 != *k).count()
            }
        }
    }

    pub(crate) fn sheet(&self) -> usize {
        match self { Self::Numeric(n) => n.sheet, Self::Mixed { index,.. } => *index }
    }

    pub(crate) fn bytes(&self) -> usize {
        match self {
            Self::Numeric(n) => n.bytes(),
            Self::Mixed { bytes,.. } => *bytes,
        }
    }

    pub fn deltas(&self) -> Box<dyn DoubleEndedIterator<Item=CellDelta> + '_> {
        match self {
            Self::Numeric(n) => Box::new(n.deltas()),
            Self::Mixed { source,index,region,order,horizontal,.. } => {
                let (r1,c1,r2,c2) = *region;
                let (first, start, end) = if *horizontal { (c1,r1,r2) } else { (r1,c1,c2) };
                let cache = std::rc::Rc::new(std::cell::RefCell::new(crate::refs::FillFormulas::default()));
                Box::new(order.iter().copied().enumerate().filter(move |&(dst,src)| first+dst as u32 != src)
                    .flat_map(move |(dst,src)| {
                        let cache = cache.clone();
                        (start..=end).filter_map(move |offset| {
                            let mut fill = cache.borrow_mut();
                            reorder_delta_cached(source,*index,first+dst as u32,Some(src),offset,*horizontal,false,&mut fill)
                        })
                    }))
            }
        }
    }
}

fn reorder_delta_cached(
    sheet: &Sheet,
    sheet_idx: usize,
    dst: u32,
    src: Option<u32>,
    offset: u32,
    horizontal: bool,
    dedup: bool,
    fill_cache: &mut crate::refs::FillFormulas,
) -> Option<CellDelta> {
    if src == Some(dst) {
        return None;
    }
    let coords = |line| if horizontal { (offset, line) } else { (line, offset) };
    let (row, col) = coords(dst);
    let before = sheet.get(row, col);
    let before_style = sheet.style_of(row, col);
    let (source, after_style) = match src.map(coords) {
        Some((r, c)) => (sheet.get(r, c), sheet.style_of(r, c)),
        None => (None, before_style),
    };
    let translated = source
        .and_then(|cell| cell.formula.as_deref())
        .filter(|_| !dedup)
        .map(|formula| {
            let shift = i64::from(dst) - i64::from(src.unwrap());
            let dr = if horizontal { 0 } else { shift };
            let dc = if horizontal { shift } else { 0 };
            if !formula.contains('!') {
                fill_cache.translate(formula, dr, dc).into()
            } else {
                crate::refs::translate(formula, dr, dc, &sheet.name).into()
            }
        });
    if translated.is_none() && before == source && before_style == after_style {
        return None;
    }
    let mut after = source.cloned().unwrap_or_default();
    if let Some(f) = translated {
        after.formula = Some(f);
    }
    let before = before.cloned().unwrap_or_default();
    if before != after || before_style != after_style {
        return Some(CellDelta {
            sheet: sheet_idx,
            row,
            col,
            before,
            after,
            before_style,
            after_style,
        });
    }
    None
}

impl RowChanges {
    pub fn apply(&self, sheet: &mut Sheet) {
        for d in &self.deltas {
            if d.before != d.after {
                sheet.set(d.row, d.col, d.after.clone());
            }
            if d.before_style != d.after_style {
                sheet.set_style(d.row, d.col, d.after_style);
            }
        }
    }
}

/// `order` contains source lines in destination order. Fewer lines than the
/// region means deduplication: clear the tail's contents but retain its styles.
pub(crate) fn reorder_lines(
    sheet: &Sheet,
    sheet_idx: usize,
    rect: (u32, u32, u32, u32),
    order: Vec<u32>,
    horizontal: bool,
    dedup: bool,
) -> RowChanges {
    let (r1, c1, r2, c2) = rect;
    let (first, last, start, end) = if horizontal { (c1, c2, r1, r2) } else { (r1, r2, c1, c2) };
    let lines = (last - first + 1) as usize;
    let mut changes = RowChanges::default();
    changes.count = if dedup {
        lines - order.len()
    } else {
        order
            .iter()
            .enumerate()
            .filter(|&(i, &src)| src != first + i as u32)
            .count()
    };
    if changes.count == 0 {
        return changes;
    }

    let mut fill_cache = crate::refs::FillFormulas::default();
    let mut collect = |dst: u32, offset: u32| {
        let src = order.get((dst - first) as usize).copied();
        if let Some(d) = reorder_delta_cached(
            sheet,
            sheet_idx,
            dst,
            src,
            offset,
            horizontal,
            dedup,
            &mut fill_cache,
        ) {
            changes.deltas.push(d);
        }
    };

    // Dense data: one linear pass without a rectangle-sized source snapshot.
    // Sparse data: only the union of occupied source/destination coordinates
    // can change, except where different inherited line styles cross.
    let area = lines as u64 * (end - start + 1) as u64;
    if area <= 4096 || area <= (sheet.len() + sheet.styles.len()) as u64 * 4 {
        for dst in first..=last {
            if order.get((dst - first) as usize) == Some(&dst) {
                continue;
            }
            for offset in start..=end {
                collect(dst, offset);
            }
        }
    } else {
        let mut inverse = vec![None; lines];
        for (i, &src) in order.iter().enumerate() {
            inverse[(src - first) as usize] = Some(first + i as u32);
        }
        let mut candidates = Vec::new();
        for k in sheet
            .cells()
            .stored_cells_in(rect)
            .map(|(k, _)| k)
            .chain(sheet.styles.iter_rect(rect).map(|(k, _)| k))
        {
            let (line, offset) = if horizontal { (k.col, k.row) } else { (k.row, k.col) };
            if order.get((line - first) as usize) != Some(&line) {
                candidates.push((line, offset));
            }
            if let Some(dst) = inverse[(line - first) as usize] {
                if dst != line {
                    candidates.push((dst, offset));
                }
            }
        }
        candidates.sort_unstable();
        candidates.dedup();
        let inherited = if horizontal {
            &sheet.col_styles
        } else {
            &sheet.row_styles
        };
        let different_style = |dst: u32| {
            order
                .get((dst - first) as usize)
                .is_some_and(|src| inherited.get(src) != inherited.get(&dst))
        };
        for (dst, offset) in candidates {
            if !different_style(dst) {
                collect(dst, offset);
            }
        }
        if !inherited.is_empty() {
            for dst in first..=last {
                if different_style(dst) {
                    for offset in start..=end {
                        collect(dst, offset);
                    }
                }
            }
        }
    }
    if !dedup { changes.permutation = Some((rect, horizontal, order)); }
    changes
}

pub enum MultiSortPlan {
    Cells(RowChanges),
    Compact(CompactSort),
}

pub fn plan_large_sort_multi(sheet: &Sheet, index: usize, region: (u32,u32,u32,u32),
                            keys: &[SortKey], header: bool) -> Result<MultiSortPlan,String> {
    let Some((region,horizontal,order)) = multi_sort_order(sheet,region,keys,header)? else {
        return Ok(MultiSortPlan::Cells(RowChanges::default()));
    };
    if CompactSort::retain_mixed_sheet(sheet,region) {
        Ok(MultiSortPlan::Compact(CompactSort::mixed(sheet,index,(region,horizontal,order))))
    } else {
        Ok(MultiSortPlan::Cells(reorder_lines(sheet,index,region,order,horizontal,false)))
    }
}

pub fn plan_sort_multi(sheet: &Sheet, index: usize, region: (u32,u32,u32,u32),
                       keys: &[SortKey], header: bool) -> Result<RowChanges,String> {
    Ok(multi_sort_order(sheet,region,keys,header)?.map_or_else(RowChanges::default, |(rect,horizontal,order)|
        reorder_lines(sheet,index,rect,order,horizontal,false)))
}

fn multi_sort_order(sheet: &Sheet, region: (u32,u32,u32,u32), keys: &[SortKey], header: bool)
                    -> Result<Option<LinePermutation>,String> {
    let (mut r1, c1, mut r2, mut c2) = region;
    if header {
        r1 = r1.saturating_add(1);
    }
    if r2 <= r1 || c2 < c1 || keys.is_empty() {
        return Ok(None);
    }
    if (r2 - r1 + 1) as u64 * (c2 - c1 + 1) as u64 > 200_000 {
        let Some((_, _, ur2, uc2)) = sheet.used_bounds() else {
            return Ok(None);
        };
        r2 = r2.min(ur2);
        c2 = c2.min(uc2.max(keys.iter().map(|k| k.col).max().unwrap_or(c1)));
        if r2 <= r1 || c2 < c1 {
            return Ok(None);
        }
    }
    crate::sort_filter::validate_sort_region(sheet, (r1, c1, r2, c2))?;
    let mut order: Vec<u32> = (r1..=r2).collect();
    let active: Vec<&SortKey> = keys.iter().filter(|k| (c1..=c2).contains(&k.col)).collect();
    if active.is_empty() {
        return Ok(Some(((r1, c1, r2, c2), false, order)));
    }

    // Top-down MSD tie-refinement: sort all rows by primary key once, then evaluate
    // secondary/tertiary keys ONLY on tied sub-slices (`len > 1`).
    let mut ties: Vec<std::ops::Range<usize>> = vec![0..order.len()];
    for key in active {
        if ties.is_empty() {
            break;
        }
        let mut ranks = HashMap::new();
        if let SortMode::Custom(values) = &key.mode {
            for (idx, value) in values.iter().enumerate() {
                ranks.entry(value.trim().to_ascii_lowercase()).or_insert(idx);
            }
        }
        let mut key_cache = crate::collate::SortKeyCache::new();
        let mut next_ties: Vec<std::ops::Range<usize>> = Vec::new();

        if ties.len() == 1 && ties[0] == (0..order.len()) {
            let prepared: Vec<PreparedKey> = (r1..=r2)
                .map(|r| PreparedKey::new(sheet.value_ref(r, key.col), key, &ranks, &mut key_cache))
                .collect();
            order.sort_by(|&a, &b| {
                prepared[(a - r1) as usize].compare(&prepared[(b - r1) as usize], key.asc)
            });
            let mut i = 0usize;
            while i < order.len() {
                let mut j = i + 1;
                while j < order.len()
                    && prepared[(order[i] - r1) as usize]
                        .compare(&prepared[(order[j] - r1) as usize], key.asc)
                        == Ordering::Equal
                {
                    j += 1;
                }
                if j - i > 1 {
                    next_ties.push(i..j);
                }
                i = j;
            }
        } else {
            for span in ties {
                let mut local: Vec<(PreparedKey, u32)> = order[span.clone()]
                    .iter()
                    .map(|&r| {
                        (
                            PreparedKey::new(sheet.value_ref(r, key.col), key, &ranks, &mut key_cache),
                            r,
                        )
                    })
                    .collect();
                local.sort_by(|a, b| a.0.compare(&b.0, key.asc));
                let mut i = 0usize;
                while i < local.len() {
                    let mut j = i + 1;
                    while j < local.len() && local[i].0.compare(&local[j].0, key.asc) == Ordering::Equal {
                        j += 1;
                    }
                    if j - i > 1 {
                        next_ties.push((span.start + i)..(span.start + j));
                    }
                    i = j;
                }
                for (dst, (_, row_id)) in order[span].iter_mut().zip(local) {
                    *dst = row_id;
                }
            }
        }
        ties = next_ties;
    }
    Ok(Some(((r1, c1, r2, c2), false, order)))
}

pub fn sort_multi(sheet: &mut Sheet, region: (u32, u32, u32, u32), keys: &[SortKey], header: bool) -> usize {
    let changes = plan_sort_multi(sheet, 0, region, keys, header).unwrap_or_default();
    changes.apply(sheet);
    changes.count
}

#[inline]
fn fnv_mix(mut h: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

#[inline]
fn hash_display_field(h: u64, col: u32, v: &Value) -> u64 {
    match v {
        Value::Empty => h,
        Value::Text(t) if t.is_empty() => h,
        Value::Text(t) => {
            let mut h = fnv_mix(h, &col.to_le_bytes());
            h = fnv_mix(h, &(t.len() as u64).to_le_bytes());
            fnv_mix(h, t.as_bytes())
        }
        Value::Bool(b) => {
            let s = if *b { b"TRUE".as_slice() } else { b"FALSE".as_slice() };
            let mut h = fnv_mix(h, &col.to_le_bytes());
            h = fnv_mix(h, &(s.len() as u64).to_le_bytes());
            fnv_mix(h, s)
        }
        Value::Error(e) => {
            let s = e.text().as_bytes();
            let mut h = fnv_mix(h, &col.to_le_bytes());
            h = fnv_mix(h, &(s.len() as u64).to_le_bytes());
            fnv_mix(h, s)
        }
        Value::Number(_) => {
            let text = v.display();
            if text.is_empty() {
                h
            } else {
                let mut h = fnv_mix(h, &col.to_le_bytes());
                h = fnv_mix(h, &(text.len() as u64).to_le_bytes());
                fnv_mix(h, text.as_bytes())
            }
        }
    }
}

fn values_same_display(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Empty, Value::Empty) => true,
        (Value::Empty, Value::Text(t)) | (Value::Text(t), Value::Empty) => t.is_empty(),
        (Value::Text(x), Value::Text(y)) => x == y,
        (Value::Number(x), Value::Number(y)) if x == y => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Error(x), Value::Error(y)) => x == y,
        _ => a.display() == b.display(),
    }
}

fn rows_match_on_cols(sheet: &Sheet, r_a: u32, r_b: u32, cols: &[u32]) -> bool {
    cols.iter()
        .all(|&c| values_same_display(sheet.value_ref(r_a, c), sheet.value_ref(r_b, c)))
}

fn distinct_rows(
    sheet: &Sheet,
    region: (u32, u32, u32, u32),
    cols: &[u32],
    header: bool,
) -> Option<((u32, u32, u32, u32), Vec<u32>)> {
    let (mut r1, c1, mut r2, mut c2) = region;
    if header && r2 > r1 {
        r1 += 1;
    }
    if r2 < r1 || c2 < c1 {
        return None;
    }
    if (r2 - r1 + 1) as u64 * (c2 - c1 + 1) as u64 > 200_000 {
        let (_, _, ur2, uc2) = sheet.used_bounds()?;
        r2 = r2.min(ur2);
        c2 = c2.min(uc2);
        if r2 < r1 || c2 < c1 {
            return None;
        }
    }
    let mut use_cols: Vec<u32> = if cols.is_empty() {
        (c1..=c2).collect()
    } else {
        cols.to_vec()
    };
    // Deduplicate column list preserving first occurrence so repeated col specs behave identically.
    let mut col_seen = HashSet::with_capacity(use_cols.len());
    use_cols.retain(|c| col_seen.insert(*c));

    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    let mut buckets: HashMap<u64, Vec<u32>> = HashMap::new();
    let mut keep = Vec::new();
    let area = (r2 - r1 + 1) as u64 * use_cols.len() as u64;
    if area > 4096 && area > sheet.len() as u64 * 4 {
        let selected: HashSet<u32> = use_cols.iter().copied().collect();
        let min_sel = use_cols.iter().copied().min().unwrap_or(c1);
        let max_sel = use_cols.iter().copied().max().unwrap_or(c2);
        let mut row_hashes: HashMap<u32, u64> = HashMap::new();
        // `sheet.iter_rect` visits in ascending `(row, col)` order!
        let mut sorted_cols = use_cols.clone();
        sorted_cols.sort_unstable();
        for (k, cell) in sheet.iter_rect((r1, min_sel, r2, max_sel)) {
            if selected.contains(&k.col) {
                let entry = row_hashes.entry(k.row).or_insert(FNV_OFFSET);
                *entry = hash_display_field(*entry, k.col, &cell.value);
            }
        }
        let mut seen_blank_row = false;
        for row in r1..=r2 {
            let h = row_hashes.get(&row).copied().unwrap_or(FNV_OFFSET);
            if h == FNV_OFFSET && !row_hashes.contains_key(&row) {
                if !seen_blank_row {
                    seen_blank_row = true;
                    keep.push(row);
                }
                continue;
            }
            let bucket = buckets.entry(h).or_default();
            if !bucket.iter().any(|&prev| rows_match_on_cols(sheet, prev, row, &sorted_cols)) {
                bucket.push(row);
                keep.push(row);
            }
        }
    } else {
        for row in r1..=r2 {
            let mut h = FNV_OFFSET;
            for &c in &use_cols {
                h = hash_display_field(h, c, sheet.value_ref(row, c));
            }
            let bucket = buckets.entry(h).or_default();
            if !bucket.iter().any(|&prev| rows_match_on_cols(sheet, prev, row, &use_cols)) {
                bucket.push(row);
                keep.push(row);
            }
        }
    }
    Some(((r1, c1, r2, c2), keep))
}

pub fn count_duplicates(sheet: &Sheet, region: (u32, u32, u32, u32), cols: &[u32], header: bool) -> usize {
    distinct_rows(sheet, region, cols, header)
        .map_or(0, |((r1, _, r2, _), keep)| (r2 - r1 + 1) as usize - keep.len())
}

pub fn plan_remove_duplicates(
    sheet: &Sheet,
    sheet_idx: usize,
    region: (u32, u32, u32, u32),
    cols: &[u32],
    header: bool,
) -> RowChanges {
    let Some((region, keep)) = distinct_rows(sheet, region, cols, header) else {
        return RowChanges::default();
    };
    reorder_lines(sheet, sheet_idx, region, keep, false, true)
}

pub fn remove_duplicates(sheet: &mut Sheet, region: (u32, u32, u32, u32), cols: &[u32], header: bool) -> usize {
    let changes = plan_remove_duplicates(sheet, 0, region, cols, header);
    changes.apply(sheet);
    changes.count
}

/// 分列：按分隔符把每格拆到右边相邻列（右侧内容整体右移腾位置）
/// 分列选项（Excel“分列”向导里的那些开关）
#[derive(Clone, Debug)]
pub struct SplitOpts {
    pub delim: char,
    /// 连续分隔符当一个（“a,,b” 或 “a   b” 不产生空列）
    pub merge_consecutive: bool,
    /// 双引号包起来的字段里不拆，`""` 表示一个引号
    pub quote: bool,
    /// 识别数字/日期；前导零（00123）保留为文本
    pub detect_types: bool,
    pub date_system: crate::model::DateSystem,
    /// 固定宽度模式：每列占几个字符（按字符数，不按字节）；None = 按分隔符拆
    pub fixed_widths: Option<Vec<usize>>,
    /// 逐列格式（Excel「列数据格式」）：None = 全部走 detect_types
    pub col_modes: Option<Vec<ColMode>>,
}

/// 分列的逐列格式
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColMode {
    /// 常规：按 detect_types 猜
    General,
    /// 文本：原样保留字符串（不转数字/日期）
    Text,
    /// 日期：能解析成日期就写序列号
    Date,
    /// 不导入此列：该列丢掉，后面的列左移
    Skip,
}

impl Default for SplitOpts {
    fn default() -> Self {
        Self {
            delim: ',',
            merge_consecutive: false,
            quote: true,
            detect_types: true,
            date_system: crate::model::DateSystem::D1900,
            fixed_widths: None,
            col_modes: None,
        }
    }
}

/// 分列结果
#[derive(Clone, Debug, Default)]
pub struct SplitReport {
    /// 写出去的格子数
    pub written: usize,
    /// 会覆盖掉右边原有非空格子的个数（执行前用于提示）
    pub overwritten: usize,
    /// 拆出来的最大列数
    pub width: usize,
    /// 识别成日期的格子（调用方补日期格式）
    pub date_cells: Vec<(u32, u32)>,
}

/// 按选项拆一行文本
pub fn split_fields(line: &str, opts: &SplitOpts) -> Vec<String> {
    if let Some(widths) = &opts.fixed_widths {
        return split_fixed(line, widths);
    }
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut in_quote = false;
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        if opts.quote && ch == '"' {
            if in_quote && chars.peek() == Some(&'"') {
                cur.push('"');
                chars.next();
            } else {
                in_quote = !in_quote;
            }
            continue;
        }
        if !in_quote && ch == opts.delim {
            if opts.merge_consecutive && cur.trim().is_empty() {
                cur.clear();
                continue;
            }
            out.push(std::mem::take(&mut cur));
            continue;
        }
        cur.push(ch);
    }
    out.push(cur);
    if opts.merge_consecutive {
        out.retain(|x| !x.trim().is_empty());
    }
    out
}

fn detect_value(text: &str, opts: &SplitOpts) -> (Value, bool) {
    let t = text.trim();
    if !opts.detect_types || t.is_empty() {
        return (Value::Text(text.to_string().into()), false);
    }
    // 前导零（00123、007）当文本，避免变成 123
    if t.len() > 1 && t.starts_with('0') && t.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return (Value::Text(text.to_string().into()), false);
    }
    if let Ok(n) = t.parse::<f64>() {
        return (Value::Number(n), false);
    }
    // 日期：2026-03-05 / 2026/3/5
    let norm = t.replace('/', "-");
    let parts: Vec<&str> = norm.split('-').collect();
    if parts.len() == 3 {
        if let (Ok(y), Ok(m), Ok(d)) = (
            parts[0].parse::<i32>(),
            parts[1].parse::<u32>(),
            parts[2].parse::<u32>(),
        ) {
            if (1900..=9999).contains(&y) && parts[0].len() == 4 {
                if let Some(serial) = crate::formula::date_to_serial_in(opts.date_system, y, m, d) {
                    return (Value::Number(serial), true);
                }
            }
        }
    }
    (Value::Text(text.to_string().into()), false)
}

/// 分列：把区域每一行按分隔符拆开写回，右边原有的内容整体右移。
///
/// `dry_run` 为真时只统计会覆盖多少非空格子，不改表。
pub fn text_to_columns_ex(
    sheet: &mut Sheet,
    region: (u32, u32, u32, u32),
    opts: &SplitOpts,
    dry_run: bool,
) -> SplitReport {
    let (r1, c1, r2, c2) = region;
    let mut report = SplitReport::default();
    let mut rows: Vec<(u32, Vec<String>)> = Vec::new();
    for r in r1..=r2 {
        let text = sheet
            .get(r, c1)
            .map(|x| x.value.display())
            .unwrap_or_default();
        if text.is_empty() {
            continue;
        }
        let fields = split_fields(&text, opts);
        let kept = match &opts.col_modes {
            Some(modes) => fields
                .iter()
                .enumerate()
                .filter(|(i, _)| modes.get(*i).copied().unwrap_or(ColMode::General) != ColMode::Skip)
                .count(),
            None => fields.len(),
        };
        report.width = report.width.max(kept);
        rows.push((r, fields));
    }
    let shift = report.width.saturating_sub(1) as u32;
    if dry_run {
        for (r, fields) in &rows {
            for (i, _) in fields.iter().enumerate().skip(1) {
                let c = c1 + i as u32;
                let occupied = sheet
                    .get(*r, c)
                    .map(|x| !matches!(x.value, Value::Empty) && !x.value.display().is_empty())
                    .unwrap_or(false);
                if occupied {
                    report.overwritten += 1;
                }
            }
        }
        return report;
    }
    // 右侧被推开的列：整块只搬一次，从最右往左
    if shift > 0 {
        let last = sheet.used_bounds().map(|b| b.3).unwrap_or(c2).max(c2);
        for cc in (c1 + 1..=last).rev() {
            for rr in r1..=r2 {
                if let Some(cell) = sheet.get(rr, cc).cloned() {
                    sheet.set(rr, cc + shift, cell);
                    let st = sheet.style_of(rr, cc);
                    sheet.set_style(rr, cc + shift, st);
                }
            }
        }
    }
    for (r, fields) in &rows {
        let mut out_c = c1;
        for (i, f) in fields.iter().enumerate() {
            let mode = opts
                .col_modes
                .as_ref()
                .and_then(|m| m.get(i).copied())
                .unwrap_or(ColMode::General);
            if mode == ColMode::Skip {
                continue;
            }
            let (v, is_date) = match mode {
                ColMode::Text => (Value::Text(f.clone().into()), false),
                ColMode::Date => {
                    let (v, d) = detect_value(f, opts);
                    if d {
                        (v, true)
                    } else {
                        (Value::Text(f.clone().into()), false)
                    }
                }
                _ => detect_value(f, opts),
            };
            if is_date {
                report.date_cells.push((*r, out_c));
            }
            sheet.set(*r, out_c, Cell { value: v, formula: None });
            report.written += 1;
            out_c += 1;
        }
    }
    report
}

/// 兼容旧调用：单分隔符、不合并、不识别类型
pub fn text_to_columns(sheet: &mut Sheet, region: (u32, u32, u32, u32), delim: char) -> usize {
    let opts = SplitOpts {
        delim,
        merge_consecutive: false,
        quote: false,
        detect_types: false,
        date_system: crate::model::DateSystem::D1900,
        fixed_widths: None,
        col_modes: None,
    };
    text_to_columns_ex(sheet, region, &opts, false).width
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_mixed_sort_matches_formula_and_style_deltas_in_both_directions() {
        let mut sheet = Sheet::new("Mixed");
        for row in 0..321 {
            for col in 0..320 {
                let n = f64::from(321-row + 320-col);
                sheet.set(row,col,Cell { value: Value::Number(n), formula: None });
            }
        }
        sheet.set(1,1, Cell { value: Value::Text("mixed".into()), formula: None });
        sheet.set(4,7, Cell { value: Value::Number(3.0), formula: Some("=A1+$B$2".into()) });
        sheet.set_style(1,1,8);
        let region = (0,0,320,319);
        for horizontal in [false,true] {
            let compact = CompactSort::plan(&sheet,0,region,0,true,horizontal).unwrap().unwrap();
            let full = crate::sort_filter::plan_sort_axis(&sheet,0,region,0,true,horizontal).unwrap();
            let mut iter = compact.deltas();
            for expected in full.deltas {
                let d = iter.next().unwrap();
                assert_eq!((d.row,d.col,d.before,d.after,d.before_style,d.after_style),
                    (expected.row,expected.col,expected.before,expected.after,expected.before_style,expected.after_style));
            }
            assert!(iter.next().is_none());
        }
    }

    #[test]
    fn dense_numeric_sort_replays_style_and_value_deltas_under_undo_guard() {
        let mut wb = crate::Workbook::new_blank();
        let sheet = &mut wb.sheets[0];
        let (rows, cols) = (1001, 101);
        for row in 0..rows {
            for col in 0..cols {
                sheet.set(row, col, Cell { value: Value::Number((rows - row + col) as f64), formula: None });
            }
        }
        sheet.set_style(rows - 1, 2, 4);
        let rect = (0, 0, rows - 1, cols - 1);
        let sort = NumericSort::plan(sheet, 0, rect, 0, true).unwrap().unwrap();
        let standard = crate::sort_filter::plan_sort_axis(sheet, 0, rect, 0, true, false).unwrap();
        assert_eq!(sort.deltas().count(), standard.deltas.len());
        for (d, expected) in sort.deltas().zip(&standard.deltas) {
            assert_eq!((d.row, d.col, d.before, d.after, d.before_style, d.after_style),
                (expected.row, expected.col, expected.before.clone(), expected.after.clone(),
                 expected.before_style, expected.after_style));
        }
        assert!(sort.bytes() < standard.deltas.len() * size_of::<CellDelta>() / 3);
        standard.apply(sheet);
        let mut undo = crate::undo::UndoStack::new();
        undo.push_numeric_sort("sort", sort, (0,0,0));
        let reverted = undo.undo_workbook(&mut wb, |_,r,c,_| (r,c) != (0,0)).unwrap();
        assert_eq!(reverted.skipped, 1);
        assert_eq!(wb.sheets[0].value(0,0), Value::Number(1.0));
        assert_eq!(wb.sheets[0].style_of(rows-1,2), 4);
        undo.redo_workbook(&mut wb, |_,_,_,_| true).unwrap();
        assert_eq!(wb.sheets[0].style_of(0,2), 4);
    }

    fn sheet_with(rows: &[&[&str]]) -> Sheet {
        let mut s = Sheet::new("T");
        for (r, row) in rows.iter().enumerate() {
            for (c, v) in row.iter().enumerate() {
                s.set(
                    r as u32,
                    c as u32,
                    Cell { value: Value::parse_input(v), formula: None },
                );
            }
        }
        s
    }

    fn col(s: &Sheet, c: u32, r1: u32, r2: u32) -> Vec<String> {
        (r1..=r2)
            .map(|r| s.get(r, c).map(|x| x.value.display()).unwrap_or_default())
            .collect()
    }

    #[test]
    fn multi_level_sort() {
        let mut s = sheet_with(&[
            &["部门", "姓名", "金额"],
            &["研发", "甲", "300"],
            &["销售", "乙", "100"],
            &["研发", "丙", "200"],
        ]);
        let moved = sort_multi(
            &mut s,
            (0, 0, 3, 2),
            &[
                SortKey::text(0, true),
                SortKey::text(2, false),
            ],
            true,
        );
        assert!(moved > 0);
        assert_eq!(col(&s, 0, 1, 3), vec!["研发", "研发", "销售"]);
        // 同部门内按金额降序
        assert_eq!(col(&s, 2, 1, 3), vec!["300", "200", "100"]);
    }

    #[test]
    fn sort_puts_empty_last() {
        let mut s = sheet_with(&[&["3"], &[""], &["1"]]);
        sort_multi(&mut s, (0, 0, 2, 0), &[SortKey::text(0, true)], false);
        assert_eq!(col(&s, 0, 0, 2), vec!["1", "3", ""]);
    }

    #[test]
    fn remove_duplicates_keeps_first() {
        let mut s = sheet_with(&[
            &["a", "1"],
            &["b", "2"],
            &["a", "1"],
            &["c", "3"],
            &["a", "1"],
        ]);
        let removed = remove_duplicates(&mut s, (0, 0, 4, 1), &[0, 1], false);
        assert_eq!(removed, 2);
        assert_eq!(col(&s, 0, 0, 2), vec!["a", "b", "c"]);
        assert_eq!(col(&s, 0, 3, 4), vec!["", ""]);
    }

    #[test]
    fn text_to_columns_splits_and_shifts() {
        let mut s = sheet_with(&[&["甲,100", "备注A"], &["乙,200", "备注B"]]);
        let n = text_to_columns(&mut s, (0, 0, 1, 1), ',');
        assert_eq!(n, 2);
        assert_eq!(col(&s, 0, 0, 1), vec!["甲", "乙"]);
        assert_eq!(col(&s, 1, 0, 1), vec!["100", "200"]);
        // 原来的备注被推到右边
        assert_eq!(col(&s, 2, 0, 1), vec!["备注A", "备注B"]);
    }

    fn col_text(s: &Sheet, c: u32, n: u32) -> Vec<String> {
        (0..n)
            .map(|r| s.get(r, c).map(|x| x.value.display()).unwrap_or_default())
            .collect()
    }

    #[test]
    fn pinyin_sort_matches_chinese_windows_order() {
        let mut s = Sheet::new("S");
        for (i, n) in ["张三", "李四", "王五", "赵六", "陈七"].iter().enumerate() {
            s.set(i as u32, 0, Cell { value: Value::Text((*n).into()), formula: None });
        }
        let mut k = SortKey::text(0, true);
        k.mode = SortMode::Pinyin;
        sort_multi(&mut s, (0, 0, 4, 0), &[k], false);
        let got = col_text(&s, 0, 5);
        assert_eq!(got, vec!["陈七", "李四", "王五", "张三", "赵六"], "拼音序：{got:?}");
    }

    #[test]
    fn custom_sequence_orders_by_list_position() {
        let mut s = Sheet::new("S");
        for (i, n) in ["低", "高", "中", "未知", "高"].iter().enumerate() {
            s.set(i as u32, 0, Cell { value: Value::Text((*n).into()), formula: None });
        }
        let mut k = SortKey::text(0, true);
        k.mode = SortMode::Custom(vec!["高".into(), "中".into(), "低".into()]);
        sort_multi(&mut s, (0, 0, 4, 0), &[k], false);
        let got = col_text(&s, 0, 5);
        assert_eq!(got, vec!["高", "高", "中", "低", "未知"], "自定义序列：{got:?}");
    }


    #[test]
    fn split_respects_quotes_and_consecutive_delims() {
        let opts = SplitOpts { delim: ',', merge_consecutive: false, quote: true, ..Default::default() };
        assert_eq!(
            split_fields(r#"甲,"乙,丙",丁"#, &opts),
            vec!["甲", "乙,丙", "丁"]
        );
        assert_eq!(split_fields(r#""他说""好""#, &opts), vec![r#"他说"好"#]);
        let opts2 = SplitOpts { delim: ' ', merge_consecutive: true, quote: false, ..Default::default() };
        assert_eq!(split_fields("甲   乙  丙", &opts2), vec!["甲", "乙", "丙"]);
    }

    #[test]
    fn split_keeps_leading_zeros_and_detects_dates() {
        let mut s = Sheet::new("S");
        s.set(0, 0, Cell { value: Value::Text("00123".into()), formula: None });
        s.set(1, 0, Cell { value: Value::Text("2026-03-05".into()), formula: None });
        s.set(2, 0, Cell { value: Value::Text("12.50".into()), formula: None });
        let opts = SplitOpts::default();
        let rep = text_to_columns_ex(&mut s, (0, 0, 2, 0), &opts, false);
        match &s.get(0, 0).unwrap().value {
            Value::Text(t) => assert_eq!(t.as_ref(), "00123", "前导零必须保留为文本"),
            v => panic!("00123 不应变成数字：{v:?}"),
        }
        match &s.get(1, 0).unwrap().value {
            Value::Number(n) => assert!((*n - 46086.0).abs() < 0.001, "2026-03-05 的序列值：{n}"),
            v => panic!("日期应识别为日期序列：{v:?}"),
        }
        assert_eq!(rep.date_cells, vec![(1, 0)]);
        assert!(matches!(s.get(2, 0).unwrap().value, Value::Number(_)));
    }

    #[test]
    fn split_warns_before_overwriting_non_empty_cells() {
        let mut s = Sheet::new("S");
        s.set(0, 0, Cell { value: Value::Text("甲,乙".into()), formula: None });
        s.set(0, 1, Cell { value: Value::Text("原有".into()), formula: None });
        s.set(0, 2, Cell { value: Value::Text("也有".into()), formula: None });
        let opts = SplitOpts::default();
        let dry = text_to_columns_ex(&mut s, (0, 0, 0, 0), &opts, true);
        assert_eq!(dry.overwritten, 1, "只有紧邻的一格被覆盖，后面的整体右移");
        assert_eq!(dry.written, 0);
        assert_eq!(s.get(0, 1).unwrap().value.display(), "原有", "试算不能改表");
        let rep = text_to_columns_ex(&mut s, (0, 0, 0, 0), &opts, false);
        assert_eq!(rep.width, 2);
        assert_eq!(s.get(0, 1).unwrap().value.display(), "乙");
        assert_eq!(s.get(0, 2).unwrap().value.display(), "原有", "原有内容右移一列");
        assert_eq!(s.get(0, 3).unwrap().value.display(), "也有");
    }

}

/// 固定宽度分列：按字符数切（Excel 的「固定宽度」也是按显示宽度，这里按字符数，
/// 中日韩全角字符一个算一个，够用且可预测）。空段保留成空串，行为与 Excel 一致。
pub fn split_fixed(line: &str, widths: &[usize]) -> Vec<String> {
    let chars: Vec<char> = line.chars().collect();
    let mut out = Vec::new();
    let mut at = 0usize;
    for &w in widths {
        if w == 0 {
            continue;
        }
        let end = (at + w).min(chars.len());
        out.push(chars[at..end].iter().collect::<String>());
        at = end;
        if at >= chars.len() {
            break;
        }
    }
    // Excel 里最后一列的右边界是「文本末尾」，断点之后的剩余部分留在最后一列
    if at < chars.len() {
        let tail: String = chars[at..].iter().collect();
        match out.last_mut() {
            Some(last) => last.push_str(&tail),
            None => out.push(tail),
        }
    }
    out
}

/// 「10,20,30」→ [10,20,30]；有任何非正数或非数字就返回 None。
pub fn parse_fixed_widths(s: &str) -> Option<Vec<usize>> {
    let mut out = Vec::new();
    for part in s.split([',', '，', ' ']).filter(|p| !p.trim().is_empty()) {
        let v: usize = part.trim().parse().ok()?;
        if v == 0 {
            return None;
        }
        out.push(v);
    }
    if out.is_empty() { None } else { Some(out) }
}

#[cfg(test)]
mod fixed_width_tests {
    use super::*;

    #[test]
    fn fixed_width_splits_by_char_count() {
        // 断点之后的剩余部分并入最后一列（Excel 的最后一列右边界到文本末尾）
        assert_eq!(split_fixed("ABCDEFG", &[3, 2]), vec!["ABC", "DEFG"]);
        assert_eq!(split_fixed("甲1乙2丙", &[2, 2]), vec!["甲1", "乙2丙"]);
        assert_eq!(split_fixed("AB", &[4, 4]), vec!["AB"], "短于宽度时不补空格");
        assert_eq!(split_fixed("", &[3]), vec![""]);
    }

    #[test]
    fn fixed_width_parse_rejects_zero_and_garbage() {
        assert_eq!(parse_fixed_widths("10,20,15"), Some(vec![10, 20, 15]));
        assert_eq!(parse_fixed_widths(" 10 ， 20 "), Some(vec![10, 20]));
        assert_eq!(parse_fixed_widths("10,0"), None);
        assert_eq!(parse_fixed_widths("10,x"), None);
        assert_eq!(parse_fixed_widths(""), None);
    }

    #[test]
    fn fixed_width_mode_bypasses_delimiters() {
        let opts = SplitOpts {
            fixed_widths: Some(vec![3, 3]),
            ..Default::default()
        };
        assert_eq!(split_fields("AB,CD,EF", &opts), vec!["AB,", "CD,EF"]);
    }
}

#[cfg(test)]
mod col_mode_tests {
    use super::*;
    use crate::model::Cell;

    fn sheet_with(line: &str) -> Sheet {
        let mut s = Sheet::new("T");
        s.set(0, 0, Cell { value: Value::Text(line.into()), formula: None });
        s
    }

    #[test]
    fn col_modes_text_date_skip() {
        let mut s = sheet_with("00123,2024-01-02,尾巴");
        let opts = SplitOpts {
            delim: ',',
            col_modes: Some(vec![ColMode::Text, ColMode::Date, ColMode::Skip]),
            ..Default::default()
        };
        let rep = text_to_columns_ex(&mut s, (0, 0, 0, 0), &opts, false);
        assert_eq!(rep.width, 2, "跳过列不计宽度");
        assert_eq!(s.get(0, 0).unwrap().value.display(), "00123", "文本列保留前导零");
        assert!(matches!(s.get(0, 0).unwrap().value, Value::Text(_)), "文本列是字符串");
        assert!(matches!(s.get(0, 1).unwrap().value, Value::Number(_)), "日期列是序列号");
        assert!(matches!(s.get(0, 2).map(|c| c.value.clone()), None | Some(Value::Empty)), "跳过列不写");
        assert_eq!(rep.written, 2);
    }

    #[test]
    fn col_modes_none_keeps_previous_behaviour() {
        let mut s = sheet_with("1,2");
        let opts = SplitOpts { delim: ',', ..Default::default() };
        let rep = text_to_columns_ex(&mut s, (0, 0, 0, 0), &opts, false);
        assert_eq!(rep.width, 2);
        assert!(matches!(s.get(0, 1).unwrap().value, Value::Number(_)), "默认仍识别数字");
    }
}
