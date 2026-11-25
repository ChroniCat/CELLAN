//! 排序与筛选。
//!
//! 排序按 Excel 的规矩：整行搬运（值、公式、样式一起走），空格永远排在最后；
//! 数字按大小、文本按不区分大小写的字典序、布尔按真假。
//! 筛选不删数据，只把不匹配的行隐藏起来（隐藏 = 行高 0，和 Excel 一致）。

use crate::model::{Sheet, Value};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasherDefault, Hasher};

/// Fast non-cryptographic FxHasher (zero external crate dependency) for sort key dedup.
#[derive(Default)]
pub(crate) struct FxHasher {
    hash: u64,
}

impl Hasher for FxHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        const K: u64 = 0x517cc1b727220a95;
        let mut h = self.hash;
        let mut chunks = bytes.chunks_exact(8);
        for chunk in &mut chunks {
            let v = u64::from_ne_bytes(chunk.try_into().unwrap());
            h = (h.rotate_left(5) ^ v).wrapping_mul(K);
        }
        let rem = chunks.remainder();
        if rem.len() >= 4 {
            let v = u32::from_ne_bytes(rem[..4].try_into().unwrap()) as u64;
            h = (h.rotate_left(5) ^ v).wrapping_mul(K);
            for &b in &rem[4..] {
                h = (h.rotate_left(5) ^ b as u64).wrapping_mul(K);
            }
        } else {
            for &b in rem {
                h = (h.rotate_left(5) ^ b as u64).wrapping_mul(K);
            }
        }
        self.hash = h;
    }

    #[inline]
    fn write_usize(&mut self, i: usize) {
        const K: u64 = 0x517cc1b727220a95;
        self.hash = (self.hash.rotate_left(5) ^ (i as u64)).wrapping_mul(K);
    }

    #[inline]
    fn write_u64(&mut self, i: u64) {
        const K: u64 = 0x517cc1b727220a95;
        self.hash = (self.hash.rotate_left(5) ^ i).wrapping_mul(K);
    }

    #[inline]
    fn finish(&self) -> u64 {
        self.hash
    }
}

pub(crate) type FxHashMap<K, V> = HashMap<K, V, BuildHasherDefault<FxHasher>>;
pub(crate) type FxHashSet<K> = HashSet<K, BuildHasherDefault<FxHasher>>;

#[inline]
pub(crate) fn prefix_u64(bytes: &[u8]) -> u64 {
    let mut buf = [0u8; 8];
    let n = bytes.len().min(8);
    buf[..n].copy_from_slice(&bytes[..n]);
    u64::from_be_bytes(buf)
}

#[inline]
fn append_lowercase_into(s: &str, arena: &mut Vec<u8>) {
    if s.is_ascii() {
        arena.extend(s.bytes().map(|b| b.to_ascii_lowercase()));
    } else if s.contains('Σ') {
        arena.extend_from_slice(s.to_lowercase().as_bytes());
    } else {
        let mut buf = [0u8; 4];
        for ch in s.chars() {
            for lc in ch.to_lowercase() {
                arena.extend_from_slice(lc.encode_utf8(&mut buf).as_bytes());
            }
        }
    }
}

#[inline]
fn merge_two_stable<T: Copy, F: Fn(&T, &T) -> Ordering>(
    a: &[T],
    b: &[T],
    out: &mut [T],
    cmp: &F,
) {
    let (mut i, mut j, mut k) = (0usize, 0usize, 0usize);
    while i < a.len() && j < b.len() {
        if cmp(&a[i], &b[j]) != Ordering::Greater {
            out[k] = a[i];
            i += 1;
        } else {
            out[k] = b[j];
            j += 1;
        }
        k += 1;
    }
    if i < a.len() {
        out[k..k + (a.len() - i)].copy_from_slice(&a[i..]);
    } else if j < b.len() {
        out[k..k + (b.len() - j)].copy_from_slice(&b[j..]);
    }
}

/// Stable parallel merge sort using `std::thread::scope` for slices > 16,384 elements.
pub(crate) fn parallel_stable_sort<T, F>(slice: &mut [T], cmp: &F)
where
    T: Copy + Send + Sync,
    F: Fn(&T, &T) -> Ordering + Sync,
{
    const PAR_THRESHOLD: usize = 16_384;
    if slice.len() <= PAR_THRESHOLD {
        slice.sort_by(cmp);
        return;
    }
    let n_threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4)
        .min(8);
    if n_threads <= 1 {
        slice.sort_by(cmp);
        return;
    }
    let chunk_size = slice.len().div_ceil(n_threads);
    let mut runs: Vec<std::ops::Range<usize>> = Vec::with_capacity(n_threads);
    {
        let mut start = 0usize;
        while start < slice.len() {
            let end = (start + chunk_size).min(slice.len());
            runs.push(start..end);
            start = end;
        }
    }
    std::thread::scope(|s| {
        for chunk in slice.chunks_mut(chunk_size) {
            s.spawn(move || chunk.sort_by(cmp));
        }
    });
    let mut scratch = slice.to_vec();
    let mut in_scratch = false;
    while runs.len() > 1 {
        let mut next_runs = Vec::with_capacity(runs.len().div_ceil(2));
        let (src, dst): (&[T], &mut [T]) = if in_scratch {
            (&scratch, slice)
        } else {
            (slice, &mut scratch)
        };
        let mut tasks: Vec<(&[T], Option<&[T]>, &mut [T])> = Vec::with_capacity(runs.len().div_ceil(2));
        let mut rem_dst = dst;
        let mut idx = 0usize;
        while idx < runs.len() {
            if idx + 1 < runs.len() {
                let a = &runs[idx];
                let b = &runs[idx + 1];
                let len = b.end - a.start;
                let (out_chunk, rest) = rem_dst.split_at_mut(len);
                rem_dst = rest;
                tasks.push((&src[a.clone()], Some(&src[b.clone()]), out_chunk));
                next_runs.push(a.start..b.end);
                idx += 2;
            } else {
                let a = &runs[idx];
                let len = a.end - a.start;
                let (out_chunk, rest) = rem_dst.split_at_mut(len);
                rem_dst = rest;
                tasks.push((&src[a.clone()], None, out_chunk));
                next_runs.push(a.clone());
                idx += 1;
            }
        }
        if tasks.len() > 1 {
            std::thread::scope(|s| {
                for (a, b_opt, out) in tasks {
                    s.spawn(move || match b_opt {
                        Some(b) => merge_two_stable(a, b, out, cmp),
                        None => out.copy_from_slice(a),
                    });
                }
            });
        } else if let Some((a, b_opt, out)) = tasks.into_iter().next() {
            match b_opt {
                Some(b) => merge_two_stable(a, b, out, cmp),
                None => out.copy_from_slice(a),
            }
        }
        runs = next_runs;
        in_scratch = !in_scratch;
    }
    if in_scratch {
        slice.copy_from_slice(&scratch);
    }
}

/// 排序不能拆开合并格或数组。动态数组目前没有独立的锚点索引，
/// 因此也保护选区中紧邻已登记溢出尾格的公式，避免只搬走锚点。
pub fn validate_sort_region(sheet: &Sheet, region: (u32, u32, u32, u32)) -> Result<(), String> {
    let (r1, c1, r2, c2) = region;
    let inside = |r: u32, c: u32| r >= r1 && r <= r2 && c >= c1 && c <= c2;
    if sheet.merges.iter().any(|&(a, b, c, d)| a <= r2 && c >= r1 && b <= c2 && d >= c1) {
        return Err("选区里有合并单元格，先取消合并再排序".into());
    }
    let array_range = sheet.arrays.iter().any(|&(_, (a, b, c, d))| a <= r2 && c >= r1 && b <= c2 && d >= c1);
    let spill_tail = sheet.spilled.iter().any(|key| inside(key.row, key.col));
    let spill_anchor = !sheet.spilled.is_empty() && sheet.formula_cells().any(|(key, _)| inside(key.row, key.col)
        && (sheet.spilled.contains(&crate::CellRef::new(key.row.saturating_add(1), key.col))
            || sheet.spilled.contains(&crate::CellRef::new(key.row, key.col.saturating_add(1)))));
    if array_range || spill_tail || spill_anchor {
        return Err("选区涉及数组公式或溢出结果，先将数组转换为数值再排序".into());
    }
    Ok(())
}

/// 在一个矩形区域里按 `col` 列排序（col 必须是区域内的一列）。
/// 调用方须在完成后重建工作簿依赖图并重算，更新移动公式的缓存及跨表下游。
pub fn sort_range(
    sheet: &mut Sheet,
    r1: u32,
    c1: u32,
    r2: u32,
    c2: u32,
    col: u32,
    asc: bool,
) -> Result<(), String> {
    let changes = plan_sort_axis(sheet, 0, (r1, c1, r2, c2), col, asc, false)?;
    changes.apply(sheet);
    Ok(())
}

/// 按行排序，以 `row` 为关键字重排区域内的列。
pub fn sort_range_by_row(
    sheet: &mut Sheet,
    r1: u32,
    c1: u32,
    r2: u32,
    c2: u32,
    row: u32,
    asc: bool,
) -> Result<(), String> {
    let changes = plan_sort_axis(sheet, 0, (r1, c1, r2, c2), row, asc, true)?;
    changes.apply(sheet);
    Ok(())
}

pub fn plan_sort_axis(
    sheet: &Sheet,
    sheet_idx: usize,
    region: (u32, u32, u32, u32),
    key: u32,
    asc: bool,
    horizontal: bool,
) -> Result<crate::data_tools::RowChanges, String> {
    let order = sort_order(sheet, region, key, asc, horizontal)?;
    if order.is_empty() { return Ok(Default::default()); }
    Ok(crate::data_tools::reorder_lines(sheet, sheet_idx, region, order, horizontal, false))
}

#[derive(Clone, Copy)]
struct SortEntry {
    /// For Number: `f64::to_bits()`.
    /// For Text/Error: big-endian 8-byte prefix (or dense `u32` rank when ranked).
    /// For Bool: 0 or 1. For Empty: 0.
    payload: u64,
    line: u32,
    /// Top 3 bits: kind (0=Number, 1=Text, 2=Bool, 3=Error, 4=Empty); bottom 29 bits: span index.
    kind_aux: u32,
}

impl SortEntry {
    const KIND_NUM: u8 = 0;
    const KIND_TEXT: u8 = 1;
    const KIND_BOOL: u8 = 2;
    const KIND_ERR: u8 = 3;
    const KIND_EMPTY: u8 = 4;

    #[inline]
    fn new(line: u32, kind: u8, aux: u32, payload: u64) -> Self {
        Self {
            payload,
            line,
            kind_aux: ((kind as u32) << 29) | (aux & 0x1fff_ffff),
        }
    }

