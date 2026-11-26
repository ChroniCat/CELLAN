//! 数据工具：多级排序、删除重复、分列。
//!
//! 排序和去重先生成实际变更，再一次提交；删除重复保留第一次出现；
//! 分列把一格按分隔符拆到右边相邻的列，右边的原有内容会被推开。

use crate::model::{Cell, Sheet, Value};
use crate::sort_filter::{FxHashMap, parallel_stable_sort};
use crate::store::{ColSlab, GatheredColumn, GatheredSlab, NumberSlab, PAGE_ROWS, StyleColumn};
use crate::undo::CellDelta;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

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
    /// 按单元格填充色置顶/置底排序
    FillColor(Option<[u8; 3]>),
    /// 按字体颜色置顶/置底排序
    FontColor([u8; 3]),
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

/// Compact 16-byte `Copy` sort key backed by deduplicated `u32` text ranks.
/// Preserves exact `PreparedKey::compare` semantics (including `Bool` vs `Text` fallback to custom/text).
#[derive(Clone, Copy)]
struct CompactKey {
    /// 0 = Number, 1 = Bool, 2 = Text/Error, 3 = Empty
    kind: u8,
    /// `u32::MAX` when not in `SortMode::Custom`.
    custom: u32,
    /// For Number: `f64::to_bits()`.
    /// For Bool: `(bool_bit << 32) | (text_rank as u64)`.
    /// For Text/Error: `text_rank as u64`.
    payload: u64,
}

impl CompactKey {
    const KIND_NUM: u8 = 0;
    const KIND_BOOL: u8 = 1;
    const KIND_TEXT: u8 = 2;
    const KIND_EMPTY: u8 = 3;

    #[inline]
    fn compare(self, other: Self, ascending: bool) -> Ordering {
        if self.kind == Self::KIND_EMPTY || other.kind == Self::KIND_EMPTY {
            return (self.kind == Self::KIND_EMPTY).cmp(&(other.kind == Self::KIND_EMPTY));
        }
        let order = match (self.kind, other.kind) {
            (Self::KIND_NUM, Self::KIND_NUM) => f64::from_bits(self.payload)
                .partial_cmp(&f64::from_bits(other.payload))
                .unwrap_or(Ordering::Equal),
            (Self::KIND_NUM, _) => Ordering::Less,
            (_, Self::KIND_NUM) => Ordering::Greater,
            (Self::KIND_BOOL, Self::KIND_BOOL) => (self.payload >> 32).cmp(&(other.payload >> 32)),
            _ => self
                .custom
                .cmp(&other.custom)
                .then_with(|| (self.payload as u32).cmp(&(other.payload as u32))),
        };
        if ascending { order } else { order.reverse() }
    }
}

struct UniqueTextEntry {
    text: Box<[u8]>,
    strokes: Box<[Option<u16>]>,
    custom: u32,
}