    #[inline]
    fn empty(line: u32) -> Self {
        Self::new(line, Self::KIND_EMPTY, 0, 0)
    }

    #[inline]
    fn kind(self) -> u8 {
        (self.kind_aux >> 29) as u8
    }

    #[inline]
    fn aux(self) -> u32 {
        self.kind_aux & 0x1fff_ffff
    }
}

pub(crate) fn sort_order(
    sheet: &Sheet,
    region: (u32, u32, u32, u32),
    key: u32,
    asc: bool,
    horizontal: bool,
) -> Result<Vec<u32>, String> {
    let (r1, c1, r2, c2) = region;
    let (first, last, lo, hi) = if horizontal {
        (c1, c2, r1, r2)
    } else {
        (r1, r2, c1, c2)
    };
    if last <= first {
        return Ok(Default::default());
    }
    if !(lo..=hi).contains(&key) {
        return Err(if horizontal {
            "排序关键字行不在选区里"
        } else {
            "排序列不在选区里"
        }
        .into());
    }
    validate_sort_region(sheet, region)?;

    let len = (last - first + 1) as usize;
    let mut entries: Vec<SortEntry> = (first..=last).map(SortEntry::empty).collect();
    let mut arena: Vec<u8> = Vec::new();
    let mut spans: Vec<(u32, u32)> = Vec::new();
    let mut ptr_cache: FxHashMap<usize, (u32, u64)> = FxHashMap::default();
    let mut text_count = 0usize;

    let mut encode_value = |line: u32, v: &Value| -> SortEntry {
        match v {
            Value::Number(n) => SortEntry::new(line, SortEntry::KIND_NUM, 0, n.to_bits()),
            Value::Text(t) => {
                text_count += 1;
                let shared = std::sync::Arc::strong_count(t) > 1;
                let ptr = std::sync::Arc::as_ptr(t) as *const () as usize;
                if shared
                    && let Some(&(span_id, pref)) = ptr_cache.get(&ptr)
                {
                    return SortEntry::new(line, SortEntry::KIND_TEXT, span_id, pref);
                }
                let off = arena.len() as u32;
                append_lowercase_into(t, &mut arena);
                let byte_len = (arena.len() as u32) - off;
                let pref = prefix_u64(&arena[off as usize..(off + byte_len) as usize]);
                let span_id = spans.len() as u32;
                spans.push((off, byte_len));
                if shared {
                    ptr_cache.insert(ptr, (span_id, pref));
                }
                SortEntry::new(line, SortEntry::KIND_TEXT, span_id, pref)
            }
            Value::Bool(b) => SortEntry::new(line, SortEntry::KIND_BOOL, 0, u64::from(*b)),
            Value::Error(e) => {
                let s = e.text().as_bytes();
                let off = arena.len() as u32;
                arena.extend_from_slice(s);
                let byte_len = s.len() as u32;
                let pref = prefix_u64(s);
                let span_id = spans.len() as u32;
                spans.push((off, byte_len));
                SortEntry::new(line, SortEntry::KIND_ERR, span_id, pref)
            }
            Value::Empty => SortEntry::empty(line),
        }
    };

    if !horizontal {
        for (base_row, slice) in sheet.column_typed_slices(first, last, key) {
            let start_idx = (base_row - first) as usize;
            match slice {
                crate::store::ColSlice::Numbers { present, start, values } => {
                    if crate::store::ColSlice::numbers_all_present(&present, start, values.len()) {
                        for (i, &n) in values.iter().enumerate() {
                            let line = base_row + i as u32;
                            entries[start_idx + i] =
                                SortEntry::new(line, SortEntry::KIND_NUM, 0, n.to_bits());
                        }
                    } else {
                        for (i, &n) in values.iter().enumerate() {
                            let ro = start + i;
                            if (present[ro / 64] & (1u64 << (ro % 64))) != 0 {
                                let line = base_row + i as u32;
                                entries[start_idx + i] =
                                    SortEntry::new(line, SortEntry::KIND_NUM, 0, n.to_bits());
                            }
                        }
                    }
                }
                crate::store::ColSlice::Values(vs) => {
                    for (i, val) in vs.iter().enumerate() {
                        if !val.is_empty() {
                            let line = base_row + i as u32;
                            entries[start_idx + i] = encode_value(line, val);
                        }
                    }
                }
                crate::store::ColSlice::Mixed(cs) => {
                    for (i, cell) in cs.iter().enumerate() {
                        if !cell.value.is_empty() {
                            let line = base_row + i as u32;
                            entries[start_idx + i] = encode_value(line, &cell.value);
                        }
                    }
                }
            }
        }
    } else {
        for i in 0..len {
            let line = first + i as u32;
            let v = sheet.value_ref(key, line);
            if !v.is_empty() {
                entries[i] = encode_value(line, v.as_ref());
            }
        }
    }

    // Deduplicated text rank fast path: use Chinese Pinyin collation when any non-ASCII text exists
    // (unifying Quick Sort with Multi-Key Sort and Chinese Excel!), keeping ASCII pure-byte fast path.
    let has_non_ascii = text_count > 0 && !arena.is_ascii();
    let text_ranked = if text_count > 0 && ((spans.len() * 2 <= text_count && spans.len() > 1) || (has_non_ascii && spans.len() > 1)) {
        let mut unique: Vec<(u64, u32)> = spans
            .iter()
            .enumerate()
            .map(|(id, &(off, blen))| {
                let bytes = &arena[off as usize..(off + blen) as usize];
                (prefix_u64(bytes), id as u32)
            })
            .collect();
        let arena_ref = arena.as_slice();
        let spans_ref = spans.as_slice();
        if has_non_ascii {
            let mut key_cache = crate::collate::SortKeyCache::new();
            let col_keys: Vec<Box<[u8]>> = spans
                .iter()
                .map(|&(off, blen)| {
                    let s = std::str::from_utf8(&arena_ref[off as usize..(off + blen) as usize]).unwrap_or("");
                    key_cache.get_or_compute(s).into()
                })
                .collect();
            parallel_stable_sort(&mut unique, &|a, b| col_keys[a.1 as usize].cmp(&col_keys[b.1 as usize]));
            let mut ranks = vec![0u32; spans.len()];
            let mut cur_rank = 0u32;
            for i in 0..unique.len() {
                if i > 0 && col_keys[unique[i - 1].1 as usize] != col_keys[unique[i].1 as usize] {
                    cur_rank += 1;
                }
                ranks[unique[i].1 as usize] = cur_rank;
            }
            for e in &mut entries {
                let k = e.kind();
                if k == SortEntry::KIND_TEXT || k == SortEntry::KIND_ERR {
                    e.payload = ranks[e.aux() as usize] as u64;
                }
            }
        } else {
            parallel_stable_sort(&mut unique, &|a, b| {
                a.0.cmp(&b.0).then_with(|| {
                    let (oa, la) = spans_ref[a.1 as usize];
                    let (ob, lb) = spans_ref[b.1 as usize];
                    arena_ref[oa as usize..(oa + la) as usize]
                        .cmp(&arena_ref[ob as usize..(ob + lb) as usize])
                })
            });
            let mut ranks = vec![0u32; spans.len()];
            let mut cur_rank = 0u32;
            for i in 0..unique.len() {
                if i > 0 {
                    let prev_id = unique[i - 1].1 as usize;
                    let cur_id = unique[i].1 as usize;
                    let (op, lp) = spans[prev_id];
                    let (oc, lc) = spans[cur_id];
                    if arena[op as usize..(op + lp) as usize]
                        != arena[oc as usize..(oc + lc) as usize]
                    {
                        cur_rank += 1;
                    }
                }
                ranks[unique[i].1 as usize] = cur_rank;
            }
            for e in &mut entries {
                let k = e.kind();
                if k == SortEntry::KIND_TEXT || k == SortEntry::KIND_ERR {
                    e.payload = ranks[e.aux() as usize] as u64;
                }
            }
        }
        true
    } else {
        false
    };

    let arena_ref = arena.as_slice();
    let spans_ref = spans.as_slice();
    let cmp = |a: &SortEntry, b: &SortEntry| -> Ordering {
        let ka = a.kind();
        let kb = b.kind();
        if ka == SortEntry::KIND_EMPTY || kb == SortEntry::KIND_EMPTY {
            return ka.cmp(&kb);
        }
        let ord = ka.cmp(&kb).then_with(|| match ka {
            SortEntry::KIND_NUM => f64::from_bits(a.payload)
                .partial_cmp(&f64::from_bits(b.payload))
                .unwrap_or(Ordering::Equal),
            SortEntry::KIND_BOOL => a.payload.cmp(&b.payload),
            _ => {
                if text_ranked {
                    a.payload.cmp(&b.payload)
                } else {
                    a.payload.cmp(&b.payload).then_with(|| {
                        let (oa, la) = spans_ref[a.aux() as usize];
                        let (ob, lb) = spans_ref[b.aux() as usize];
                        arena_ref[oa as usize..(oa + la) as usize]
                            .cmp(&arena_ref[ob as usize..(ob + lb) as usize])
                    })
                }
            }
        });
        if asc { ord } else { ord.reverse() }
    };

    parallel_stable_sort(&mut entries, &cmp);
    Ok(entries.into_iter().map(|e| e.line).collect())
}

/// 区域第一列是不是表头列：整列都是非空文本，且右侧出现过数字（按行排序时跳过它）
pub fn has_header_col(sheet: &Sheet, r1: u32, c1: u32, r2: u32, c2: u32) -> bool {
    if c2 <= c1 {
        return false;
    }
    let all_text = (r1..=r2).all(|r| {
        sheet.value_ref(r, c1).as_str().is_some_and(|t| !t.is_empty())
    });
    if !all_text {
        return false;
    }
    has_numeric(sheet, (r1, c1 + 1, r2, c2))
}

/// 带表头列判断的按行排序
pub fn sort_region_by_row(
    sheet: &mut Sheet,
    region: (u32, u32, u32, u32),
    row: u32,
    asc: bool,
) -> Result<(), String> {
    let (r1, c1, r2, c2) = region;
    if has_header_col(sheet, r1, c1, r2, c2) {
        sort_range_by_row(sheet, r1, c1 + 1, r2, c2, row, asc)
    } else {
        sort_range_by_row(sheet, r1, c1, r2, c2, row, asc)
    }
}

/// 区域第一行是不是表头：整行都是非空文本，且下面出现过数字
pub fn has_header(sheet: &Sheet, r1: u32, c1: u32, r2: u32, c2: u32) -> bool {
    if r2 <= r1 {
        return false;
    }
    let all_text = (c1..=c2).all(|c| {
        sheet.value_ref(r1, c).as_str().is_some_and(|t| !t.is_empty())
    });
    if !all_text {
        return false;
    }
    has_numeric(sheet, (r1 + 1, c1, r2, c2))
}

fn has_numeric(sheet: &Sheet, rect: (u32, u32, u32, u32)) -> bool {
    let (r1, c1, r2, c2) = rect;
    if r2 < r1 || c2 < c1 { return false; }
    let area = u64::from(r2 - r1 + 1) * u64::from(c2 - c1 + 1);
    if area > 4096 && area > sheet.len() as u64 * 4 {
        sheet.stored_cells().any(|(k, cell)| (r1..=r2).contains(&k.row)
            && (c1..=c2).contains(&k.col) && cell.as_f64().is_some())
    } else {
        (r1..=r2).any(|r| (c1..=c2).any(|c| sheet.get_f64(r, c).is_some()))
    }
}

/// 带表头判断的排序：有表头就只排序下面的数据行
pub fn sort_region(sheet: &mut Sheet, region: (u32, u32, u32, u32), col: u32, asc: bool) -> Result<(), String> {
    let (r1, c1, r2, c2) = region;
    if has_header(sheet, r1, c1, r2, c2) {
        sort_range(sheet, r1 + 1, c1, r2, c2, col, asc)
    } else {
        sort_range(sheet, r1, c1, r2, c2, col, asc)
    }
}

/// 以 (r, c) 为中心找出连续的数据区域（Excel 的「当前区域」）。
/// 采用逐轮 1 格边界扩展：使用 `Store` 页级 `row_mask` 与 `col_nonempty` 位图，
/// 既可在微秒级跳过百万空行，又严格止步于空白分隔行/列，绝不误串隔空表。
pub fn current_region(sheet: &Sheet, r: u32, c: u32) -> (u32, u32, u32, u32) {
    let Some((ur1, uc1, ur2, uc2)) = sheet.used_bounds() else {
        return (r, c, r, c);
    };
    if r > ur2.saturating_add(1)
        || r.saturating_add(1) < ur1
        || c > uc2.saturating_add(1)
        || c.saturating_add(1) < uc1
    {
        return (r, c, r, c);
    }
    let min_r = ur1.min(r);
    let max_r = ur2.max(r);
    let min_c = uc1.min(c);
    let max_c = uc2.max(c);
    let cells = sheet.cells();

    let mut r1 = r;
    let mut r2 = r;
    let mut c1 = c;
    let mut c2 = c;
    loop {
        let mut changed = false;
        let c_lo = c1.saturating_sub(1).max(min_c);
        let c_hi = c2.saturating_add(1).min(max_c);
        while r1 > min_r && cells.row_has_any_in(r1 - 1, c_lo, c_hi) {
            r1 -= 1;
            changed = true;
        }
        while r2 < max_r && cells.row_has_any_in(r2 + 1, c_lo, c_hi) {
            r2 += 1;
            changed = true;
        }
        let r_lo = r1.saturating_sub(1).max(min_r);
        let r_hi = r2.saturating_add(1).min(max_r);
        while c1 > min_c && cells.col_has_any_in(c1 - 1, r_lo, r_hi) {
            c1 -= 1;
            changed = true;
        }
        while c2 < max_c && cells.col_has_any_in(c2 + 1, r_lo, r_hi) {
            c2 += 1;
            changed = true;
        }
        if !changed {
            break;
        }
    }
    (r1, c1, r2, c2)
}

/// 比价方式：
/// - 文本类：Contains/NotContains/Begins/Ends
/// - 数值类：Eq/Ne/Gt/Ge/Lt/Le/Between（两边都能转成数字就按数字比，否则按文本比）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
    Between,
    Contains,
    NotContains,
    Begins,
    Ends,
}

impl CmpOp {
    /// 菜单里的中文名
    pub fn label(&self) -> &'static str {
        match self {
            CmpOp::Eq => "等于",
            CmpOp::Ne => "不等于",
            CmpOp::Gt => "大于",
            CmpOp::Ge => "大于或等于",
            CmpOp::Lt => "小于",
            CmpOp::Le => "小于或等于",
            CmpOp::Between => "介于",
            CmpOp::Contains => "包含",
            CmpOp::NotContains => "不包含",
            CmpOp::Begins => "开头是",
            CmpOp::Ends => "结尾是",
        }
    }

    /// xlsx customFilter 的 operator 属性
    pub fn xml_operator(&self) -> &'static str {
        match self {
            CmpOp::Eq => "equal",
            CmpOp::Ne => "notEqual",
            CmpOp::Gt => "greaterThan",
            CmpOp::Ge => "greaterThanOrEqual",
            CmpOp::Lt => "lessThan",
            CmpOp::Le => "lessThanOrEqual",
            CmpOp::Between => "between",
            CmpOp::Contains => "contains",
            CmpOp::NotContains => "notContains",
            CmpOp::Begins => "beginsWith",
            CmpOp::Ends => "endsWith",
        }
    }

    pub fn from_xml(op: &str) -> Option<Self> {
        Some(match op {
            "equal" => CmpOp::Eq,
            "notEqual" => CmpOp::Ne,
            "greaterThan" => CmpOp::Gt,
            "greaterThanOrEqual" => CmpOp::Ge,
            "lessThan" => CmpOp::Lt,
            "lessThanOrEqual" => CmpOp::Le,
            "between" => CmpOp::Between,
            "contains" => CmpOp::Contains,
            "notContains" => CmpOp::NotContains,
            "beginsWith" => CmpOp::Begins,
            "endsWith" => CmpOp::Ends,
            _ => return None,
        })
    }

    /// 文本类还是数值类（决定菜单里显示「文本筛选」还是「数字筛选」）
    pub fn is_text(&self) -> bool {
        matches!(
            self,
            CmpOp::Contains | CmpOp::NotContains | CmpOp::Begins | CmpOp::Ends
        )
    }
}

/// Excel 风格通配符匹配（大小写不敏感；`*` 任意串、`?` 单字符、`~` 转义）。
pub fn wildcard_match_ci(text: &str, pattern: &str) -> bool {
    let t: Vec<char> = text.to_lowercase().chars().collect();
    #[derive(Clone, Copy)]
    enum Tok {
        Star,
        Any,
        Lit(char),
    }
    let mut toks: Vec<Tok> = Vec::new();
    let pat_lower = pattern.to_lowercase();
    let mut it = pat_lower.chars();
    while let Some(ch) = it.next() {
        match ch {
            '~' => {
                if let Some(next) = it.next() {
                    toks.push(Tok::Lit(next));
                } else {
                    toks.push(Tok::Lit('~'));
                }
            }
            '*' => {
                if !matches!(toks.last(), Some(Tok::Star)) {
                    toks.push(Tok::Star);
                }
            }
            '?' => toks.push(Tok::Any),
            other => toks.push(Tok::Lit(other)),
        }
    }
    let (mut ti, mut pi) = (0usize, 0usize);
    let mut star_pi: Option<usize> = None;
    let mut star_ti = 0usize;
    while ti < t.len() {
        if pi < toks.len() {
            match toks[pi] {
                Tok::Star => {
                    star_pi = Some(pi);
                    star_ti = ti;
                    pi += 1;
                    continue;
                }
                Tok::Any => {
                    ti += 1;
                    pi += 1;
                    continue;
                }
                Tok::Lit(c) if c == t[ti] => {
                    ti += 1;
                    pi += 1;
                    continue;
                }
                _ => {}
            }
        }
        if let Some(sp) = star_pi {
            star_ti += 1;
            ti = star_ti;
            pi = sp + 1;
        } else {
            return false;
        }
    }
    while pi < toks.len() && matches!(toks[pi], Tok::Star) {
        pi += 1;
    }
    pi == toks.len()
}

#[inline]
fn pattern_has_wildcard(s: &str) -> bool {
    s.contains('*') || s.contains('?') || s.contains('~')
}

/// 一条筛选条件（支持 Between 及 Excel 双条件 AND / OR 组合与通配符 `*`/`?`/`~`）
#[derive(Clone, Debug, PartialEq)]
pub struct Criterion {
    pub op: CmpOp,
    pub val: String,
    /// 只有 Between 用
    pub val2: Option<String>,
    /// 可选第二子条件：`(is_and, op2, val2)`，对应 Excel 自定义筛选「与 / 或」
    pub second: Option<(bool, CmpOp, String)>,
}

impl Criterion {
    pub fn new(op: CmpOp, val: impl Into<String>) -> Self {
        Self {
            op,
            val: val.into(),
            val2: None,
            second: None,
        }
    }

    pub fn between(a: impl Into<String>, b: impl Into<String>) -> Self {
        Self {
            op: CmpOp::Between,
            val: a.into(),
            val2: Some(b.into()),
            second: None,
        }
    }

    pub fn compound(
        op1: CmpOp,
        val1: impl Into<String>,
        is_and: bool,
        op2: CmpOp,
        val2: impl Into<String>,
    ) -> Self {
        Self {
            op: op1,
            val: val1.into(),
            val2: None,
            second: Some((is_and, op2, val2.into())),
        }
    }

    fn matches_single(op: CmpOp, val: &str, val2: Option<&str>, text: &str) -> bool {
        match op {
            CmpOp::Contains => {
                if pattern_has_wildcard(val) {
                    wildcard_match_ci(text, &format!("*{val}*"))
                } else {
                    text.to_lowercase().contains(&val.to_lowercase())
                }
            }
            CmpOp::NotContains => !Self::matches_single(CmpOp::Contains, val, None, text),
            CmpOp::Begins => {
                if pattern_has_wildcard(val) {
                    wildcard_match_ci(text, &format!("{val}*"))
                } else {
                    text.to_lowercase().starts_with(&val.to_lowercase())
                }
            }
            CmpOp::Ends => {
                if pattern_has_wildcard(val) {
                    wildcard_match_ci(text, &format!("*{val}"))
                } else {
                    text.to_lowercase().ends_with(&val.to_lowercase())
                }
            }
            _ => {
                if matches!(op, CmpOp::Eq | CmpOp::Ne) && pattern_has_wildcard(val) {
                    let hit = wildcard_match_ci(text, val);
                    return if op == CmpOp::Eq { hit } else { !hit };
                }
                // 数值优先，转不动就按文本比
                if let (Ok(a), Ok(b)) = (text.trim().parse::<f64>(), val.trim().parse::<f64>()) {
                    return match op {
                        CmpOp::Eq => (a - b).abs() <= 1e-12 * (1.0 + a.abs().max(b.abs())),
                        CmpOp::Ne => (a - b).abs() > 1e-12 * (1.0 + a.abs().max(b.abs())),
                        CmpOp::Gt => a > b,
                        CmpOp::Ge => a >= b,
                        CmpOp::Lt => a < b,
                        CmpOp::Le => a <= b,
                        CmpOp::Between => {
                            let hi = val2
                                .and_then(|v| v.trim().parse::<f64>().ok())
                                .unwrap_or(b);
                            a >= b.min(hi) && a <= b.max(hi)
                        }
                        _ => true,
                    };
                }
                let t = text.to_lowercase();
                let v = val.to_lowercase();
                match op {
                    CmpOp::Eq => t == v,
                    CmpOp::Ne => t != v,
                    CmpOp::Gt => t > v,
                    CmpOp::Ge => t >= v,
                    CmpOp::Lt => t < v,
                    CmpOp::Le => t <= v,
                    CmpOp::Between => {
                        let hi = val2.unwrap_or("").to_lowercase();
                        let (lo, hi) = if v <= hi { (v, hi) } else { (hi, v) };
                        t >= lo && t <= hi
                    }
                    _ => true,
                }
            }
        }
    }