impl UniqueTextEntry {
    fn cmp_text(&self, other: &Self) -> Ordering {
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

fn build_compact_keys_for_rows(
    sheet: &Sheet,
    rows: &[u32],
    spec: &SortKey,
    custom_ranks: &HashMap<String, usize>,
    key_cache: &mut crate::collate::SortKeyCache,
) -> Vec<CompactKey> {
    let mut keys = Vec::with_capacity(rows.len());
    let mut ptr_cache: FxHashMap<usize, u32> = FxHashMap::default();
    let mut str_dedup: FxHashMap<String, u32> = FxHashMap::default();
    let mut uniques: Vec<UniqueTextEntry> = Vec::new();
    let mut scratch = String::new();

    let mut intern_text = |raw: &str,
                           shared_ptr: Option<usize>,
                           key_cache: &mut crate::collate::SortKeyCache|
     -> (u32, u32) {
        if let Some(ptr) = shared_ptr
            && let Some(&uid) = ptr_cache.get(&ptr)
        {
            return (uid, uniques[uid as usize].custom);
        }
        let norm: &str = if !spec.ignore_case
            || (raw.is_ascii() && !raw.bytes().any(|b| b.is_ascii_uppercase()))
        {
            raw
        } else {
            scratch.clear();
            if raw.is_ascii() {
                scratch.extend(raw.bytes().map(|b| b.to_ascii_lowercase() as char));
            } else {
                scratch.push_str(&raw.to_lowercase());
            }
            &scratch
        };
        if let Some(&uid) = str_dedup.get(norm) {
            if let Some(ptr) = shared_ptr {
                ptr_cache.insert(ptr, uid);
            }
            return (uid, uniques[uid as usize].custom);
        }
        let strokes: Box<[Option<u16>]> = if matches!(spec.mode, SortMode::Stroke) {
            norm.chars().map(crate::collate::stroke_rank).collect()
        } else {
            Box::default()
        };
        let custom = if matches!(spec.mode, SortMode::Custom(_)) {
            custom_ranks
                .get(&norm.trim().to_ascii_lowercase())
                .map_or(u32::MAX, |&r| r as u32)
        } else {
            u32::MAX
        };
        let text: Box<[u8]> = if matches!(spec.mode, SortMode::Default) {
            norm.as_bytes().into()
        } else {
            key_cache.get_or_compute(norm).into()
        };
        let uid = uniques.len() as u32;
        uniques.push(UniqueTextEntry {
            text,
            strokes,
            custom,
        });
        str_dedup.insert(norm.to_owned(), uid);
        if let Some(ptr) = shared_ptr {
            ptr_cache.insert(ptr, uid);
        }
        (uid, custom)
    };

    for &r in rows {
        let v_cow = sheet.value_ref(r, spec.col);
        match v_cow.as_ref() {
            Value::Number(n) => keys.push(CompactKey {
                kind: CompactKey::KIND_NUM,
                custom: u32::MAX,
                payload: n.to_bits(),
            }),
            Value::Empty => keys.push(CompactKey {
                kind: CompactKey::KIND_EMPTY,
                custom: u32::MAX,
                payload: 0,
            }),
            Value::Text(t) => {
                let shared = (Arc::strong_count(t) > 1)
                    .then(|| Arc::as_ptr(t) as *const () as usize);
                let (uid, custom) = intern_text(t.as_ref(), shared, key_cache);
                keys.push(CompactKey {
                    kind: CompactKey::KIND_TEXT,
                    custom,
                    payload: uid as u64,
                });
            }
            Value::Bool(b) => {
                let disp = if *b { "TRUE" } else { "FALSE" };
                let (uid, custom) = intern_text(disp, None, key_cache);
                keys.push(CompactKey {
                    kind: CompactKey::KIND_BOOL,
                    custom,
                    payload: (u64::from(*b) << 32) | (uid as u64),
                });
            }
            Value::Error(e) => {
                let (uid, custom) = intern_text(e.text(), None, key_cache);
                keys.push(CompactKey {
                    kind: CompactKey::KIND_TEXT,
                    custom,
                    payload: uid as u64,
                });
            }
        }
    }

    if uniques.len() > 1 {
        let mut uids: Vec<u32> = (0..uniques.len() as u32).collect();
        let uref = uniques.as_slice();
        parallel_stable_sort(&mut uids, &|&a, &b| uref[a as usize].cmp_text(&uref[b as usize]));
        let mut dense_rank = vec![0u32; uniques.len()];
        let mut rank = 0u32;
        for i in 0..uids.len() {
            if i > 0
                && uref[uids[i - 1] as usize].cmp_text(&uref[uids[i] as usize]) != Ordering::Equal
            {
                rank += 1;
            }
            dense_rank[uids[i] as usize] = rank;
        }
        for k in &mut keys {
            match k.kind {
                CompactKey::KIND_TEXT => {
                    k.payload = dense_rank[k.payload as usize] as u64;
                }
                CompactKey::KIND_BOOL => {
                    let b_hi = k.payload & 0xffff_ffff_0000_0000;
                    let uid = (k.payload & 0xffff_ffff) as usize;
                    k.payload = b_hi | (dense_rank[uid] as u64);
                }
                _ => {}
            }
        }
    }
    keys
}

/// Ordered cells to change, suitable for one undo action. Every value is read
/// before mutation, including destinations that were empty before the operation.
pub type LinePermutation = ((u32,u32,u32,u32), bool, Vec<u32>);

#[derive(Debug, Default)]
pub struct RowChanges {
    pub permutation: Option<LinePermutation>,
    pub deltas: Vec<CellDelta>,
    pub notes: crate::undo::NoteUndoEdit,
    pub count: usize,
}

/// 规划排序/去重期间矩形区内批注（`notes`）与线程会话（`threads`）的坐标重排差异。
/// 仅在工作表存在批注或线程时扫描，空表零开销。
pub fn plan_reorder_notes(
    sheet: &Sheet,
    sheet_idx: usize,
    rect: (u32, u32, u32, u32),
    order: &[u32],
    horizontal: bool,
) -> crate::undo::NoteUndoEdit {
    let mut out = crate::undo::NoteUndoEdit::default();
    if sheet.notes.is_empty() && sheet.threads.is_empty() {
        return out;
    }
    let (r1, c1, r2, c2) = rect;
    if r1 > r2 || c1 > c2 {
        return out;
    }
    let (first, last) = if horizontal { (c1, c2) } else { (r1, r2) };
    let in_rect = |k: &crate::model::CellRef| {
        k.row >= r1 && k.row <= r2 && k.col >= c1 && k.col <= c2
    };
    let has_note_in_rect = sheet.notes.keys().any(in_rect);
    let has_thread_in_rect = sheet.threads.keys().any(in_rect);
    if !has_note_in_rect && !has_thread_in_rect {
        return out;
    }
    let mut inverse: HashMap<u32, u32> = HashMap::with_capacity(order.len());
    for (i, &src) in order.iter().enumerate() {
        let dst = first + i as u32;
        inverse.insert(src, dst);
    }
    let collect_coords = |keys: Vec<crate::model::CellRef>| -> Vec<(u32, u32)> {
        let mut coords = Vec::with_capacity(keys.len() * 2);
        for k in keys {
            coords.push((k.row, k.col));
            let line = if horizontal { k.col } else { k.row };
            if let Some(&dst_line) = inverse.get(&line) {
                if dst_line <= last {
                    let dst = if horizontal { (k.row, dst_line) } else { (dst_line, k.col) };
                    coords.push(dst);
                }
            }
        }
        coords.sort_unstable();
        coords.dedup();
        coords
    };
    if has_note_in_rect {
        let keys: Vec<_> = sheet.notes.keys().copied().filter(in_rect).collect();
        for (dst_r, dst_c) in collect_coords(keys) {
            let dst_key = crate::model::CellRef::new(dst_r, dst_c);
            let before = sheet.notes.get(&dst_key).cloned();
            let dst_line = if horizontal { dst_c } else { dst_r };
            let src_line = order.get((dst_line - first) as usize).copied();
            let after = src_line.and_then(|sl| {
                let src_key = if horizontal {
                    crate::model::CellRef::new(dst_r, sl)
                } else {
                    crate::model::CellRef::new(sl, dst_c)
                };
                sheet.notes.get(&src_key).cloned().map(|mut n| {
                    n.r = dst_r;
                    n.c = dst_c;
                    n
                })
            });
            if before != after {
                out.notes.push((sheet_idx, (dst_r, dst_c), before, after));
            }
        }
    }
    if has_thread_in_rect {
        let keys: Vec<_> = sheet.threads.keys().copied().filter(in_rect).collect();
        for (dst_r, dst_c) in collect_coords(keys) {
            let dst_key = crate::model::CellRef::new(dst_r, dst_c);
            let before = sheet.threads.get(&dst_key).cloned();
            let dst_line = if horizontal { dst_c } else { dst_r };
            let src_line = order.get((dst_line - first) as usize).copied();
            let after = src_line.and_then(|sl| {
                let src_key = if horizontal {
                    crate::model::CellRef::new(dst_r, sl)
                } else {
                    crate::model::CellRef::new(sl, dst_c)
                };
                sheet.threads.get(&src_key).cloned().map(|mut tc| {
                    tc.r = dst_r;
                    tc.c = dst_c;
                    tc
                })
            });
            if before != after {
                out.threads.push((sheet_idx, (dst_r, dst_c), before, after));
            }
        }
    }
    out
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
        let rows = (r2 - r1 + 1) as usize;
        let width = (c2 - c1 + 1) as usize;
        let area = width as u64 * rows as u64;
        if area <= 100_000 || area > sheet.len() as u64 { return Ok(None); }
        crate::sort_filter::validate_sort_region(sheet, region)?;

        // Fast O(pages * cols) bitmap & slab-kind pre-check: reject trailing blanks, DictText,
        // or formulas in microseconds BEFORE allocating any multi-megabyte row-major buffers!
        let p_lo = r1 / PAGE_ROWS;
        let p_hi = r2 / PAGE_ROWS;
        let cells = sheet.cells();
        for pi in p_lo..=p_hi {
            let Some(page) = cells.page(pi) else {
                return Ok(None);
            };
            let base = pi * PAGE_ROWS;
            let lo = r1.saturating_sub(base) as usize;
            let hi = (r2 - base).min(PAGE_ROWS - 1) as usize;
            let span_len = hi - lo + 1;
            for c in c1..=c2 {
                let Some(slab) = page.cols.get(c) else {
                    return Ok(None);
                };
                match slab.as_ref() {
                    ColSlab::Numbers(ns) => {
                        if ns.values.len() <= hi || !NumberSlab::all_present_in(&ns.present, lo, span_len) {
                            return Ok(None);
                        }
                    }
                    ColSlab::Values(vs) => {
                        if vs.is_dict_text()
                            || vs.len() <= hi
                            || !NumberSlab::all_present_in(&vs.present, lo, span_len)
                        {
                            return Ok(None);
                        }
                    }
                    ColSlab::Mixed(cs) => {
                        if cs.len() <= hi
                            || cs.has_formulas()
                            || !NumberSlab::all_present_in(&cs.row_mask(), lo, span_len)
                        {
                            return Ok(None);
                        }
                    }
                }
            }
        }

        let total = rows * width;
        let mut values = vec![0.0f64; total];
        for pi in p_lo..=p_hi {
            let page = cells.page(pi).unwrap();
            let base = pi * PAGE_ROWS;
            let lo = r1.saturating_sub(base) as usize;
            let hi = (r2 - base).min(PAGE_ROWS - 1) as usize;
            for c in c1..=c2 {
                let col_off = (c - c1) as usize;
                let slab = page.cols.get(c).unwrap();
                match slab.as_ref() {
                    ColSlab::Numbers(ns) => {
                        for (i, &n) in ns.values[lo..=hi].iter().enumerate() {
                            if !n.is_finite() {
                                return Ok(None);
                            }
                            let dst_row = (base as usize + lo + i) - r1 as usize;
                            values[dst_row * width + col_off] = n;
                        }
                    }
                    _ => {
                        for r_off in lo..=hi {
                            let Some(n) = slab.get_f64_plain(r_off as u32) else {
                                return Ok(None);
                            };
                            if !n.is_finite() {
                                return Ok(None);
                            }
                            let dst_row = (base as usize + r_off) - r1 as usize;
                            values[dst_row * width + col_off] = n;
                        }
                    }
                }
            }
        }

        let has_styles = !sheet.styles.is_empty() || !sheet.row_styles.is_empty() || !sheet.col_styles.is_empty();
        let mut styles = Vec::new();
        if has_styles {
            styles.resize(total, 0u16);
            for r in r1..=r2 {
                let row_off = (r - r1) as usize * width;
                for c in c1..=c2 {
                    styles[row_off + (c - c1) as usize] = sheet.style_of(r, c);
                }
            }
        }

        let key_col = (key - c1) as usize;
        let mut keys: Vec<f64> = Vec::with_capacity(rows);
        for r in 0..rows {
            keys.push(values[r * width + key_col]);
        }
        let mut order: Vec<u32> = (0..rows as u32).collect();
        let keys_ref = keys.as_slice();
        parallel_stable_sort(&mut order, &|&a, &b| {
            let left = keys_ref[a as usize];
            let right = keys_ref[b as usize];
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

    /// Parallel columnar gather directly into `sheet` (no per-cell `CellDelta` / `sheet.set` loop).
    /// Returns exact count of changed cells (matching `self.deltas().count()`).
    pub fn apply_to_sheet(&self, sheet: &mut Sheet) -> usize {
        let rows = self.order.len();
        if rows == 0 || self.width == 0 {
            return 0;
        }
        let r1 = self.first_row;
        let r2 = r1 + rows as u32 - 1;
        let c1 = self.first_col;
        let p_lo = r1 / PAGE_ROWS;
        let p_hi = r2 / PAGE_ROWS;
        let n_pages = (p_hi - p_lo + 1) as usize;
        let has_styles = !self.styles.is_empty();
        let width = self.width;

        struct NumericColResult {
            col_data: GatheredColumn,
            style_slabs: Option<Vec<Option<Arc<StyleColumn>>>>,
            style_edits: Vec<(u32, [u64; 4])>,
            changed: usize,
            style_changed: bool,
        }

        let build_col = |col_off: usize| -> NumericColResult {
            let col = c1 + col_off as u32;
            let old_slabs = sheet.cells().col_page_slices(col, p_lo, p_hi);
            let mut new_slabs: Vec<Option<GatheredSlab>> = Vec::with_capacity(n_pages);
            let mut style_slabs = has_styles.then(|| Vec::with_capacity(n_pages));
            let mut style_edits: Vec<(u32, [u64; 4])> = Vec::new();
            let mut occupied_masks: Vec<(u32, [u64; 4])> = Vec::new();
            let mut len_delta: isize = 0;
            let mut has_formula = false;
            let mut has_qual_formula = false;
            let mut changed = 0usize;
            let mut style_changed = false;
            let has_col_style = has_styles && sheet.col_styles.contains_key(&col);
            let track_cleared = !sheet.cleared.is_empty();

            for page_idx in 0..n_pages {
                let pi = p_lo + page_idx as u32;
                let base = pi * PAGE_ROWS;
                let lo = r1.saturating_sub(base) as usize;
                let hi = (r2 - base).min(PAGE_ROWS - 1) as usize;
                let old_slab = old_slabs[page_idx];
                let old_len = old_slab.map_or(0, ColSlab::len);
                let old_count = old_slab.map_or(0, ColSlab::occupied_count) as isize;
                let target_len = old_len.max(hi + 1);
                let push_mode = lo == 0 && hi + 1 >= old_len;
                let keep_numbers = push_mode || old_slab.is_none_or(ColSlab::is_numbers);

                let mut style_dense = [None::<u16>; PAGE_ROWS as usize];
                let mut st_edit_mask = [0u64; 4];
                let old_style_col = if has_styles {
                    sheet.styles.page_col(pi, col)
                } else {
                    None
                };
                if let Some(osc) = old_style_col {
                    for r in 0..lo {
                        style_dense[r] = osc.get(r as u32).copied();
                    }
                    for r in (hi + 1)..PAGE_ROWS as usize {
                        style_dense[r] = osc.get(r as u32).copied();
                    }
                }

                let mut occ_mask = [0u64; 4];
                if keep_numbers {
                    let mut out_vals = if push_mode {
                        Vec::with_capacity(target_len)
                    } else {
                        vec![0.0; target_len]
                    };
                    let mut present = [0u64; 4];
                    let mut new_count: isize = 0;
                    if !push_mode
                        && let Some(ColSlab::Numbers(ons)) = old_slab
                    {
                        for r in 0..lo.min(ons.values.len()) {
                            if ons.contains(r as u32) {
                                out_vals[r] = ons.values[r];
                                present[r / 64] |= 1u64 << (r % 64);
                                new_count += 1;
                            }
                        }
                        for r in (hi + 1)..ons.values.len() {
                            if ons.contains(r as u32) {
                                out_vals[r] = ons.values[r];
                                present[r / 64] |= 1u64 << (r % 64);
                                new_count += 1;
                            }
                        }
                    }

                    for r_off in lo..=hi {
                        let dst_r = base + r_off as u32;
                        let dst = (dst_r - r1) as usize;
                        let src = self.order[dst] as usize;
                        let a_idx = dst * width + col_off;
                        let b_idx = src * width + col_off;
                        let val = self.values[b_idx];
                        let val_diff = dst != src && self.values[a_idx] != val;
                        let mut st_diff = false;
                        if has_styles {
                            let before_st = self.styles[a_idx];
                            let after_st = self.styles[b_idx];
                            if dst != src && before_st != after_st {
                                st_diff = true;
                                style_changed = true;
                                st_edit_mask[r_off / 64] |= 1u64 << (r_off % 64);
                                let explicit = (after_st != 0
                                    || has_col_style
                                    || (!sheet.row_styles.is_empty()
                                        && sheet.row_styles.contains_key(&dst_r)))
                                .then_some(after_st);
                                style_dense[r_off] = explicit;
                            } else {
                                style_dense[r_off] =
                                    old_style_col.and_then(|osc| osc.get(r_off as u32).copied());
                            }
                        }
                        if val_diff || st_diff {
                            changed += 1;
                        }
                        new_count += 1;
                        present[r_off / 64] |= 1u64 << (r_off % 64);
                        if track_cleared {
                            occ_mask[r_off / 64] |= 1u64 << (r_off % 64);
                        }
                        if push_mode {
                            out_vals.push(val);
                        } else {
                            out_vals[r_off] = val;
                        }
                    }

                    len_delta += new_count - old_count;
                    if track_cleared && occ_mask != [0; 4] {
                        occupied_masks.push((pi, occ_mask));
                    }
                    new_slabs.push((present != [0; 4]).then(|| GatheredSlab {
                        cells: Arc::new(ColSlab::Numbers(NumberSlab {
                            present,
                            values: out_vals,
                        })),
                        row_mask: present,
                    }));
                } else {
                    let mut out_vec = vec![Cell::default(); target_len];
                    let mut new_count: isize = 0;
                    let mut row_mask = [0u64; 4];
                    if let Some(os) = old_slab {
                        for r in 0..lo.min(old_len) {
                            if let Some(v) = os.get_view(r as u32) {
                                if let Some(f) = v.formula() {
                                    has_formula = true;
                                    if f.contains('!') {
                                        has_qual_formula = true;
                                    }
                                }
                                new_count += 1;
                                row_mask[r / 64] |= 1u64 << (r % 64);
                                out_vec[r] = v.to_cell();
                            }
                        }
                        for r in (hi + 1)..old_len {
                            if let Some(v) = os.get_view(r as u32) {
                                if let Some(f) = v.formula() {
                                    has_formula = true;
                                    if f.contains('!') {
                                        has_qual_formula = true;
                                    }
                                }
                                new_count += 1;
                                row_mask[r / 64] |= 1u64 << (r % 64);
                                out_vec[r] = v.to_cell();
                            }
                        }
                    }
                    for r_off in lo..=hi {
                        let dst_r = base + r_off as u32;
                        let dst = (dst_r - r1) as usize;
                        let src = self.order[dst] as usize;
                        let a_idx = dst * width + col_off;
                        let b_idx = src * width + col_off;
                        let val = self.values[b_idx];
                        let val_diff = dst != src && self.values[a_idx] != val;
                        let mut st_diff = false;
                        if has_styles {
                            let before_st = self.styles[a_idx];
                            let after_st = self.styles[b_idx];
                            if dst != src && before_st != after_st {
                                st_diff = true;
                                style_changed = true;
                                st_edit_mask[r_off / 64] |= 1u64 << (r_off % 64);
                                let explicit = (after_st != 0
                                    || has_col_style
                                    || (!sheet.row_styles.is_empty()
                                        && sheet.row_styles.contains_key(&dst_r)))
                                .then_some(after_st);
                                style_dense[r_off] = explicit;
                            } else {
                                style_dense[r_off] =
                                    old_style_col.and_then(|osc| osc.get(r_off as u32).copied());
                            }
                        }
                        if val_diff || st_diff {
                            changed += 1;
                        }
                        new_count += 1;
                        row_mask[r_off / 64] |= 1u64 << (r_off % 64);
                        if track_cleared {
                            occ_mask[r_off / 64] |= 1u64 << (r_off % 64);
                        }
                        out_vec[r_off] = Cell {
                            value: Value::Number(val),
                            formula: None,
                        };
                    }
                    while out_vec.last().is_some_and(Cell::is_blank) {
                        out_vec.pop();
                    }
                    len_delta += new_count - old_count;
                    if track_cleared && occ_mask != [0; 4] {
                        occupied_masks.push((pi, occ_mask));
                    }
                    let slab = if has_formula {
                        ColSlab::Mixed(out_vec.into())
                    } else {
                        ColSlab::Values(out_vec.into_iter().map(|c| c.value).collect())
                    };
                    new_slabs.push((!slab.is_empty()).then(|| GatheredSlab {
                        cells: Arc::new(slab),
                        row_mask,
                    }));
                }

                if let Some(ref mut slabs) = style_slabs {
                    slabs.push(StyleColumn::from_dense(&style_dense));
                    if st_edit_mask != [0; 4] {
                        style_edits.push((pi, st_edit_mask));
                    }
                }
            }

            NumericColResult {
                col_data: GatheredColumn {
                    col,
                    slabs: new_slabs,
                    len_delta,
                    has_formula,
                    has_qual_formula,
                    vacated_masks: Vec::new(),
                    occupied_masks,
                },
                style_slabs,
                style_edits,
                changed,
                style_changed,
            }
        };

        let area = rows as u64 * width as u64;
        let results: Vec<NumericColResult> = if area >= 16_384 && width >= 2 {
            let n_threads = std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
                .min(8)
                .min(width);
            let col_indices: Vec<usize> = (0..width).collect();
            let chunk_size = width.div_ceil(n_threads);
            let mut all = Vec::with_capacity(width);
            std::thread::scope(|s| {
                let mut handles = Vec::with_capacity(n_threads);
                for chunk in col_indices.chunks(chunk_size) {
                    let f = &build_col;
                    handles.push(s.spawn(move || {
                        chunk.iter().map(|&c| f(c)).collect::<Vec<_>>()
                    }));
                }
                for h in handles {
                    if let Ok(local) = h.join() {
                        all.extend(local);
                    }
                }
            });
            all
        } else {
            (0..width).map(build_col).collect()
        };

        let mut total_changed = 0usize;
        let mut any_style_changed = false;
        let mut gathered = Vec::with_capacity(results.len());
        let mut style_updates = Vec::new();
        for res in results {
            total_changed += res.changed;
            any_style_changed |= res.style_changed;
            let col = res.col_data.col;
            gathered.push(res.col_data);
            if let Some(slabs) = res.style_slabs {
                style_updates.push((col, slabs, res.style_edits));
            }
        }
        let (cells, cleared) = sheet.cells_and_cleared_mut();
        cells.install_gathered_columns(p_lo, p_hi, gathered, cleared);
        if any_style_changed {
            for (col, slabs, edits) in style_updates {
                sheet.styles.install_col_pages(col, p_lo, slabs);
                for (pi, mask) in edits {
                    sheet.style_edits.or_page_mask(pi, col, mask);
                }
            }
            sheet.bump_display_style_revision();
            if !sheet.loading {
                sheet.styles_dirty = true;
            }
        }
        if !sheet.notes.is_empty() || !sheet.threads.is_empty() {
            let abs_order: Vec<u32> = self.order.iter().map(|&line| self.first_row + line).collect();
            let rect = (
                self.first_row,
                self.first_col,
                self.first_row + self.order.len() as u32 - 1,
                self.first_col + self.width as u32 - 1,
            );
            let note_edit = plan_reorder_notes(sheet, self.sheet, rect, &abs_order, false);
            if !note_edit.is_empty() {
                note_edit.apply_on_sheet(sheet, true);
            }
        }
        if total_changed > 0 && !sheet.loading {
            sheet.edited = true;
        }
        total_changed
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
            + source.styles.payload_bytes()
            + (source.row_styles.capacity() + source.col_styles.capacity()) * size_of::<(u32,u16)>()
            + source.cells().total_cell_heap_bytes();
        Self::Mixed { source, index, region, order, horizontal, bytes }
    }

    /// Parallel columnar gather directly into `sheet` (no per-cell `CellDelta` / `sheet.set` loop).
    /// Returns exact count of changed cells (value/formula OR style).
    pub fn apply_to_sheet(&self, sheet: &mut Sheet) -> usize {
        match self {
            Self::Numeric(n) => n.apply_to_sheet(sheet),
            Self::Mixed {
                source,
                region,
                order,
                horizontal,
                ..
            } => {
                let (r1, c1, r2, c2) = *region;
                if r1 > r2 || c1 > c2 || order.is_empty() {
                    return 0;
                }
                if *horizontal {
                    let changed = apply_horizontal_gather(source, sheet, *region, order);
                    if !sheet.notes.is_empty() || !sheet.threads.is_empty() {
                        let note_edit = plan_reorder_notes(sheet, self.sheet(), *region, order, true);
                        if !note_edit.is_empty() {
                            note_edit.apply_on_sheet(sheet, true);
                        }
                    }
                    return changed;
                }
                let (cells, cleared) = sheet.cells_and_cleared_mut();
                let cell_changed = cells.gather_rows(
                    source.cells(),
                    *region,
                    order,
                    &source.name,
                    cleared,
                );
                let has_styles = !source.styles.is_empty()
                    || !source.row_styles.is_empty()
                    || !source.col_styles.is_empty()
                    || !sheet.styles.is_empty();
                let mut style_only_changed = 0usize;
                if has_styles {
                    let p_lo = r1 / PAGE_ROWS;
                    let p_hi = r2 / PAGE_ROWS;
                    let n_pages = (p_hi - p_lo + 1) as usize;
                    let cols: Vec<u32> = (c1..=c2).collect();

                    let build_style_col = |col: u32| -> (u32, Vec<Option<Arc<StyleColumn>>>, Vec<(u32, [u64; 4])>, usize, bool) {
                        let mut slabs = Vec::with_capacity(n_pages);
                        let mut edits = Vec::new();
                        let mut extra_changed = 0usize;
                        let mut col_changed = false;
                        let has_col_style = sheet.col_styles.contains_key(&col);
                        let has_row_styles = !sheet.row_styles.is_empty();

                        for page_idx in 0..n_pages {
                            let pi = p_lo + page_idx as u32;
                            let base = pi * PAGE_ROWS;
                            let lo = r1.saturating_sub(base) as usize;
                            let hi = (r2 - base).min(PAGE_ROWS - 1) as usize;
                            let mut dense = [None::<u16>; PAGE_ROWS as usize];
                            let old_sc = sheet.styles.page_col(pi, col);
                            if let Some(osc) = old_sc {
                                for r in 0..lo {
                                    dense[r] = osc.get(r as u32).copied();
                                }
                                for r in (hi + 1)..PAGE_ROWS as usize {
                                    dense[r] = osc.get(r as u32).copied();
                                }
                            }
                            let mut edit_mask = [0u64; 4];
                            for r_off in lo..=hi {
                                let dst_r = base + r_off as u32;
                                let src_r = order[(dst_r - r1) as usize];
                                if dst_r == src_r {
                                    dense[r_off] =
                                        old_sc.and_then(|osc| osc.get(r_off as u32).copied());
                                    continue;
                                }
                                let before_st = source.style_of(dst_r, col);
                                let after_st = source.style_of(src_r, col);
                                if before_st != after_st {
                                    col_changed = true;
                                    edit_mask[r_off / 64] |= 1u64 << (r_off % 64);
                                    let explicit = (after_st != 0
                                        || has_col_style
                                        || (has_row_styles && sheet.row_styles.contains_key(&dst_r)))
                                    .then_some(after_st);
                                    dense[r_off] = explicit;
                                    if source.get(dst_r, col) == sheet.get(dst_r, col) {
                                        extra_changed += 1;
                                    }
                                } else {
                                    dense[r_off] =
                                        old_sc.and_then(|osc| osc.get(r_off as u32).copied());
                                }
                            }
                            slabs.push(StyleColumn::from_dense(&dense));
                            if edit_mask != [0; 4] {
                                edits.push((pi, edit_mask));
                            }
                        }
                        (col, slabs, edits, extra_changed, col_changed)
                    };

                    let area = u64::from(r2 - r1 + 1) * cols.len() as u64;
                    let results: Vec<_> = if area >= 16_384 && cols.len() >= 2 {
                        let n_threads = std::thread::available_parallelism()
                            .map(|n| n.get())
                            .unwrap_or(4)
                            .min(8)
                            .min(cols.len());
                        let chunk_size = cols.len().div_ceil(n_threads);
                        let mut all = Vec::with_capacity(cols.len());
                        std::thread::scope(|s| {
                            let mut handles = Vec::with_capacity(n_threads);
                            for chunk in cols.chunks(chunk_size) {
                                let f = &build_style_col;
                                handles.push(s.spawn(move || {
                                    chunk.iter().map(|&c| f(c)).collect::<Vec<_>>()
                                }));
                            }
                            for h in handles {
                                if let Ok(local) = h.join() {
                                    all.extend(local);
                                }
                            }
                        });
                        all
                    } else {
                        cols.iter().map(|&c| build_style_col(c)).collect()
                    };

                    let mut any_style_changed = false;
                    for (col, slabs, edits, extra, changed) in results {
                        style_only_changed += extra;
                        if changed {
                            any_style_changed = true;
                            sheet.styles.install_col_pages(col, p_lo, slabs);
                            for (pi, mask) in edits {
                                sheet.style_edits.or_page_mask(pi, col, mask);
                            }
                        }
                    }
                    if any_style_changed {
                        sheet.bump_display_style_revision();
                        if !sheet.loading {
                            sheet.styles_dirty = true;
                        }
                    }
                }
                if !sheet.notes.is_empty() || !sheet.threads.is_empty() {
                    let note_edit = plan_reorder_notes(sheet, self.sheet(), *region, order, false);
                    if !note_edit.is_empty() {
                        note_edit.apply_on_sheet(sheet, true);
                    }
                }
                let total = cell_changed + style_only_changed;
                if total > 0 && !sheet.loading {
                    sheet.edited = true;
                }
                total
            }
        }
    }

    /// Apply via parallel columnar gather AND collect up to `max_sample` leading `CellDelta`s
    /// for revision audit logs without materializing millions of deltas.
    pub fn apply_to_sheet_with_sample(
        &self,
        sheet: &mut Sheet,
        max_sample: usize,
    ) -> (usize, Vec<CellDelta>) {
        let sample: Vec<CellDelta> = if max_sample == 0 {
            Vec::new()
        } else {
            self.deltas().take(max_sample).collect()
        };
        let changed = self.apply_to_sheet(sheet);
        (changed, sample)
    }

    /// 规划本次大表排序涉及的批注/线程重排差异（供撤销栈与协作广播复用）。
    pub fn plan_notes(&self, sheet: &Sheet) -> crate::undo::NoteUndoEdit {
        let (region, horizontal, order) = self.permutation();
        plan_reorder_notes(sheet, self.sheet(), region, &order, horizontal)
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
                let has_styles = !source.styles.is_empty()
                    || !source.row_styles.is_empty()
                    || !source.col_styles.is_empty();
                let has_formulas = source.cells().has_formulas();
                let cache = std::rc::Rc::new(std::cell::RefCell::new(crate::refs::FillFormulas::default()));
                Box::new(order.iter().copied().enumerate().filter(move |&(dst,src)| first+dst as u32 != src)
                    .flat_map(move |(dst,src)| {
                        let cache = cache.clone();
                        (start..=end).filter_map(move |offset| {
                            if !has_styles && !has_formulas {
                                let (row, col) = if *horizontal { (offset, first + dst as u32) } else { (first + dst as u32, offset) };
                                let (sr, sc) = if *horizontal { (offset, src) } else { (src, offset) };
                                let before = source.get(row, col);
                                let after = source.get(sr, sc);
                                if before == after {
                                    return None;
                                }
                                return Some(CellDelta {
                                    sheet: *index,
                                    row,
                                    col,
                                    before: before.map(|c| c.into_owned()).unwrap_or_default(),
                                    after: after.map(|c| c.into_owned()).unwrap_or_default(),
                                    before_style: 0,
                                    after_style: 0,
                                });
                            }
                            let mut fill = cache.borrow_mut();
                            reorder_delta_cached(source,*index,first+dst as u32,Some(src),offset,*horizontal,false,&mut fill)
                        })
                    }))
            }
        }
    }
}

fn apply_horizontal_gather(
    source: &Sheet,
    sheet: &mut Sheet,
    region: (u32, u32, u32, u32),
    order: &[u32],
) -> usize {
    let (r1, c1, r2, c2) = region;
    let p_lo = r1 / PAGE_ROWS;
    let p_hi = r2 / PAGE_ROWS;
    let n_pages = (p_hi - p_lo + 1) as usize;
    let has_styles = !source.styles.is_empty()
        || !source.row_styles.is_empty()
        || !source.col_styles.is_empty()
        || !sheet.styles.is_empty();
    let cols: Vec<u32> = (c1..=c2).collect();

    let build_col = |dst_c: u32| -> (GatheredColumn, Option<Vec<Option<Arc<StyleColumn>>>>, Vec<(u32, [u64; 4])>, usize, bool) {
        let src_c = order[(dst_c - c1) as usize];
        let src_slabs = source.cells().col_page_slices(src_c, p_lo, p_hi);
        let old_slabs = sheet.cells().col_page_slices(dst_c, p_lo, p_hi);
        let mut fill_cache = crate::refs::FillFormulas::default();
        let mut new_slabs = Vec::with_capacity(n_pages);
        let mut style_slabs = has_styles.then(|| Vec::with_capacity(n_pages));
        let mut style_edits = Vec::new();
        let mut vacated_masks = Vec::new();
        let mut occupied_masks = Vec::new();
        let mut len_delta: isize = 0;
        let mut has_formula = false;
        let mut has_qual_formula = false;
        let mut changed = 0usize;
        let mut style_changed = false;
        let has_col_style = has_styles && sheet.col_styles.contains_key(&dst_c);
        let has_row_styles = has_styles && !sheet.row_styles.is_empty();
        let dc = i64::from(dst_c) - i64::from(src_c);

        for page_idx in 0..n_pages {
            let pi = p_lo + page_idx as u32;
            let base = pi * PAGE_ROWS;
            let lo = r1.saturating_sub(base) as usize;
            let hi = (r2 - base).min(PAGE_ROWS - 1) as usize;
            let old_slab = old_slabs[page_idx];
            let src_slab = src_slabs[page_idx];
            let old_len = old_slab.map_or(0, ColSlab::len);
            let old_count = old_slab.map_or(0, ColSlab::occupied_count) as isize;

            let target_len = old_len.max(hi + 1);
            let mut out_vec = vec![Cell::default(); target_len];
            let mut new_count: isize = 0;
            let mut row_mask = [0u64; 4];
            let mut slab_has_formula = false;
            let mut slab_all_numbers = true;
            if let Some(os) = old_slab {
                for r in 0..lo.min(old_len) {
                    if let Some(v) = os.get_view(r as u32) {
                        if let Some(f) = v.formula() {
                            has_formula = true;
                            slab_has_formula = true;
                            slab_all_numbers = false;
                            if f.contains('!') {
                                has_qual_formula = true;
                            }
                        } else if v.as_f64().is_none() {
                            slab_all_numbers = false;
                        }
                        new_count += 1;
                        row_mask[r / 64] |= 1u64 << (r % 64);
                        out_vec[r] = v.to_cell();
                    }
                }
                for r in (hi + 1)..old_len {
                    if let Some(v) = os.get_view(r as u32) {
                        if let Some(f) = v.formula() {
                            has_formula = true;
                            slab_has_formula = true;
                            slab_all_numbers = false;
                            if f.contains('!') {
                                has_qual_formula = true;
                            }
                        } else if v.as_f64().is_none() {
                            slab_all_numbers = false;
                        }
                        new_count += 1;
                        row_mask[r / 64] |= 1u64 << (r % 64);
                        out_vec[r] = v.to_cell();
                    }
                }
            }

            let mut vac_mask = [0u64; 4];
            let mut occ_mask = [0u64; 4];
            let mut dense_st = [None::<u16>; PAGE_ROWS as usize];
            let mut st_edit_mask = [0u64; 4];
            let old_sc = if has_styles { sheet.styles.page_col(pi, dst_c) } else { None };
            if let Some(osc) = old_sc {
                for r in 0..lo {
                    dense_st[r] = osc.get(r as u32).copied();
                }
                for r in (hi + 1)..PAGE_ROWS as usize {
                    dense_st[r] = osc.get(r as u32).copied();
                }
            }

            for r_off in lo..=hi {
                let row = base + r_off as u32;
                let src_view = src_slab.and_then(|s| s.get_view(r_off as u32));
                let old_view = old_slab.and_then(|s| s.get_view(r_off as u32));
                let mut cell_diff = false;
                if let Some(sv) = src_view {
                    let mut cell = sv.to_cell();
                    if dc != 0
                        && let Some(f) = cell.formula.as_deref()
                    {
                        let nf: Arc<str> = if !f.contains('!') {
                            fill_cache.translate(f, 0, dc).into()
                        } else {
                            crate::refs::translate(f, 0, dc, &source.name).into()
                        };
                        cell.formula = Some(nf);
                    }
                    if old_view.is_none_or(|ov| !ov.eq_cell(&cell)) {
                        cell_diff = true;
                    }
                    if let Some(f) = cell.formula.as_deref() {
                        has_formula = true;
                        slab_has_formula = true;
                        slab_all_numbers = false;
                        if f.contains('!') {
                            has_qual_formula = true;
                        }
                    } else if !matches!(cell.value, Value::Number(_)) {
                        slab_all_numbers = false;
                    }
                    new_count += 1;
                    row_mask[r_off / 64] |= 1u64 << (r_off % 64);
                    occ_mask[r_off / 64] |= 1u64 << (r_off % 64);
                    out_vec[r_off] = cell;
                } else if old_view.is_some() {
                    cell_diff = true;
                    vac_mask[r_off / 64] |= 1u64 << (r_off % 64);
                }

                let mut st_diff = false;
                if has_styles {
                    if dc != 0 {
                        let before_st = source.style_of(row, dst_c);
                        let after_st = source.style_of(row, src_c);
                        if before_st != after_st {
                            st_diff = true;
                            style_changed = true;
                            st_edit_mask[r_off / 64] |= 1u64 << (r_off % 64);
                            let explicit = (after_st != 0
                                || has_col_style
                                || (has_row_styles && sheet.row_styles.contains_key(&row)))
                            .then_some(after_st);
                            dense_st[r_off] = explicit;
                        } else {
                            dense_st[r_off] = old_sc.and_then(|osc| osc.get(r_off as u32).copied());
                        }
                    } else {
                        dense_st[r_off] = old_sc.and_then(|osc| osc.get(r_off as u32).copied());
                    }
                }
                if cell_diff || st_diff {
                    changed += 1;
                }
            }

            while out_vec.last().is_some_and(Cell::is_blank) {
                out_vec.pop();
            }
            len_delta += new_count - old_count;
            if vac_mask != [0; 4] {
                vacated_masks.push((pi, vac_mask));
            }
            if occ_mask != [0; 4] {
                occupied_masks.push((pi, occ_mask));
            }
            let slab = if out_vec.is_empty() {
                None
            } else if slab_has_formula {
                Some(ColSlab::Mixed(out_vec.into()))
            } else if slab_all_numbers {
                let mut vals = vec![0.0; out_vec.len()];
                for (r, c) in out_vec.iter().enumerate() {
                    if let Value::Number(n) = c.value {
                        vals[r] = n;
                    }
                }
                Some(ColSlab::Numbers(NumberSlab {
                    present: row_mask,
                    values: vals,
                }))
            } else {
                Some(ColSlab::Values(out_vec.into_iter().map(|c| c.value).collect()))
            };
            new_slabs.push(slab.map(|s| GatheredSlab {
                cells: Arc::new(s),
                row_mask,
            }));
            if let Some(ref mut slabs) = style_slabs {
                slabs.push(StyleColumn::from_dense(&dense_st));
                if st_edit_mask != [0; 4] {
                    style_edits.push((pi, st_edit_mask));
                }
            }
        }

        (
            GatheredColumn {
                col: dst_c,
                slabs: new_slabs,
                len_delta,
                has_formula,
                has_qual_formula,
                vacated_masks,
                occupied_masks,
            },
            style_slabs,
            style_edits,
            changed,
            style_changed,
        )
    };

    let area = u64::from(r2 - r1 + 1) * cols.len() as u64;
    let results: Vec<_> = if area >= 16_384 && cols.len() >= 2 {
        let n_threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .min(8)
            .min(cols.len());
        let chunk_size = cols.len().div_ceil(n_threads);
        let mut all = Vec::with_capacity(cols.len());
        std::thread::scope(|s| {
            let mut handles = Vec::with_capacity(n_threads);
            for chunk in cols.chunks(chunk_size) {
                let f = &build_col;
                handles.push(s.spawn(move || chunk.iter().map(|&c| f(c)).collect::<Vec<_>>()));
            }
            for h in handles {
                if let Ok(local) = h.join() {
                    all.extend(local);
                }
            }
        });
        all
    } else {
        cols.iter().map(|&c| build_col(c)).collect()
    };

    let mut total_changed = 0usize;
    let mut any_style_changed = false;
    let mut gathered = Vec::with_capacity(results.len());
    let mut style_updates = Vec::new();
    for (gc, st_slabs, st_edits, ch, st_ch) in results {
        total_changed += ch;
        let col = gc.col;
        gathered.push(gc);
        if st_ch
            && let Some(slabs) = st_slabs
        {
            any_style_changed = true;
            style_updates.push((col, slabs, st_edits));
        }
    }
    let (cells, cleared) = sheet.cells_and_cleared_mut();
    cells.install_gathered_columns(p_lo, p_hi, gathered, cleared);
    if any_style_changed {
        for (col, slabs, edits) in style_updates {
            sheet.styles.install_col_pages(col, p_lo, slabs);
            for (pi, mask) in edits {
                sheet.style_edits.or_page_mask(pi, col, mask);
            }
        }
        sheet.bump_display_style_revision();
        if !sheet.loading {
            sheet.styles_dirty = true;
        }
    }
    if total_changed > 0 && !sheet.loading {
        sheet.edited = true;
    }
    total_changed
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
        .as_deref()
        .and_then(|cell| cell.formula.as_deref())
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
    let mut after = source.map(|c| c.into_owned()).unwrap_or_default();
    if let Some(f) = translated {
        after.formula = Some(f);
    }
    let before = before.map(|c| c.into_owned()).unwrap_or_default();
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
        if !self.notes.is_empty() {
            self.notes.apply_on_sheet(sheet, true);
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
    changes.notes = plan_reorder_notes(sheet, sheet_idx, rect, &order, horizontal);
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
    plan_sort_multi_with_styles(sheet, None, index, region, keys, header, false)
}

/// 完整多级排序规划：支持填充色/字体色排序、区分大小写、中文拼音/笔画/自定义序列、以及按行（水平）多级排序！
pub fn plan_sort_multi_with_styles(
    sheet: &Sheet,
    styles: Option<&crate::style::StyleTable>,
    index: usize,
    region: (u32, u32, u32, u32),
    keys: &[SortKey],
    header: bool,
    horizontal: bool,
) -> Result<RowChanges, String> {
    let needs_color_or_horiz = horizontal
        || keys.iter().any(|k| matches!(k.mode, SortMode::FillColor(_) | SortMode::FontColor(_)));
    if !needs_color_or_horiz {
        return Ok(multi_sort_order(sheet, region, keys, header)?.map_or_else(
            RowChanges::default,
            |(rect, horiz, order)| reorder_lines(sheet, index, rect, order, horiz, false),
        ));
    }
    let (mut r1, mut c1, r2, c2) = region;
    if horizontal {
        if header {
            c1 = c1.saturating_add(1);
        }
        if c2 <= c1 || r2 < r1 || keys.is_empty() {
            return Ok(RowChanges::default());
        }
        crate::sort_filter::validate_sort_region(sheet, (r1, c1, r2, c2))?;
        let active: Vec<&SortKey> = keys.iter().filter(|k| (r1..=r2).contains(&k.col)).collect();
        if active.is_empty() {
            return Ok(RowChanges::default());
        }
        let mut order: Vec<u32> = (c1..=c2).collect();
        let mut key_cache = crate::collate::SortKeyCache::new();
        // 为每列构建多级键
        let col_keys: Vec<Vec<(u8, u32, u64, String)>> = order
            .iter()
            .map(|&col| {
                active
                    .iter()
                    .map(|k| eval_cell_sort_tuple(sheet, styles, k.col, col, k, &mut key_cache))
                    .collect()
            })
            .collect();
        let mut idxs: Vec<usize> = (0..order.len()).collect();
        idxs.sort_by(|&a, &b| {
            for (ki, k) in active.iter().enumerate() {
                let ord = cmp_sort_tuple(&col_keys[a][ki], &col_keys[b][ki], k.asc);
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            Ordering::Equal
        });
        order = idxs.into_iter().map(|i| order[i]).collect();
        return Ok(reorder_lines(sheet, index, (r1, c1, r2, c2), order, true, false));
    }
    // 垂直排序（含颜色排序键）
    if header {
        r1 = r1.saturating_add(1);
    }
    if r2 <= r1 || c2 < c1 || keys.is_empty() {
        return Ok(RowChanges::default());
    }
    crate::sort_filter::validate_sort_region(sheet, (r1, c1, r2, c2))?;
    let active: Vec<&SortKey> = keys.iter().filter(|k| (c1..=c2).contains(&k.col)).collect();
    if active.is_empty() {
        return Ok(RowChanges::default());
    }
    let mut order: Vec<u32> = (r1..=r2).collect();
    let mut key_cache = crate::collate::SortKeyCache::new();
    let row_keys: Vec<Vec<(u8, u32, u64, String)>> = order
        .iter()
        .map(|&row| {
            active
                .iter()
                .map(|k| eval_cell_sort_tuple(sheet, styles, row, k.col, k, &mut key_cache))
                .collect()
        })
        .collect();
    let mut idxs: Vec<usize> = (0..order.len()).collect();
    idxs.sort_by(|&a, &b| {
        for (ki, k) in active.iter().enumerate() {
            let ord = cmp_sort_tuple(&row_keys[a][ki], &row_keys[b][ki], k.asc);
            if ord != Ordering::Equal {
                return ord;
            }
        }
        Ordering::Equal
    });
    order = idxs.into_iter().map(|i| order[i]).collect();
    Ok(reorder_lines(sheet, index, (r1, c1, r2, c2), order, false, false))
}

/// 返回 `(kind: 0=color_match/num, 1=bool, 2=text, 3=empty/color_miss, custom_rank, num_bits, collate_hex)`
fn eval_cell_sort_tuple(
    sheet: &Sheet,
    styles: Option<&crate::style::StyleTable>,
    r: u32,
    c: u32,
    spec: &SortKey,
    key_cache: &mut crate::collate::SortKeyCache,
) -> (u8, u32, u64, String) {
    match &spec.mode {
        SortMode::FillColor(target) => {
            let hit = styles.is_some_and(|st| st.get(sheet.style_of(r, c)).fill == *target);
            // 置顶(asc=true): 命中=0, 未命中=1；置底(asc=false): 反转时未命中在前
            return (0, if hit { 0 } else { 1 }, 0, String::new());
        }
        SortMode::FontColor(target) => {
            let hit = styles.is_some_and(|st| st.get(sheet.style_of(r, c)).color == *target);
            return (0, if hit { 0 } else { 1 }, 0, String::new());
        }
        _ => {}
    }
    let v = sheet.value_ref(r, c);
    match v.as_ref() {
        Value::Empty => (3, u32::MAX, 0, String::new()),
        Value::Number(n) => (0, u32::MAX, n.to_bits(), String::new()),
        Value::Bool(b) => (1, u32::MAX, u64::from(*b), String::new()),
        Value::Text(t) => {
            let norm = if spec.ignore_case { t.to_lowercase() } else { t.to_string() };
            let custom = if let SortMode::Custom(list) = &spec.mode {
                list.iter()
                    .position(|x| x.trim().eq_ignore_ascii_case(norm.trim()))
                    .map_or(u32::MAX, |i| i as u32)
            } else {
                u32::MAX
            };
            let s = match &spec.mode {
                SortMode::Default => norm,
                SortMode::Stroke => {
                    let mut out = String::new();
                    for ch in norm.chars() {
                        if let Some(rk) = crate::collate::stroke_rank(ch) {
                            out.push_str(&format!("{rk:05x}"));
                        } else {
                            out.push_str("fffff");
                        }
                    }
                    out
                }
                _ => {
                    let bytes = key_cache.get_or_compute(&norm);
                    bytes.iter().map(|b| format!("{b:02x}")).collect()
                }
            };
            (2, custom, 0, s)
        }
        Value::Error(e) => (2, u32::MAX, 0, e.text().to_string()),
    }
}

fn cmp_sort_tuple(
    a: &(u8, u32, u64, String),
    b: &(u8, u32, u64, String),
    asc: bool,
) -> Ordering {
    if a.0 == 3 || b.0 == 3 {
        return (a.0 == 3).cmp(&(b.0 == 3));
    }
    let ord = a.0.cmp(&b.0).then_with(|| match a.0 {
        0 if a.1 != u32::MAX || b.1 != u32::MAX => a.1.cmp(&b.1),
        0 => f64::from_bits(a.2)
            .partial_cmp(&f64::from_bits(b.2))
            .unwrap_or(Ordering::Equal),
        1 => a.2.cmp(&b.2),
        _ => a.1.cmp(&b.1).then_with(|| a.3.cmp(&b.3)),
    });
    if asc { ord } else { ord.reverse() }
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
            let prepared = build_compact_keys_for_rows(sheet, &order, key, &ranks, &mut key_cache);
            let mut indexed: Vec<(CompactKey, u32)> = prepared
                .into_iter()
                .zip(order.iter().copied())
                .collect();
            let asc = key.asc;
            parallel_stable_sort(&mut indexed, &|a, b| a.0.compare(b.0, asc));
            let mut i = 0usize;
            while i < indexed.len() {
                let mut j = i + 1;
                while j < indexed.len() && indexed[i].0.compare(indexed[j].0, asc) == Ordering::Equal
                {
                    j += 1;
                }
                if j - i > 1 {
                    next_ties.push(i..j);
                }
                i = j;
            }
            for (dst, (_, row_id)) in order.iter_mut().zip(indexed) {
                *dst = row_id;
            }
        } else {
            for span in ties {
                let slice = &order[span.clone()];
                let prepared = build_compact_keys_for_rows(sheet, slice, key, &ranks, &mut key_cache);
                let mut local: Vec<(CompactKey, u32)> = prepared
                    .into_iter()
                    .zip(slice.iter().copied())
                    .collect();
                let asc = key.asc;
                parallel_stable_sort(&mut local, &|a, b| a.0.compare(b.0, asc));
                let mut i = 0usize;
                while i < local.len() {
                    let mut j = i + 1;
                    while j < local.len() && local[i].0.compare(local[j].0, asc) == Ordering::Equal {
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
        Value::Number(n) => crate::model::with_formatted_number(*n, |text| {
            if text.is_empty() {
                h
            } else {
                let mut h = fnv_mix(h, &col.to_le_bytes());
                h = fnv_mix(h, &(text.len() as u64).to_le_bytes());
                fnv_mix(h, text.as_bytes())
            }
        }),
    }
}

#[inline]
fn values_same_display(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Empty, Value::Empty) => true,
        (Value::Empty, Value::Text(t)) | (Value::Text(t), Value::Empty) => t.is_empty(),
        (Value::Text(x), Value::Text(y)) => Arc::ptr_eq(x, y) || x == y,
        (Value::Number(x), Value::Number(y)) if x == y => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Error(x), Value::Error(y)) => x == y,
        _ => a.with_display_str(|sa| b.with_display_str(|sb| sa == sb)),
    }
}

fn rows_match_on_cols(sheet: &Sheet, r_a: u32, r_b: u32, cols: &[u32]) -> bool {
    cols.iter()
        .all(|&c| values_same_display(sheet.value_ref(r_a, c).as_ref(), sheet.value_ref(r_b, c).as_ref()))
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
    let mut buckets: FxHashMap<u64, Vec<u32>> = FxHashMap::default();
    let mut keep = Vec::new();
    let area = (r2 - r1 + 1) as u64 * use_cols.len() as u64;
    if area > 4096 && area > sheet.len() as u64 * 4 {
        let selected: HashSet<u32> = use_cols.iter().copied().collect();
        let min_sel = use_cols.iter().copied().min().unwrap_or(c1);
        let max_sel = use_cols.iter().copied().max().unwrap_or(c2);
        let mut row_hashes: FxHashMap<u32, u64> = FxHashMap::default();
        // `sheet.iter_rect_views` visits in ascending `(row, col)` order!
        let mut sorted_cols = use_cols.clone();
        sorted_cols.sort_unstable();
        for (k, cell) in sheet.iter_rect_views((r1, min_sel, r2, max_sel)) {
            if selected.contains(&k.col) {
                let entry = row_hashes.entry(k.row).or_insert(FNV_OFFSET);
                *entry = hash_display_field(*entry, k.col, cell.value().as_ref());
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
        let p_lo = r1 / PAGE_ROWS;
        let p_hi = r2 / PAGE_ROWS;
        let col_slabs: Vec<(u32, Vec<Option<&ColSlab>>)> = use_cols
            .iter()
            .map(|&c| (c, sheet.cells().col_page_slices(c, p_lo, p_hi)))
            .collect();
        for row in r1..=r2 {
            let pi_idx = ((row / PAGE_ROWS) - p_lo) as usize;
            let r_off = row % PAGE_ROWS;
            let mut h = FNV_OFFSET;
            for &(c, ref slabs) in &col_slabs {
                if let Some(slab) = slabs[pi_idx]
                    && let Some(val) = slab.value_ref(r_off)
                {
                    h = hash_display_field(h, c, val.as_ref());
                }
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
    let Some((rect, keep)) = distinct_rows(sheet, region, cols, header) else {
        return 0;
    };
    let (r1, c1, r2, c2) = rect;
    let total_rows = (r2 - r1 + 1) as usize;
    let removed = total_rows - keep.len();
    if removed == 0 {
        return 0;
    }
    let p_lo = r1 / PAGE_ROWS;
    let p_hi = r2 / PAGE_ROWS;
    let rect_has_formulas = sheet
        .cells()
        .iter_pages_in(p_lo, p_hi)
        .any(|(_, p)| p.may_have_formula);
    if !rect_has_formulas {
        let source = sheet.search_snapshot();
        let (cells, cleared) = sheet.cells_and_cleared_mut();
        let cell_changed = cells.gather_rows(source.cells(), rect, &keep, &source.name, cleared);
        let has_styles = !source.styles.is_empty()
            || !source.row_styles.is_empty()
            || !source.col_styles.is_empty();
        let mut style_changed = false;
        if has_styles {
            let n_pages = (p_hi - p_lo + 1) as usize;
            let has_row_styles = !sheet.row_styles.is_empty();
            for col in c1..=c2 {
                let has_col_style = sheet.col_styles.contains_key(&col);
                let mut slabs = Vec::with_capacity(n_pages);
                let mut edits = Vec::new();
                let mut col_ch = false;
                for page_idx in 0..n_pages {
                    let pi = p_lo + page_idx as u32;
                    let base = pi * PAGE_ROWS;
                    let lo = r1.saturating_sub(base) as usize;
                    let hi = (r2 - base).min(PAGE_ROWS - 1) as usize;
                    let mut dense = [None::<u16>; PAGE_ROWS as usize];
                    let old_sc = sheet.styles.page_col(pi, col);
                    if let Some(osc) = old_sc {
                        for r in 0..PAGE_ROWS as usize {
                            dense[r] = osc.get(r as u32).copied();
                        }
                    }
                    let mut edit_mask = [0u64; 4];
                    for r_off in lo..=hi {
                        let dst_r = base + r_off as u32;
                        let ord_idx = (dst_r - r1) as usize;
                        // Cleared tail retains existing styles; moved surviving rows carry their source style!
                        let Some(&src_r) = keep.get(ord_idx) else {
                            continue;
                        };
                        if src_r == dst_r {
                            continue;
                        }
                        let before_st = source.style_of(dst_r, col);
                        let after_st = source.style_of(src_r, col);
                        if before_st != after_st {
                            col_ch = true;
                            edit_mask[r_off / 64] |= 1u64 << (r_off % 64);
                            dense[r_off] = (after_st != 0
                                || has_col_style
                                || (has_row_styles && sheet.row_styles.contains_key(&dst_r)))
                            .then_some(after_st);
                        }
                    }
                    slabs.push(StyleColumn::from_dense(&dense));
                    if edit_mask != [0; 4] {
                        edits.push((pi, edit_mask));
                    }
                }
                if col_ch {
                    style_changed = true;
                    sheet.styles.install_col_pages(col, p_lo, slabs);
                    for (pi, mask) in edits {
                        sheet.style_edits.or_page_mask(pi, col, mask);
                    }
                }
            }
            if style_changed {
                sheet.bump_display_style_revision();
                if !sheet.loading {
                    sheet.styles_dirty = true;
                }
            }
        }
        if (cell_changed > 0 || style_changed) && !sheet.loading {
            sheet.edited = true;
        }
        return removed;
    }
    let changes = reorder_lines(sheet, 0, rect, keep, false, true);
    changes.apply(sheet);
    changes.count
}

/// 分列：按分隔符把每格拆到右边相邻列（右侧内容整体右移腾位置）
/// 分列选项（Excel“分列”向导里的那些开关）
#[derive(Clone, Debug)]
pub struct SplitOpts {
    pub delim: char,
    /// 额外分隔符集合（支持同时勾选逗号/分号/制表符/空格等）
    pub extra_delims: Vec<char>,
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
    /// 目标起始位置（Excel 分列第 3 步“目标区域”）；None = 原地覆盖
    pub destination: Option<(u32, u32)>,
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
            extra_delims: Vec::new(),
            merge_consecutive: false,
            quote: true,
            detect_types: true,
            date_system: crate::model::DateSystem::D1900,
            fixed_widths: None,
            col_modes: None,
            destination: None,
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
        if !in_quote && (ch == opts.delim || opts.extra_delims.contains(&ch)) {
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
        return (Value::Text(crate::model::intern_short_str(text)), false);
    }
    // 前导零（00123、007）当文本，避免变成 123
    if t.len() > 1 && t.starts_with('0') && t.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return (Value::Text(crate::model::intern_short_str(text)), false);
    }
    if let Ok(n) = t.parse::<f64>() {
        return (Value::Number(n), false);
    }
    // 日期：2026-03-05 / 2026/3/5（零堆分配切分）
    if t.len() >= 8 && t.len() <= 10 && t.is_ascii() {
        let sep = if t.as_bytes().get(4) == Some(&b'-') {
            Some(b'-')
        } else if t.as_bytes().get(4) == Some(&b'/') {
            Some(b'/')
        } else {
            None
        };
        if let Some(sep_b) = sep
            && let Some(rel) = t.as_bytes()[5..].iter().position(|&b| b == sep_b)
        {
            let p2 = 5 + rel;
            if let (Ok(y), Ok(m), Ok(d)) = (
                t[..4].parse::<i32>(),
                t[5..p2].parse::<u32>(),
                t[p2 + 1..].parse::<u32>(),
            ) && (1900..=9999).contains(&y)
                && let Some(serial) = crate::formula::date_to_serial_in(opts.date_system, y, m, d)
            {
                return (Value::Number(serial), true);
            }
        }
    }
    (Value::Text(crate::model::intern_short_str(text)), false)
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
    for (k, cell) in sheet.iter_rect_views((r1, c1, r2, c1)) {
        let fields = cell.value().with_display_str(|text| {
            if text.is_empty() {
                None
            } else {
                Some(split_fields(text, opts))
            }
        });
        let Some(fields) = fields else {
            continue;
        };
        let kept = match &opts.col_modes {
            Some(modes) => fields
                .iter()
                .enumerate()
                .filter(|(i, _)| modes.get(*i).copied().unwrap_or(ColMode::General) != ColMode::Skip)
                .count(),
            None => fields.len(),
        };
        report.width = report.width.max(kept);
        rows.push((k.row, fields));
    }
    let (dst_r0, dst_c0) = opts.destination.unwrap_or((r1, c1));
    let in_place = opts.destination.is_none() || opts.destination == Some((r1, c1));
    let shift = report.width.saturating_sub(1) as u32;
    if dry_run {
        for (r, fields) in &rows {
            let dr = dst_r0 + (*r - r1);
            let skip_n = if in_place { 1 } else { 0 };
            for (i, _) in fields.iter().enumerate().skip(skip_n) {
                let c = dst_c0 + i as u32;
                let occupied = sheet
                    .get_view(dr, c)
                    .is_some_and(|x| x.value().with_display_str(|s| !s.is_empty()));
                if occupied {
                    report.overwritten += 1;
                }
            }
        }
        return report;
    }
    // 仅原地拆分时把右侧原有列整体右移腾位置；指定了独立目标单元格时直接写目标区域
    if in_place && shift > 0 {
        let last = sheet.used_bounds().map(|b| b.3).unwrap_or(c2).max(c2);
        if c1 + 1 <= last {
            let mut to_shift: Vec<(u32, u32, Cell, u16)> = sheet
                .iter_rect_views((r1, c1 + 1, r2, last))
                .map(|(k, v)| (k.row, k.col, v.to_cell(), sheet.style_of(k.row, k.col)))
                .collect();
            to_shift.sort_unstable_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            for (rr, cc, cell, st) in to_shift {
                sheet.set(rr, cc + shift, cell);
                sheet.set_style(rr, cc + shift, st);
            }
        }
    }
    for (r, fields) in &rows {
        let dr = dst_r0 + (*r - r1);
        let mut out_c = dst_c0;
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
                ColMode::Text => (Value::Text(crate::model::intern_short_str(f)), false),
                ColMode::Date => {
                    let (v, d) = detect_value(f, opts);
                    if d {
                        (v, true)
                    } else {
                        (Value::Text(crate::model::intern_short_str(f)), false)
                    }
                }
                _ => detect_value(f, opts),
            };
            if is_date {
                report.date_cells.push((dr, out_c));
            }
            sheet.set(dr, out_c, Cell { value: v, formula: None });
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
        extra_delims: Vec::new(),
        merge_consecutive: false,
        quote: false,
        detect_types: false,
        date_system: crate::model::DateSystem::D1900,
        fixed_widths: None,
        col_modes: None,
        destination: None,
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

#[cfg(test)]
mod compact_sort_gather_tests {
    use super::*;
    use crate::model::{Cell, Sheet, Value};

    #[test]
    fn apply_to_sheet_matches_deltas_on_numeric_mixed_formulas_styles_and_horizontal() {
        // 1) Mixed vertical sort (text keys + numbers + formulas + explicit & row styles + boundary rows outside rect)
        let mut base = Sheet::new("Data");
        base.set(0, 0, Cell { value: Value::Text("Header".into()), formula: None });
        base.set_row_style(10, 4);
        for r in 1..=20_005u32 {
            let key = format!("SKU-{:05}", (20_006 - r) % 777);
            base.set(r, 0, Cell { value: Value::Text(key.into()), formula: None });
            base.set(r, 1, Cell { value: Value::Number((r % 97) as f64), formula: None });
            if r % 13 == 0 {
                base.set(r, 2, Cell { value: Value::Number(0.0), formula: Some(format!("=B{}", r + 1).into()) });
            } else if r % 5 != 0 {
                base.set(r, 2, Cell { value: Value::Number((r * 2) as f64), formula: None });
            }
            base.set(r, 3, Cell { value: Value::Number((r * 3) as f64), formula: None });
            base.set(r, 4, Cell { value: Value::Number((r * 4) as f64), formula: None });
            base.set(r, 5, Cell { value: Value::Number((r * 5) as f64), formula: None });
            if r % 3 == 0 {
                base.set_style(r, 0, 2);
            }
        }
        // Row outside sort rect on boundary page
        base.set(20_006, 0, Cell { value: Value::Text("Footer".into()), formula: None });
        base.set_style(20_006, 0, 9);

        let rect = (1, 0, 20_005, 5);
        let plan = CompactSort::plan(&base, 0, rect, 0, true, false).unwrap().unwrap();

        let mut via_deltas = base.clone();
        let mut expected_count = 0usize;
        for d in plan.deltas() {
            if d.before != d.after {
                via_deltas.set(d.row, d.col, d.after);
            }
            if d.before_style != d.after_style {
                via_deltas.set_style(d.row, d.col, d.after_style);
            }
            expected_count += 1;
        }

        let mut via_gather = base.clone();
        let (gather_count, sample) = plan.apply_to_sheet_with_sample(&mut via_gather, 100);
        assert_eq!(gather_count, expected_count);
        assert_eq!(sample.len(), 100);
        assert_eq!(via_gather.len(), via_deltas.len());
        for r in 0..=20_006u32 {
            for c in 0..=5u32 {
                assert_eq!(via_gather.get(r, c), via_deltas.get(r, c), "cell mismatch at ({r},{c})");
                assert_eq!(via_gather.style_of(r, c), via_deltas.style_of(r, c), "style mismatch at ({r},{c})");
            }
        }
        assert_eq!(via_gather.get(0, 0).unwrap().value, Value::Text("Header".into()));
        assert_eq!(via_gather.get(20_006, 0).unwrap().value, Value::Text("Footer".into()));
        assert_eq!(via_gather.style_of(20_006, 0), 9);

        // 2) Dense NumericSort path (25,000 x 5 = 125,000 > 100,000)
        let mut num_base = Sheet::new("Num");
        for r in 0..25_000u32 {
            for c in 0..5u32 {
                num_base.set(r, c, Cell { value: Value::Number(((25_000 - r) + c * 3) as f64), formula: None });
            }
            if r % 4 == 0 {
                num_base.set_style(r, 0, 3);
            }
        }
        let num_plan = CompactSort::plan(&num_base, 0, (0, 0, 24_999, 4), 0, true, false)
            .unwrap()
            .unwrap();
        assert!(matches!(num_plan, CompactSort::Numeric(_)));
        let mut num_deltas = num_base.clone();
        let mut num_expected = 0usize;
        for d in num_plan.deltas() {
            if d.before != d.after {
                num_deltas.set(d.row, d.col, d.after);
            }
            if d.before_style != d.after_style {
                num_deltas.set_style(d.row, d.col, d.after_style);
            }
            num_expected += 1;
        }
        let mut num_gather = num_base.clone();
        let num_got = num_plan.apply_to_sheet(&mut num_gather);
        assert_eq!(num_got, num_expected);
        for r in [0u32, 1, 255, 256, 12_345, 24_999] {
            for c in 0..5u32 {
                assert_eq!(num_gather.get(r, c), num_deltas.get(r, c));
                assert_eq!(num_gather.style_of(r, c), num_deltas.style_of(r, c));
            }
        }

        // 3) Horizontal CompactSort path (20,001 rows x 6 cols = 120,006 > 100,000)
        let mut h_base = Sheet::new("Horiz");
        for c in 0..6u32 {
            h_base.set(0, c, Cell { value: Value::Number((6 - c) as f64), formula: None });
            for r in 1..=20_000u32 {
                h_base.set(r, c, Cell { value: Value::Number((r * 10 + c) as f64), formula: None });
            }
        }
        let h_plan = CompactSort::plan(&h_base, 0, (0, 0, 20_000, 5), 0, true, true)
            .unwrap()
            .unwrap();
        let mut h_deltas = h_base.clone();
        let mut h_expected = 0usize;
        for d in h_plan.deltas() {
            h_deltas.set(d.row, d.col, d.after);
            h_expected += 1;
        }
        let mut h_gather = h_base.clone();
        let h_got = h_plan.apply_to_sheet(&mut h_gather);
        assert_eq!(h_got, h_expected);
        for r in [0u32, 1, 255, 256, 20_000] {
            for c in 0..6u32 {
                assert_eq!(h_gather.get(r, c), h_deltas.get(r, c));
            }
        }
    }

    #[test]
    fn apply_to_sheet_200k_x10_gather_verify_and_timing() {
        let rows = 200_000u32;
        let cols = 10u32;
        let mut sheet = Sheet::new("Bench");
        for r in 0..rows {
            sheet.set(
                r,
                0,
                Cell {
                    value: Value::Text(format!("SKU-{:06}", rows - r).into()),
                    formula: None,
                },
            );
            for c in 1..cols {
                sheet.set(
                    r,
                    c,
                    Cell {
                        value: Value::Number(((r + c * 7) % 10_000) as f64),
                        formula: None,
                    },
                );
            }
        }
        let t_plan = std::time::Instant::now();
        let plan = CompactSort::plan(&sheet, 0, (0, 0, rows - 1, cols - 1), 0, true, false)
            .unwrap()
            .unwrap();
        let plan_ms = t_plan.elapsed().as_secs_f64() * 1000.0;

        let t_apply = std::time::Instant::now();
        let changed = plan.apply_to_sheet(&mut sheet);
        let apply_ms = t_apply.elapsed().as_secs_f64() * 1000.0;
        eprintln!(
            "COMPACT_SORT_GATHER_200K_X10: plan_ms={plan_ms:.2} apply_ms={apply_ms:.2} total_ms={:.2} changed={changed}",
            plan_ms + apply_ms
        );
        assert_eq!(changed, 2_000_000);
        assert_eq!(sheet.get(0, 0).unwrap().value, Value::Text("SKU-000001".into()));
        assert_eq!(sheet.get(rows - 1, 0).unwrap().value, Value::Text("SKU-200000".into()));
        assert_eq!(sheet.len(), (rows * cols) as usize);
    }
}


#[cfg(test)]
mod wave5_data_tools_tests {
    use super::*;
    use crate::model::{Cell, Sheet, Value};

    #[test]
    fn numeric_sort_precheck_falls_back_cleanly_on_trailing_blank_and_dedup_gathers_columns() {
        // 1) 25,000 x 5 numeric sheet with last row key blank -> precheck falls back to General and puts blank last
        let mut sh = Sheet::new("SortPrecheck");
        for r in 0..25_000u32 {
            if r < 24_999 {
                sh.set(r, 0, Cell { value: Value::Number((25_000 - r) as f64), formula: None });
            }
            for c in 1..5u32 {
                sh.set(r, c, Cell { value: Value::Number((r * 10 + c) as f64), formula: None });
            }
        }
        let plan = CompactSort::plan(&sh, 0, (0, 0, 24_999, 4), 0, true, false)
            .unwrap()
            .unwrap();
        assert!(matches!(plan, CompactSort::Mixed { .. }));
        plan.apply_to_sheet(&mut sh);
        assert_eq!(sh.get(0, 0).unwrap().value, Value::Number(2.0));
        assert!(sh.get(24_999, 0).is_none(), "blank key must remain at bottom row");
        assert_eq!(sh.get(24_999, 1).unwrap().value, Value::Number(249_991.0));

        // 2) Columnar remove_duplicates preserves first rows, styles, and translates surviving formulas
        let mut dedup_sh = Sheet::new("Dedup");
        dedup_sh.set(0, 0, Cell { value: Value::Text("部门".into()), formula: None });
        dedup_sh.set(0, 1, Cell { value: Value::Text("基数".into()), formula: None });
        dedup_sh.set(0, 2, Cell { value: Value::Text("十倍".into()), formula: None });
        for r in 1..=600u32 {
            let dept = format!("D{}", r % 15);
            dedup_sh.set(r, 0, Cell { value: Value::Text(dept.into()), formula: None });
            dedup_sh.set(r, 1, Cell { value: Value::Number(r as f64), formula: None });
            dedup_sh.set(
                r,
                2,
                Cell {
                    value: Value::Number((r * 10) as f64),
                    formula: Some(format!("=B{}*10", r + 1).into()),
                },
            );
            dedup_sh.set_style(r, 0, ((r % 5) + 1) as u16);
        }
        let removed = remove_duplicates(&mut dedup_sh, (0, 0, 600, 2), &[0], true);
        assert_eq!(removed, 585);
        assert_eq!(dedup_sh.used_bounds(), Some((0, 0, 15, 2)));
        for r in 1..=15u32 {
            assert_eq!(dedup_sh.formula_of(r, 2), Some(format!("=B{}*10", r + 1).as_str()));
            assert_eq!(dedup_sh.style_of(r, 0), ((r % 5) + 1) as u16);
        }

        // 3) Formula-free columnar gather_rows fast path (`!rect_has_formulas`) + tail style retention
        let mut pure_sh = Sheet::new("PureDedup");
        pure_sh.set(0, 0, Cell { value: Value::Text("地区".into()), formula: None });
        pure_sh.set(0, 1, Cell { value: Value::Text("销量".into()), formula: None });
        let regions = ["华东", "华南", "华北", "西南"];
        for r in 1..=1_000u32 {
            pure_sh.set(r, 0, Cell { value: Value::Text(regions[(r as usize) % 4].into()), formula: None });
            pure_sh.set(r, 1, Cell { value: Value::Number(r as f64), formula: None });
            pure_sh.set_style(r, 0, ((r % 4) + 10) as u16);
        }
        let pure_removed = remove_duplicates(&mut pure_sh, (0, 0, 1_000, 1), &[0], true);
        assert_eq!(pure_removed, 996);
        assert_eq!(pure_sh.used_bounds(), Some((0, 0, 4, 1)));
        assert_eq!(pure_sh.get(1, 0).unwrap().value, Value::Text("华南".into()));
        assert_eq!(pure_sh.get(4, 0).unwrap().value, Value::Text("华东".into()));
        assert!(pure_sh.get(5, 0).is_none());
        // Cleared tail retains existing styles while moved surviving rows keep their source styles
        assert_eq!(pure_sh.style_of(1, 0), 11);
        assert_eq!(pure_sh.style_of(500, 0), ((500 % 4) + 10) as u16);
    }
}