    /// 这条条件是否放行某个显示文本
    pub fn matches(&self, text: &str) -> bool {
        let first = Self::matches_single(self.op, &self.val, self.val2.as_deref(), text);
        if let Some((is_and, op2, val2)) = &self.second {
            let second = Self::matches_single(*op2, val2, None, text);
            if *is_and { first && second } else { first || second }
        } else {
            first
        }
    }
}

/// 前 N 项 / 后 N 项 / 前 N% / 后 N% 筛选（OOXML `<top10>`）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TopFilter {
    pub top: bool,
    pub count: u32,
    pub percent: bool,
}

/// 动态筛选：高于/低于平均值与动态日期（OOXML `<dynamicFilter>`）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DynamicFilter {
    AboveAverage,
    BelowAverage,
    Today,
    Yesterday,
    Tomorrow,
    ThisWeek,
    LastWeek,
    NextWeek,
    ThisMonth,
    LastMonth,
    NextMonth,
    ThisYear,
    LastYear,
    NextYear,
}

impl DynamicFilter {
    pub fn from_ooxml(s: &str) -> Option<Self> {
        Some(match s {
            "aboveAverage" => Self::AboveAverage,
            "belowAverage" => Self::BelowAverage,
            "today" => Self::Today,
            "yesterday" => Self::Yesterday,
            "tomorrow" => Self::Tomorrow,
            "thisWeek" => Self::ThisWeek,
            "lastWeek" => Self::LastWeek,
            "nextWeek" => Self::NextWeek,
            "thisMonth" => Self::ThisMonth,
            "lastMonth" => Self::LastMonth,
            "nextMonth" => Self::NextMonth,
            "thisYear" => Self::ThisYear,
            "lastYear" => Self::LastYear,
            "nextYear" => Self::NextYear,
            _ => return None,
        })
    }
    pub fn as_ooxml(self) -> &'static str {
        match self {
            Self::AboveAverage => "aboveAverage",
            Self::BelowAverage => "belowAverage",
            Self::Today => "today",
            Self::Yesterday => "yesterday",
            Self::Tomorrow => "tomorrow",
            Self::ThisWeek => "thisWeek",
            Self::LastWeek => "lastWeek",
            Self::NextWeek => "nextWeek",
            Self::ThisMonth => "thisMonth",
            Self::LastMonth => "lastMonth",
            Self::NextMonth => "nextMonth",
            Self::ThisYear => "thisYear",
            Self::LastYear => "lastYear",
            Self::NextYear => "nextYear",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::AboveAverage => "高于平均值",
            Self::BelowAverage => "低于平均值",
            Self::Today => "今天",
            Self::Yesterday => "昨天",
            Self::Tomorrow => "明天",
            Self::ThisWeek => "本周",
            Self::LastWeek => "上周",
            Self::NextWeek => "下周",
            Self::ThisMonth => "本月",
            Self::LastMonth => "上月",
            Self::NextMonth => "下月",
            Self::ThisYear => "今年",
            Self::LastYear => "去年",
            Self::NextYear => "明年",
        }
    }
    pub fn is_date(self) -> bool {
        !matches!(self, Self::AboveAverage | Self::BelowAverage)
    }
    pub fn matches_date(self, serial: f64, today_serial: i64, sys: crate::model::DateSystem) -> bool {
        let Some((y, m, _)) = crate::formula::functions::serial_to_date_in(sys, serial) else {
            return false;
        };
        let Some((ty, tm, _)) = crate::formula::functions::serial_to_date_in(sys, today_serial as f64) else {
            return false;
        };
        let d = serial.floor() as i64;
        match self {
            Self::Today => d == today_serial,
            Self::Yesterday => d == today_serial - 1,
            Self::Tomorrow => d == today_serial + 1,
            Self::ThisWeek | Self::LastWeek | Self::NextWeek => {
                let dow = (today_serial - 1).rem_euclid(7);
                let ws = today_serial - dow;
                let (lo, hi) = match self {
                    Self::ThisWeek => (ws, ws + 6),
                    Self::LastWeek => (ws - 7, ws - 1),
                    Self::NextWeek => (ws + 7, ws + 13),
                    _ => unreachable!(),
                };
                d >= lo && d <= hi
            }
            Self::ThisMonth => y == ty && m == tm,
            Self::LastMonth => {
                let (wy, wm) = if tm == 1 { (ty - 1, 12) } else { (ty, tm - 1) };
                y == wy && m == wm
            }
            Self::NextMonth => {
                let (wy, wm) = if tm == 12 { (ty + 1, 1) } else { (ty, tm + 1) };
                y == wy && m == wm
            }
            Self::ThisYear => y == ty,
            Self::LastYear => y == ty - 1,
            Self::NextYear => y == ty + 1,
            Self::AboveAverage | Self::BelowAverage => false,
        }
    }
}

/// 按颜色筛选（单元格填充色或字体颜色）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ColorFilter {
    /// 匹配指定背景填充色（`None` 表示无填充）。
    Fill(Option<[u8; 3]>),
    /// 匹配指定字体颜色。
    Font([u8; 3]),
}

/// 日期层级筛选节点（年 / 年-月 / 年-月-日）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DateGroupItem {
    pub year: i32,
    pub month: Option<u32>,
    pub day: Option<u32>,
}

impl DateGroupItem {
    #[inline]
    pub fn matches(&self, y: i32, m: u32, d: u32) -> bool {
        if self.year != y {
            return false;
        }
        if let Some(mo) = self.month
            && mo != m
        {
            return false;
        }
        if let Some(da) = self.day
            && da != d
        {
            return false;
        }
        true
    }
}

/// 筛选列里的日期树（年 -> 月 -> 日）和唯一填充/字体颜色。
#[derive(Clone, Debug, Default)]
pub struct ColumnFilterMeta {
    pub dates: std::collections::BTreeMap<i32, std::collections::BTreeMap<u32, std::collections::BTreeSet<u32>>>,
    pub fill_colors: Vec<Option<[u8; 3]>>,
    pub font_colors: Vec<[u8; 3]>,
}

/// 一个筛选器：区域 + 每列允许的值 / 条件 / 日期分组 / 颜色筛选 / Top10 / 动态筛选
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Filter {
    pub range: (u32, u32, u32, u32),
    /// 列号 → 允许出现文本值；没有条目的列不过滤（CoW 共享，避免 Sheet 克隆深拷几万唯一值）
    pub allows: crate::model::CellMetadata<u32, HashSet<String>>,
    /// 列号 → 条件（文本筛选/数字筛选/AND-OR/通配符）；和 allows 是「与」关系
    pub criteria: HashMap<u32, Criterion>,
    /// 列号 → 允许的日期层级分组（年 / 月 / 日）
    pub date_groups: HashMap<u32, Vec<DateGroupItem>>,
    /// 列号 → 按单元格填充色或字体色筛选
    pub color_filters: HashMap<u32, ColorFilter>,
    /// 列号 → 前/后 N 项或 N% 筛选
    pub top_filters: HashMap<u32, TopFilter>,
    /// 列号 → 高于/低于平均或动态日期筛选
    pub dynamic_filters: HashMap<u32, DynamicFilter>,
}

#[inline]
fn ascii_contains_ci(hay: &[u8], needle_lower: &[u8]) -> bool {
    if needle_lower.is_empty() {
        return true;
    }
    if hay.len() < needle_lower.len() {
        return false;
    }
    let b0 = needle_lower[0];
    let end = hay.len() - needle_lower.len();
    for i in 0..=end {
        if hay[i].to_ascii_lowercase() == b0
            && hay[i..i + needle_lower.len()].eq_ignore_ascii_case(needle_lower)
        {
            return true;
        }
    }
    false
}

struct CompiledCriterion {
    op: CmpOp,
    num1: Option<f64>,
    num2: Option<f64>,
    lower1: String,
    lower2: String,
    ascii_only: bool,
    has_wild: bool,
    raw_val: String,
    second: Option<(bool, Box<CompiledCriterion>)>,
}

impl CompiledCriterion {
    fn new(c: &Criterion) -> Self {
        let num1 = if c.op.is_text() {
            None
        } else {
            c.val.trim().parse::<f64>().ok()
        };
        let num2 = if c.op == CmpOp::Between {
            c.val2
                .as_deref()
                .and_then(|v| v.trim().parse::<f64>().ok())
                .or(num1)
        } else {
            None
        };
        let lower1 = c.val.to_lowercase();
        let lower2 = c.val2.as_deref().unwrap_or("").to_lowercase();
        let ascii_only = lower1.is_ascii() && lower2.is_ascii();
        let has_wild = pattern_has_wildcard(&c.val);
        let second = c.second.as_ref().map(|(is_and, op2, v2)| {
            (*is_and, Box::new(CompiledCriterion::new(&Criterion::new(*op2, v2.clone()))))
        });
        Self {
            op: c.op,
            num1,
            num2,
            lower1,
            lower2,
            ascii_only,
            has_wild,
            raw_val: c.val.clone(),
            second,
        }
    }

    #[inline]
    fn matches_num(&self, a: f64, b: f64) -> bool {
        match self.op {
            CmpOp::Eq => (a - b).abs() <= 1e-12 * (1.0 + a.abs().max(b.abs())),
            CmpOp::Ne => (a - b).abs() > 1e-12 * (1.0 + a.abs().max(b.abs())),
            CmpOp::Gt => a > b,
            CmpOp::Ge => a >= b,
            CmpOp::Lt => a < b,
            CmpOp::Le => a <= b,
            CmpOp::Between => {
                let hi = self.num2.unwrap_or(b);
                a >= b.min(hi) && a <= b.max(hi)
            }
            _ => true,
        }
    }

    fn matches_str_single(&self, text: &str) -> bool {
        if self.has_wild {
            return match self.op {
                CmpOp::Eq => wildcard_match_ci(text, &self.raw_val),
                CmpOp::Ne => !wildcard_match_ci(text, &self.raw_val),
                CmpOp::Contains => wildcard_match_ci(text, &format!("*{}*", self.raw_val)),
                CmpOp::NotContains => !wildcard_match_ci(text, &format!("*{}*", self.raw_val)),
                CmpOp::Begins => wildcard_match_ci(text, &format!("{}*", self.raw_val)),
                CmpOp::Ends => wildcard_match_ci(text, &format!("*{}", self.raw_val)),
                _ => false,
            };
        }
        if self.op.is_text() {
            if self.ascii_only && text.is_ascii() {
                let tb = text.as_bytes();
                let nb = self.lower1.as_bytes();
                return match self.op {
                    CmpOp::Contains => ascii_contains_ci(tb, nb),
                    CmpOp::NotContains => !ascii_contains_ci(tb, nb),
                    CmpOp::Begins => tb.len() >= nb.len() && tb[..nb.len()].eq_ignore_ascii_case(nb),
                    CmpOp::Ends => {
                        tb.len() >= nb.len() && tb[tb.len() - nb.len()..].eq_ignore_ascii_case(nb)
                    }
                    _ => unreachable!(),
                };
            }
            let low = text.to_lowercase();
            return match self.op {
                CmpOp::Contains => low.contains(&self.lower1),
                CmpOp::NotContains => !low.contains(&self.lower1),
                CmpOp::Begins => low.starts_with(&self.lower1),
                CmpOp::Ends => low.ends_with(&self.lower1),
                _ => unreachable!(),
            };
        }
        if let Some(b) = self.num1 {
            let trimmed = text.trim();
            if trimmed
                .as_bytes()
                .first()
                .is_some_and(|ch| ch.is_ascii_digit() || matches!(ch, b'+' | b'-' | b'.' | b'i' | b'I' | b'n' | b'N'))
                && let Ok(a) = trimmed.parse::<f64>()
            {
                return self.matches_num(a, b);
            }
        }
        let t = text.to_lowercase();
        let v = &self.lower1;
        match self.op {
            CmpOp::Eq => &t == v,
            CmpOp::Ne => &t != v,
            CmpOp::Gt => &t > v,
            CmpOp::Ge => &t >= v,
            CmpOp::Lt => &t < v,
            CmpOp::Le => &t <= v,
            CmpOp::Between => {
                let hi = &self.lower2;
                let (lo, hi) = if v <= hi { (v, hi) } else { (hi, v) };
                &t >= lo && &t <= hi
            }
            _ => true,
        }
    }

    fn matches_str(&self, text: &str) -> bool {
        let a = self.matches_str_single(text);
        if let Some((is_and, b)) = &self.second {
            if *is_and { a && b.matches_str(text) } else { a || b.matches_str(text) }
        } else {
            a
        }
    }

    #[inline]
    fn matches_value_single(&self, v: &Value) -> bool {
        match v {
            Value::Number(n) if !self.op.is_text() && !self.has_wild => {
                if let Some(b) = self.num1 {
                    self.matches_num(*n, b)
                } else {
                    v.with_display_str(|s| self.matches_str_single(s))
                }
            }
            Value::Text(t) => self.matches_str_single(t),
            Value::Empty => self.matches_str_single(""),
            Value::Bool(b) => self.matches_str_single(if *b { "TRUE" } else { "FALSE" }),
            Value::Error(e) => self.matches_str_single(e.text()),
            Value::Number(_) => v.with_display_str(|s| self.matches_str_single(s)),
        }
    }

    #[inline]
    fn matches_value(&self, v: &Value) -> bool {
        let a = self.matches_value_single(v);
        if let Some((is_and, b)) = &self.second {
            if *is_and { a && b.matches_value(v) } else { a || b.matches_value(v) }
        } else {
            a
        }
    }
}

impl Filter {
    pub fn new(r1: u32, c1: u32, r2: u32, c2: u32) -> Self {
        Self {
            range: (r1, c1, r2, c2),
            allows: Default::default(),
            criteria: HashMap::new(),
            date_groups: HashMap::new(),
            color_filters: HashMap::new(),
            top_filters: HashMap::new(),
            dynamic_filters: HashMap::new(),
        }
    }

    /// Compute hidden rows directly as coalesced intervals (`HiddenIndices`),
    /// matching native `&Value` without per-cell `display() -> parse::<f64>()` formatting.
    pub fn hidden_row_intervals(&self, sheet: &Sheet) -> crate::layout::HiddenIndices {
        let (r1, c1, mut r2, c2) = self.range;
        if (self.allows.is_empty() && self.criteria.is_empty()) || r1 >= r2 {
            return crate::layout::HiddenIndices::default();
        }
        let used_max_r = sheet.used_bounds().map(|(_, _, ur2, _)| ur2).unwrap_or(r1);
        if r2 > used_max_r.saturating_add(64) {
            r2 = used_max_r.saturating_add(1).min(r2);
        }
        if r1 >= r2 {
            return crate::layout::HiddenIndices::default();
        }
        let mut col_ids: Vec<u32> = self
            .allows
            .keys()
            .chain(self.criteria.keys())
            .copied()
            .filter(|&c| c >= c1 && c <= c2)
            .collect();
        col_ids.sort_unstable();
        col_ids.dedup();
        if col_ids.is_empty() {
            return crate::layout::HiddenIndices::default();
        }

        struct ColRule<'a> {
            col: u32,
            allows: Option<&'a HashSet<String>>,
            crit: Option<CompiledCriterion>,
            empty_ok: bool,
        }
        impl<'a> ColRule<'a> {
            #[inline]
            fn passes(&self, v: &Value) -> bool {
                if matches!(v, Value::Empty) {
                    return self.empty_ok;
                }
                if let Some(allows) = self.allows {
                    let ok = match v {
                        Value::Text(t) => allows.contains(t.as_ref()),
                        Value::Bool(b) => allows.contains(if *b { "TRUE" } else { "FALSE" }),
                        Value::Error(e) => allows.contains(e.text()),
                        Value::Number(n) => {
                            crate::model::with_formatted_number(*n, |s| allows.contains(s))
                        }
                        Value::Empty => unreachable!(),
                    };
                    if !ok {
                        return false;
                    }
                }
                if let Some(crit) = &self.crit {
                    return crit.matches_value(v);
                }
                true
            }
        }

        let rules: Vec<ColRule<'_>> = col_ids
            .iter()
            .map(|&col| {
                let allows = self.allows.get(&col);
                let crit = self.criteria.get(&col).map(CompiledCriterion::new);
                let empty_ok = allows.is_none_or(|a| a.contains(""))
                    && crit.as_ref().is_none_or(|c| c.matches_str(""));
                ColRule {
                    col,
                    allows,
                    crit,
                    empty_ok,
                }
            })
            .collect();

        let first = r1 + 1;
        if let Some(req_idx) = rules.iter().position(|r| !r.empty_ok) {
            let req = &rules[req_idx];
            let mut intervals: Vec<(u32, u32)> = Vec::new();
            let mut next = first;
            for (key, cell) in sheet.cells().stored_cells_in((first, req.col, r2, req.col)) {
                let val_cow = cell.value();
                if req.passes(val_cow.as_ref())
                    && rules
                        .iter()
                        .enumerate()
                        .all(|(i, r)| i == req_idx || r.passes(sheet.value_ref(key.row, r.col).as_ref()))
                {
                    if next < key.row {
                        intervals.push((next, key.row - 1));
                    }
                    next = key.row.saturating_add(1);
                }
            }
            if next <= r2 {
                intervals.push((next, r2));
            }
            crate::layout::HiddenIndices::from_intervals(intervals)
        } else {
            let mut rows = Vec::new();
            for rule in &rules {
                for (key, cell) in sheet.cells().stored_cells_in((first, rule.col, r2, rule.col)) {
                    if !rule.passes(cell.value().as_ref()) {
                        rows.push(key.row);
                    }
                }
            }
            rows.into_iter().collect()
        }
    }

    /// 按当前规则算出要隐藏的行（不含表头行）。
    /// 对整列自动筛选（如 `A1:D1048576`），裁剪到工作表实际数据边界（保留 1 行尾部空行语义），
    /// 避免遍历 104 万空行并往 `hidden_rows` 塞入百万条无效行号。
    pub fn hidden_rows(&self, sheet: &Sheet) -> HashSet<u32> {
        let intervals = self.hidden_row_intervals(sheet);
        let mut out = HashSet::with_capacity(intervals.len());
        for &(lo, hi) in intervals.intervals() {
            out.extend(lo..=hi);
        }
        out
    }

    /// 支持日期分组、颜色、Top-N 与动态筛选的完整隐藏行区间计算（需要样式表与日期体系）。
    pub fn hidden_row_intervals_with_styles(
        &self,
        sheet: &Sheet,
        styles: &crate::style::StyleTable,
        sys: crate::model::DateSystem,
    ) -> crate::layout::HiddenIndices {
        if self.date_groups.is_empty()
            && self.color_filters.is_empty()
            && self.top_filters.is_empty()
            && self.dynamic_filters.is_empty()
        {
            return self.hidden_row_intervals(sheet);
        }
        let (r1, c1, mut r2, c2) = self.range;
        if r1 >= r2 {
            return crate::layout::HiddenIndices::default();
        }
        let used_max_r = sheet.used_bounds().map(|(_, _, ur2, _)| ur2).unwrap_or(r1);
        if r2 > used_max_r.saturating_add(64) {
            r2 = used_max_r.saturating_add(1).min(r2);
        }
        if r1 >= r2 {
            return crate::layout::HiddenIndices::default();
        }
        let mut hidden = self.hidden_row_intervals(sheet);

        // 预计算 Top-N 阈值与 Above/Below Average 均值
        let mut top_thresh: HashMap<u32, (bool, f64)> = HashMap::new();
        for (&col, tf) in &self.top_filters {
            if col < c1 || col > c2 {
                continue;
            }
            let mut vals: Vec<f64> = Vec::new();
            for r in (r1 + 1)..=r2 {
                if let Value::Number(v) = sheet.value_ref(r, col).as_ref()
                    && v.is_finite()
                {
                    vals.push(*v);
                }
            }
            if !vals.is_empty() {
                vals.sort_unstable_by(|a, b| a.total_cmp(b));
                let k = if tf.percent {
                    ((vals.len() as f64) * (tf.count.max(1) as f64) / 100.0).ceil().max(1.0) as usize
                } else {
                    (tf.count as usize).max(1)
                }
                .min(vals.len());
                let thr = if tf.top { vals[vals.len() - k] } else { vals[k - 1] };
                top_thresh.insert(col, (tf.top, thr));
            }
        }
        let mut avg_map: HashMap<u32, f64> = HashMap::new();
        let today = crate::cf::today_excel_serial();
        for (&col, df) in &self.dynamic_filters {
            if col < c1 || col > c2 || df.is_date() {
                continue;
            }
            let mut sum = 0.0;
            let mut cnt = 0usize;
            for r in (r1 + 1)..=r2 {
                if let Value::Number(v) = sheet.value_ref(r, col).as_ref()
                    && v.is_finite()
                {
                    sum += *v;
                    cnt += 1;
                }
            }
            if cnt > 0 {
                avg_map.insert(col, sum / cnt as f64);
            }
        }

        let mut extra_hidden: Vec<u32> = Vec::new();
        for r in (r1 + 1)..=r2 {
            if hidden.contains(&r) {
                continue;
            }
            let mut keep = true;
            for (&col, groups) in &self.date_groups {
                if col < c1 || col > c2 || groups.is_empty() {
                    continue;
                }
                let st = styles.get(sheet.style_of(r, col));
                let matched = match sheet.value_ref(r, col).as_ref() {
                    Value::Number(n) if crate::style::is_date_format(&st.format) => {
                        crate::formula::functions::serial_to_date_in(sys, *n)
                            .is_some_and(|(y, m, d)| groups.iter().any(|g| g.matches(y, m, d)))
                    }
                    _ => false,
                };
                if !matched {
                    keep = false;
                    break;
                }
            }
            if keep {
                for (&col, cf) in &self.color_filters {
                    if col < c1 || col > c2 {
                        continue;
                    }
                    let st = styles.get(sheet.style_of(r, col));
                    let matched = match *cf {
                        ColorFilter::Fill(target) => st.fill == target,
                        ColorFilter::Font(target) => st.color == target,
                    };
                    if !matched {
                        keep = false;
                        break;
                    }
                }
            }
            if keep {
                for (&col, &(is_top, thr)) in &top_thresh {
                    let matched = match sheet.value_ref(r, col).as_ref() {
                        Value::Number(v) if v.is_finite() => {
                            if is_top { *v >= thr } else { *v <= thr }
                        }
                        _ => false,
                    };
                    if !matched {
                        keep = false;
                        break;
                    }
                }
            }
            if keep {
                for (&col, df) in &self.dynamic_filters {
                    if col < c1 || col > c2 {
                        continue;
                    }
                    let matched = match sheet.value_ref(r, col).as_ref() {
                        Value::Number(v) if v.is_finite() => match df {
                            DynamicFilter::AboveAverage => avg_map.get(&col).is_some_and(|&a| *v > a),
                            DynamicFilter::BelowAverage => avg_map.get(&col).is_some_and(|&a| *v < a),
                            _ => df.matches_date(*v, today, sys),
                        },
                        _ => false,
                    };
                    if !matched {
                        keep = false;
                        break;
                    }
                }
            }
            if !keep {
                extra_hidden.push(r);
            }
        }
        for r in extra_hidden {
            hidden.insert(r);
        }
        hidden
    }

    pub fn hidden_rows_with_styles(
        &self,
        sheet: &Sheet,
        styles: &crate::style::StyleTable,
        sys: crate::model::DateSystem,
    ) -> HashSet<u32> {
        let intervals = self.hidden_row_intervals_with_styles(sheet, styles, sys);
        let mut out = HashSet::with_capacity(intervals.len());
        for &(lo, hi) in intervals.intervals() {
            out.extend(lo..=hi);
        }
        out
    }

    /// 构造去掉 `except_col` 筛选条件后的副本，用于级联筛选（仅展示其他列已放行行里的候选项）。
    pub fn without_col(&self, except_col: u32) -> Self {
        let mut f = self.clone();
        f.allows.remove(&except_col);
        f.criteria.remove(&except_col);
        f.date_groups.remove(&except_col);
        f.color_filters.remove(&except_col);
        f.top_filters.remove(&except_col);
        f.dynamic_filters.remove(&except_col);
        f
    }

    /// 这列有没有任何筛选（值列表、条件、日期分组、颜色、Top10 或动态筛选）
    pub fn is_filtered(&self, col: u32) -> bool {
        self.allows.get(&col).map(|a| !a.is_empty()).unwrap_or(false)
            || self.criteria.contains_key(&col)
            || self.date_groups.get(&col).is_some_and(|g| !g.is_empty())
            || self.color_filters.contains_key(&col)
            || self.top_filters.contains_key(&col)
            || self.dynamic_filters.contains_key(&col)
    }

    /// 任意列是否存在活动筛选
    pub fn has_any_active(&self) -> bool {
        !self.allows.is_empty()
            || !self.criteria.is_empty()
            || self.date_groups.values().any(|g| !g.is_empty())
            || !self.color_filters.is_empty()
            || !self.top_filters.is_empty()
            || !self.dynamic_filters.is_empty()
    }
}

/// 级联筛选候选项：仅收集满足**其他列**筛选条件的行，同时返回 `(原始显示键, 带数字/日期格式展示标签)`。
pub fn cascading_column_choices(
    sheet: &Sheet,
    filter: &Filter,
    col: u32,
    styles: &crate::style::StyleTable,
    sys: crate::model::DateSystem,
) -> (Vec<(String, String)>, bool) {
    let other = filter.without_col(col);
    let other_hidden = if other.has_any_active() {
        Some(other.hidden_row_intervals_with_styles(sheet, styles, sys))
    } else {
        None
    };
    let (r1, _, mut r2, _) = filter.range;
    let used_max_r = sheet.used_bounds().map(|(_, _, ur2, _)| ur2).unwrap_or(r1);
    if r2 > used_max_r.saturating_add(64) {
        r2 = used_max_r.saturating_add(1).min(r2);
    }
    let mut map: HashMap<String, String> = HashMap::new();
    let mut all_numeric = true;
    let mut any_nonempty = false;
    let cap_end = r2.min(r1.saturating_add(200_000));
    for r in (r1 + 1)..=cap_end {
        if other_hidden.as_ref().is_some_and(|h| h.contains(&r)) {
            continue;
        }
        let v = sheet.value_ref(r, col);
        let raw_key = v.display();
        if !raw_key.is_empty() {
            any_nonempty = true;
            if !matches!(v.as_ref(), Value::Number(_)) && raw_key.trim().parse::<f64>().is_err() {
                all_numeric = false;
            }
        }
        map.entry(raw_key.clone()).or_insert_with(|| {
            if raw_key.is_empty() {
                String::new()
            } else {
                let fmt = &styles.get(sheet.style_of(r, col)).format;
                if fmt.is_empty() || fmt.eq_ignore_ascii_case("General") {
                    raw_key
                } else {
                    crate::style::format_value_in(sys, v.as_ref(), fmt)
                }
            }
        });
    }
    let mut pairs: Vec<(Option<f64>, String, String)> = map
        .into_iter()
        .map(|(k, lbl)| (k.trim().parse::<f64>().ok(), k, lbl))
        .collect();
    pairs.sort_unstable_by(|(an, a, _), (bn, b, _)| match (an, bn) {
        (Some(x), Some(y)) => x.total_cmp(y).then_with(|| a.cmp(b)),
        (Some(_), None) if !b.is_empty() => Ordering::Less,
        (None, Some(_)) if !a.is_empty() => Ordering::Greater,
        _ => a.cmp(b),
    });
    let out = pairs.into_iter().map(|(_, k, lbl)| (k, lbl)).collect();
    (out, any_nonempty && all_numeric)
}

/// 高级筛选结果
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AdvFilterReport {
    pub matched: usize,
    pub total: usize,
    pub copied_range: Option<(u32, u32, u32, u32)>,
}

fn parse_adv_cell_criterion(expr: &str) -> Criterion {
    let s = expr.trim();
    if let Some(rest) = s.strip_prefix(">=") {
        Criterion::new(CmpOp::Ge, rest.trim())
    } else if let Some(rest) = s.strip_prefix("<=") {
        Criterion::new(CmpOp::Le, rest.trim())
    } else if let Some(rest) = s.strip_prefix("<>") {
        Criterion::new(CmpOp::Ne, rest.trim())
    } else if let Some(rest) = s.strip_prefix('>') {
        Criterion::new(CmpOp::Gt, rest.trim())
    } else if let Some(rest) = s.strip_prefix('<') {
        Criterion::new(CmpOp::Lt, rest.trim())
    } else if let Some(rest) = s.strip_prefix('=') {
        Criterion::new(CmpOp::Eq, rest.trim())
    } else if pattern_has_wildcard(s) {
        Criterion::new(CmpOp::Eq, s)
    } else {
        Criterion::new(CmpOp::Eq, s)
    }
}

/// Excel 高级筛选：支持条件区域（同行 AND、多行 OR、比较运算符与 `*`/`?` 通配符）、原地隐藏或复制到目标位置、选择不重复记录。
pub fn advanced_filter(
    sheet: &mut Sheet,
    list_range: (u32, u32, u32, u32),
    criteria_range: Option<(u32, u32, u32, u32)>,
    copy_to: Option<(u32, u32)>,
    unique_only: bool,
) -> Result<AdvFilterReport, String> {
    let (lr1, lc1, lr2, lc2) = list_range;
    if lr2 <= lr1 || lc2 < lc1 {
        return Err("列表区域至少包含表头和一行数据".into());
    }
    let headers: Vec<String> = (lc1..=lc2)
        .map(|c| sheet.value(lr1, c).display().trim().to_lowercase())
        .collect();

    // 解析条件区域：每行是一条 OR 分支，行内每个非空列是 AND 约束
    let mut or_branches: Vec<Vec<(u32, Criterion)>> = Vec::new();
    if let Some((cr1, cc1, cr2, cc2)) = criteria_range {
        if cr2 <= cr1 {
            return Err("条件区域至少包含标题行和一行条件".into());
        }
        let crit_cols: Vec<Option<u32>> = (cc1..=cc2)
            .map(|c| {
                let h = sheet.value(cr1, c).display().trim().to_lowercase();
                headers.iter().position(|lh| !h.is_empty() && lh == &h).map(|off| lc1 + off as u32)
            })
            .collect();
        for r in (cr1 + 1)..=cr2 {
            let mut and_terms: Vec<(u32, Criterion)> = Vec::new();
            for (idx, c) in (cc1..=cc2).enumerate() {
                let raw = sheet.value(r, c).display();
                if raw.trim().is_empty() {
                    continue;
                }
                if let Some(target_col) = crit_cols[idx] {
                    and_terms.push((target_col, parse_adv_cell_criterion(&raw)));
                }
            }
            if !and_terms.is_empty() {
                or_branches.push(and_terms);
            }
        }
    }

    let mut matched_rows: Vec<u32> = Vec::new();
    let mut seen_unique: HashSet<Vec<String>> = HashSet::new();
    let total = (lr2 - lr1) as usize;
    for r in (lr1 + 1)..=lr2 {
        let passes = if or_branches.is_empty() {
            true
        } else {
            or_branches.iter().any(|branch| {
                branch.iter().all(|(col, crit)| {
                    let v = sheet.value(r, *col).display();
                    crit.matches(&v)
                })
            })
        };
        if !passes {
            continue;
        }
        if unique_only {
            let sig: Vec<String> = (lc1..=lc2).map(|c| sheet.value(r, c).display()).collect();
            if !seen_unique.insert(sig) {
                continue;
            }
        }
        matched_rows.push(r);
    }

    if let Some((dr, dc)) = copy_to {
        // 复制表头 + 命中行到目标位置
        let width = lc2 - lc1 + 1;
        for off in 0..width {
            let src_c = lc1 + off;
            if let Some(cell) = sheet.get(lr1, src_c) {
                sheet.set(dr, dc + off, cell.into_owned());
            }
            let sid = sheet.style_of(lr1, src_c);
            if sid != 0 {
                sheet.set_style(dr, dc + off, sid);
            }
        }
        for (i, &src_r) in matched_rows.iter().enumerate() {
            let dst_r = dr + 1 + i as u32;
            for off in 0..width {
                let src_c = lc1 + off;
                let cell = sheet.get(src_r, src_c).map(|c| c.into_owned()).unwrap_or_default();
                sheet.set(dst_r, dc + off, cell);
                let sid = sheet.style_of(src_r, src_c);
                sheet.set_style(dst_r, dc + off, sid);
            }
        }
        let end_r = dr + matched_rows.len() as u32;
        let end_c = dc + width - 1;
        sheet.edited = true;
        Ok(AdvFilterReport {
            matched: matched_rows.len(),
            total,
            copied_range: Some((dr, dc, end_r, end_c)),
        })
    } else {
        // 原地筛选：隐藏未命中的行
        let kept: HashSet<u32> = matched_rows.iter().copied().collect();
        let mut hidden = crate::layout::HiddenIndices::default();
        for r in (lr1 + 1)..=lr2 {
            if !kept.contains(&r) {
                hidden.insert(r);
            }
        }
        sheet.hidden_rows = hidden;
        sheet.filter_dirty = true;
        Ok(AdvFilterReport {
            matched: matched_rows.len(),
            total,
            copied_range: None,
        })
    }
}

/// 扫描筛选列内的日期层级（年 -> 月 -> 日）以及出现的填充色/字体色。
pub fn collect_column_dates_and_colors(
    sheet: &Sheet,
    styles: &crate::style::StyleTable,
    sys: crate::model::DateSystem,
    range: (u32, u32, u32, u32),
    col: u32,
) -> ColumnFilterMeta {
    let (r1, _, mut r2, _) = range;
    let used_max_r = sheet.used_bounds().map(|(_, _, ur2, _)| ur2).unwrap_or(r1);
    if r2 > used_max_r.saturating_add(64) {
        r2 = used_max_r.saturating_add(1).min(r2);
    }
    let mut meta = ColumnFilterMeta::default();
    if r1 >= r2 {
        return meta;
    }
    let mut seen_fills: HashSet<Option<[u8; 3]>> = HashSet::new();
    let mut seen_fonts: HashSet<[u8; 3]> = HashSet::new();
    let cap_rows = (r2 - r1).min(200_000);
    for r in (r1 + 1)..=(r1 + cap_rows) {
        let sid = sheet.style_of(r, col);
        let st = styles.get(sid);
        if seen_fills.insert(st.fill) {
            meta.fill_colors.push(st.fill);
        }
        if seen_fonts.insert(st.color) {
            meta.font_colors.push(st.color);
        }
        if crate::style::is_date_format(&st.format)
            && let Value::Number(n) = sheet.value_ref(r, col).as_ref()
            && let Some((y, m, d)) = crate::formula::functions::serial_to_date_in(sys, *n)
        {
            meta.dates.entry(y).or_default().entry(m).or_default().insert(d);
        }
    }
    meta
}

/// 某列在筛选区域里的去重值（按显示文本，排序后返回）
pub fn unique_values(sheet: &Sheet, range: (u32, u32, u32, u32), col: u32) -> Vec<String> {
    FilterColumn::new(sheet, range, col).choices().0
}

/// Shares the source column's pages; a pending popup never retains drawings or other columns.
#[derive(Clone)]
pub struct FilterColumn {
    pages: Vec<(u32, std::sync::Arc<crate::store::ColSlab>)>,
    header: u32,
    last: u32,
}

impl FilterColumn {
    pub fn new(sheet: &Sheet, range: (u32, u32, u32, u32), col: u32) -> Self {
        let mut pages = sheet.column_pages(col);
        pages.retain(|(base, cells)| *base <= range.2 && base.saturating_add(cells.len() as u32) > range.0.saturating_add(1));
        pages.sort_unstable_by_key(|(base, _)| *base);
        Self { pages, header: range.0, last: range.2 }
    }

    pub fn same_pages(&self, other: &Self) -> bool {
        self.header == other.header && self.last == other.last && self.pages.len() == other.pages.len()
            && self.pages.iter().zip(&other.pages).all(|((a, x), (b, y))| a == b && std::sync::Arc::ptr_eq(x, y))
    }

    pub fn choices(self) -> (Vec<String>, bool) {
        self.choices_while(|| true).unwrap()
    }

    pub fn choices_while(self, keep: impl Fn() -> bool) -> Option<(Vec<String>, bool)> {
        let mut unique: FxHashSet<String> = FxHashSet::default();
        let mut seen_ptrs: FxHashSet<usize> = FxHashSet::default();
        let mut seen_num_bits: FxHashSet<u64> = FxHashSet::default();
        let mut seen_empty = false;
        let mut seen_true = false;
        let mut seen_false = false;
        let mut populated = 0u32;

        let mut insert_val = |v: &Value| {
            match v {
                Value::Empty => {
                    if !seen_empty {
                        seen_empty = true;
                        unique.insert(String::new());
                    }
                }
                Value::Number(n) => {
                    let nb = if *n == 0.0 { 0u64 } else { n.to_bits() };
                    if seen_num_bits.insert(nb) {
                        unique.insert(crate::model::format_number(*n));
                    }
                }
                Value::Text(s) => {
                    let ptr = s.as_ptr() as usize;
                    if seen_ptrs.insert(ptr) && !unique.contains(s.as_ref()) {
                        unique.insert(s.to_string());
                    }
                }
                Value::Bool(true) => {
                    if !seen_true {
                        seen_true = true;
                        unique.insert("TRUE".to_string());
                    }
                }
                Value::Bool(false) => {
                    if !seen_false {
                        seen_false = true;
                        unique.insert("FALSE".to_string());
                    }
                }
                Value::Error(e) => {
                    let t = e.text();
                    if !unique.contains(t) {
                        unique.insert(t.to_string());
                    }
                }
            }
        };

        for (base, cells) in self.pages {
            if !keep() { return None; }
            let start = self.header.saturating_add(1).saturating_sub(base) as usize;
            if self.last < base || start >= cells.len() { continue; }
            let end = ((self.last - base) as usize + 1).min(cells.len());
            if start >= end { continue; }
            let hi = end - 1;
            let start_w = start / 64;
            let end_w = hi / 64;
            match cells.as_ref() {
                crate::store::ColSlab::Numbers(ns) => {
                    for w in start_w..=end_w {
                        let mut bits = ns.present[w];
                        if w == start_w {
                            bits &= !0u64 << (start % 64);
                        }
                        if w == end_w && (hi % 64) < 63 {
                            bits &= (1u64 << ((hi % 64) + 1)) - 1;
                        }
                        populated += bits.count_ones();
                        while bits != 0 {
                            let tz = bits.trailing_zeros() as usize;
                            bits &= bits - 1;
                            insert_val(&Value::Number(ns.values[w * 64 + tz]));
                        }
                    }
                }
                crate::store::ColSlab::Values(vs) => {
                    if let Some((codes, dict)) = vs.dict_text() {
                        populated += crate::store::NumberSlab::present_count_in(
                            &vs.present,
                            start,
                            end - start,
                        ) as u32;
                        let mut seen_codes = [false; 129];
                        for &code in &codes[start..end] {
                            seen_codes[code as usize] = true;
                        }
                        for (code, seen) in seen_codes.iter().enumerate().take(dict.len()).skip(1) {
                            if *seen {
                                insert_val(&dict[code]);
                            }
                        }
                    } else {
                        for w in start_w..=end_w {
                            let mut bits = vs.present[w];
                            if w == start_w {
                                bits &= !0u64 << (start % 64);
                            }
                            if w == end_w && (hi % 64) < 63 {
                                bits &= (1u64 << ((hi % 64) + 1)) - 1;
                            }
                            populated += bits.count_ones();
                            while bits != 0 {
                                let tz = bits.trailing_zeros() as usize;
                                bits &= bits - 1;
                                if let Some(v) = vs.get_val(w * 64 + tz) {
                                    insert_val(v);
                                }
                            }
                        }
                    }
                }
                crate::store::ColSlab::Mixed(cs) => {
                    let pm = cs.row_mask();
                    for w in start_w..=end_w {
                        let mut bits = pm[w];
                        if w == start_w {
                            bits &= !0u64 << (start % 64);
                        }
                        if w == end_w && (hi % 64) < 63 {
                            bits &= (1u64 << ((hi % 64) + 1)) - 1;
                        }
                        populated += bits.count_ones();
                        while bits != 0 {
                            let tz = bits.trailing_zeros() as usize;
                            bits &= bits - 1;
                            if let Some(cell) = cs.get_cell(w * 64 + tz) {
                                insert_val(&cell.value);
                            }
                        }
                    }
                }
            }
        }
        if populated < self.last.saturating_sub(self.header) {
            unique.insert(String::new());
        }
        let mut nonempty = unique.iter().map(|s| s.trim()).filter(|s| !s.is_empty()).peekable();
        let numeric = nonempty.peek().is_some() && nonempty.all(|s| s.parse::<f64>().is_ok());
        // A total ordering also handles mixed numeric and text columns consistently.
        let mut choices: Vec<_> = unique.into_iter().map(|s| (s.parse::<f64>().ok(), s)).collect();
        if !keep() { return None; }
        choices.sort_unstable_by(|(an, a), (bn, b)| {
            use std::cmp::Ordering;
            match (an, bn) {
                (Some(x), Some(y)) => x.total_cmp(y).then_with(|| a.cmp(b)),
                (Some(_), None) if !b.is_empty() => Ordering::Less,
                (None, Some(_)) if !a.is_empty() => Ordering::Greater,
                _ => a.cmp(b),
            }
        });
        keep().then(|| (choices.into_iter().map(|(_, s)| s).collect(), numeric))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Cell;

    fn sheet() -> Sheet {
        let mut s = Sheet::new("T");
        let rows = [
            ("张三", 30.0),
            ("李四", 10.0),
            ("王五", 20.0),
            ("赵六", 0.0),
        ];
        for (i, (n, v)) in rows.iter().enumerate() {
            let r = (i + 1) as u32;
            s.set(r, 0, Cell { value: Value::Text((*n).into()), formula: None });
            s.set(r, 1, Cell { value: Value::Number(*v), formula: None });
        }
        s.set(0, 0, Cell { value: Value::Text("姓名".into()), formula: None });
        s.set(0, 1, Cell { value: Value::Text("分数".into()), formula: None });
        s
    }

    #[test]
    fn criteria_filter_by_text_and_number() {
        let s = sheet();
        let mut f = Filter::new(0, 0, 4, 1);
        f.criteria.insert(0, Criterion::new(CmpOp::Contains, "张"));
        let hid = f.hidden_rows(&s);
        assert_eq!(hid.len(), 3, "只有张三含「张」");

        let mut f2 = Filter::new(0, 0, 4, 1);
        f2.criteria.insert(1, Criterion::new(CmpOp::Gt, "15"));
        let hid2 = f2.hidden_rows(&s);
        // 分数 30/10/20/0 → 大于 15 只有 30、20
        assert_eq!(hid2.len(), 2);

        let mut f3 = Filter::new(0, 0, 4, 1);
        f3.criteria.insert(1, Criterion::between("5", "25"));
        assert_eq!(f3.hidden_rows(&s).len(), 2, "10、20 落在 5..25");

        let mut f4 = Filter::new(0, 0, 4, 1);
        f4.criteria.insert(0, Criterion::new(CmpOp::Begins, "李"));
        assert_eq!(f4.hidden_rows(&s).len(), 3);
    }

    #[test]
    fn criterion_xml_operator_roundtrip() {
        for op in [
            CmpOp::Eq,
            CmpOp::Ne,
            CmpOp::Gt,
            CmpOp::Ge,
            CmpOp::Lt,
            CmpOp::Le,
            CmpOp::Between,
            CmpOp::Contains,
            CmpOp::NotContains,
            CmpOp::Begins,
            CmpOp::Ends,
        ] {
            assert_eq!(CmpOp::from_xml(op.xml_operator()), Some(op), "{op:?}");
        }
    }

    #[test]
    fn sorts_ascending_and_descending() {
        let mut s = sheet();
        sort_range(&mut s, 1, 0, 4, 1, 1, true).unwrap();
        let names: Vec<String> = (1..=4)
            .map(|r| s.get(r, 0).map(|c| c.value.display()).unwrap_or_default())
            .collect();
        assert_eq!(names, vec!["赵六", "李四", "王五", "张三"], "按分数升序");
        sort_range(&mut s, 1, 0, 4, 1, 1, false).unwrap();
        let names: Vec<String> = (1..=4)
            .map(|r| s.get(r, 0).map(|c| c.value.display()).unwrap_or_default())
            .collect();
        assert_eq!(names, vec!["张三", "王五", "李四", "赵六"], "降序");
    }

    #[test]
    fn blanks_go_last() {
        let mut s = sheet();
        s.remove(3, 1); // 王五那行分数留空（第 3 行是王五）
        sort_range(&mut s, 1, 0, 4, 1, 1, true).unwrap();
        let last = s.get(4, 0).map(|c| c.value.display()).unwrap_or_default();
        assert_eq!(last, "王五", "空格应排到最后，不受升降序影响");
    }

    #[test]
    fn styles_move_with_rows() {
        let mut s = sheet();
        let mut st = crate::style::CellStyle::default();
        st.bold = true;
        let id = {
            let mut t = crate::style::StyleTable::new();
            t.intern(st)
        };
        // 给王五（第 3 行，20 分）加样式
        s.set_style_loaded(3, 0, id);
        sort_range(&mut s, 1, 0, 4, 1, 1, true).unwrap();
        // 排序后找到王五所在行，样式应跟着它
        let mut found = None;
        for r in 1..=4 {
            if s.get(r, 0).map(|c| c.value.display()) == Some("王五".to_string()) {
                found = Some(r);
            }
        }
        let r = found.expect("王五应在表内");
        assert_eq!(s.style_of(r, 0), id, "样式应跟着王五走");
    }

    #[test]
    fn rejects_merged_range() {
        let mut s = sheet();
        s.merge_range(1, 0, 2, 0);
        assert!(sort_range(&mut s, 1, 0, 4, 1, 1, true).is_err());
    }

    #[test]
    fn filter_hides_non_matching_rows() {
        let s = sheet();
        let mut f = Filter::new(0, 0, 4, 1);
        let mut allow = HashSet::new();
        allow.insert("张三".to_string());
        allow.insert("王五".to_string());
        f.allows.insert(0, allow);
        let hidden = f.hidden_rows(&s);
        assert!(hidden.contains(&2), "李四应被隐藏");
        assert!(hidden.contains(&4), "赵六应被隐藏");
        assert!(!hidden.contains(&1), "张三应保留");
        assert!(!hidden.contains(&0), "表头不参与");
    }

    #[test]
    fn region_expands_to_contiguous_block() {
        let s = sheet();
        // 从中间任意一格出发应拿到 A1:B5（0 基：(0,0)-(4,1)）
        assert_eq!(current_region(&s, 2, 1), (0, 0, 4, 1));
        assert_eq!(current_region(&s, 0, 0), (0, 0, 4, 1));
        // 远处空格自成一块
        let (r1, c1, r2, c2) = current_region(&s, 20, 5);
        assert_eq!((r1, c1, r2, c2), (20, 5, 20, 5));
    }

    #[test]
    fn sparse_filter_preserves_blank_criteria_and_one_trailing_row() {
        let mut sh = Sheet::new("Sparse");
        let cell = |s: &str| Cell { value: Value::Text(s.into()), formula: None };
        sh.set(0, 0, cell("Header"));
        sh.set(17, 0, cell("drop"));
        sh.set(9000, 0, cell("keep"));
        sh.set(9000, 1, cell("no"));
        let mut f = Filter::new(0, 0, 1_048_575, 1);
        f.allows.insert(0, ["".into(), "keep".into()].into_iter().collect());
        assert_eq!(f.hidden_rows(&sh), [17].into_iter().collect());
        f.allows.insert(1, ["".into()].into_iter().collect());
        assert_eq!(f.hidden_rows(&sh), [17,9000].into_iter().collect());
        f.allows.insert(0, ["keep".into()].into_iter().collect());
        assert_eq!(f.hidden_rows(&sh), (1..=9001).collect());
        f.allows.remove(&1);
        let hidden = f.hidden_rows(&sh);
        assert_eq!(hidden.len(), 9000);
        assert!(hidden.contains(&9001) && !hidden.contains(&9000));
    }

    #[test]
    fn unique_values_sorted() {
        let s = sheet();
        let u = unique_values(&s, (0, 0, 4, 1), 0);
        assert_eq!(u, vec!["张三", "李四", "王五", "赵六"]);
    }
    #[test]
    fn sort_region_by_row_reorders_columns_and_keeps_header_col() {
        let mut s = Sheet::new("S");
        // A 列是行标题（文本），B..D 是数据
        let rows = [
            ("一月", 30.0, 10.0, 20.0),
            ("二月", 5.0, 40.0, 15.0),
            ("三月", 12.0, 7.0, 25.0),
        ];
        for (i, (label, a, b, c)) in rows.iter().enumerate() {
            let r = i as u32;
            s.set(r, 0, crate::model::Cell { value: Value::Text((*label).into()), formula: None });
            s.set(r, 1, crate::model::Cell { value: Value::Number(*a), formula: None });
            s.set(r, 2, crate::model::Cell { value: Value::Number(*b), formula: None });
            s.set(r, 3, crate::model::Cell { value: Value::Number(*c), formula: None });
        }
        // 以第 3 行（r=2，值 12 / 7 / 25）为关键字升序 → 列顺序应收成 7,12,25
        sort_region_by_row(&mut s, (0, 0, 2, 3), 2, true).unwrap();
        let row2: Vec<String> = (0..4).map(|c| s.get(2, c).map(|x| x.value.display()).unwrap_or_default()).collect();
        assert_eq!(row2, vec!["三月", "7", "12", "25"], "标题列不动，数据列按第 3 行升序");
        let row0: Vec<String> = (1..4).map(|c| s.get(0, c).map(|x| x.value.display()).unwrap_or_default()).collect();
        assert_eq!(row0, vec!["10", "30", "20"], "整列跟着移动");
    }

    #[test]
    fn sort_region_by_row_rejects_key_row_outside() {
        let mut s = Sheet::new("S");
        s.set(0, 0, crate::model::Cell { value: Value::Number(1.0), formula: None });
        s.set(0, 1, crate::model::Cell { value: Value::Number(2.0), formula: None });
        assert!(sort_region_by_row(&mut s, (0, 0, 0, 1), 5, true).is_err());
    }

    #[test]
    fn wave5_choices_while_short_circuits_dict_text_and_number_bits_with_blanks() {
        let mut s = Sheet::new("Choices");
        s.set(0, 0, crate::model::Cell { value: Value::Text("状态".into()), formula: None });
        s.set(0, 1, crate::model::Cell { value: Value::Text("金额".into()), formula: None });
        let items: [std::sync::Arc<str>; 3] = ["已完成".into(), "处理中".into(), "待审核".into()];
        for r in 1..=600u32 {
            if r != 300 {
                s.set(
                    r,
                    0,
                    crate::model::Cell {
                        value: Value::Text(items[(r as usize) % 3].clone()),
                        formula: None,
                    },
                );
            }
            let n = if r % 2 == 0 { -0.0 } else { 12.5 };
            s.set(r, 1, crate::model::Cell { value: Value::Number(n), formula: None });
        }
        let (text_choices, text_numeric) = FilterColumn::new(&s, (0, 0, 600, 1), 0).choices();
        assert!(!text_numeric);
        assert!(text_choices.iter().any(String::is_empty), "row 300 blank must insert empty choice");
        assert_eq!(text_choices.len(), 4);

        let (num_choices, num_numeric) = FilterColumn::new(&s, (0, 0, 600, 1), 1).choices();
        assert!(num_numeric);
        assert_eq!(num_choices, vec!["0".to_string(), "12.5".to_string()]);

        // Column 2: 100% populated by unevaluated formulas (`formula: Some`, cached `Value::Empty`)
        // must still include `""` in `choices()`.
        s.set(0, 2, crate::model::Cell { value: Value::Text("待算".into()), formula: None });
        for r in 1..=600u32 {
            s.set(
                r,
                2,
                crate::model::Cell {
                    value: Value::Empty,
                    formula: Some("=B2".into()),
                },
            );
        }
        let (empty_f_choices, empty_f_numeric) = FilterColumn::new(&s, (0, 0, 600, 2), 2).choices();
        assert!(!empty_f_numeric);
        assert_eq!(empty_f_choices, vec![String::new()]);
    }
}
