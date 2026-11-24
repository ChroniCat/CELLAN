//! 数据透视表：按行字段（可选列字段）分组，对值字段做汇总，结果写成一块普通单元格。
//!
//! 说明：这里生成的是「值化」的透视结果（Excel 里叫「粘贴为值」），
//! 不会创建 Excel 的 pivotTable 对象，所以导出的文件在任何工具里都能看到数字，
//! 但不能在 Excel 里拖字段重组。这是有意的取舍：真 pivotTable 需要
//! pivotCacheDefinition + pivotTableDefinition 两套部件，投入产出不划算。

use crate::model::{Cell, Sheet, Value};
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Agg {
    Sum,
    Count,
    Avg,
    Max,
    Min,
}

impl Agg {
    pub fn label(&self) -> &'static str {
        match self {
            Agg::Sum => "求和",
            Agg::Count => "计数",
            Agg::Avg => "平均值",
            Agg::Max => "最大值",
            Agg::Min => "最小值",
        }
    }
}

/// 行字段的分组方式
#[derive(Clone, Debug, PartialEq)]
pub enum GroupBy {
    None,
    /// 日期按年
    Year,
    /// 日期按月（yyyy-mm）
    Month,
    /// 数值按区间（桶宽）
    Bucket(f64),
}

impl Default for GroupBy {
    fn default() -> Self {
        GroupBy::None
    }
}

impl GroupBy {
    pub fn label(&self) -> String {
        match self {
            GroupBy::None => "不分组".into(),
            GroupBy::Year => "按年".into(),
            GroupBy::Month => "按月".into(),
            GroupBy::Bucket(n) => format!("每 {n} 一档"),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PivotSpec {
    /// 源区域（含表头行）
    pub source: (u32, u32, u32, u32),
    /// 行字段（列号）
    pub row_field: u32,
    /// 列字段（列号），None 表示只有行
    pub col_field: Option<u32>,
    /// 值字段（列号）
    pub value_field: u32,
    pub agg: Agg,
    /// 行字段的分组
    pub group: GroupBy,
    /// 排除的行标签（分组后的字符串）
    pub exclude: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PivotResult {
    /// 表头：第一列是行字段名，其余是列字段的取值
    pub headers: Vec<String>,
    /// 数据行：第一格是行标签，其余是汇总值
    pub rows: Vec<Vec<Value>>,
    /// 行字段名
    pub row_name: String,
    pub value_name: String,
    /// 列字段名（有列字段时 Excel 的第二行表头要用它）
    pub col_name: Option<String>,
    /// 值字段标题，形如「求和项:金额」
    pub value_caption: String,
}

/// 计算透视结果
/// 把行字段的原始值按分组方式变成标签
pub fn group_key(v: &Value, g: &GroupBy) -> String {
    match g {
        GroupBy::None => v.display(),
        GroupBy::Year | GroupBy::Month => {
            let (y, m) = match v {
                Value::Number(serial) => match crate::formula::serial_to_date_pub(*serial) {
                    Some((y, m, _)) => (y, m),
                    None => return v.display(),
                },
                Value::Text(t) => {
                    let t = t.trim();
                    let mut it = t.split(['-', '/', '年', '月']);
                    let y = it.next().and_then(|s| s.parse::<i32>().ok());
                    let m = it.next().and_then(|s| s.parse::<u32>().ok());
                    match (y, m) {
                        (Some(y), Some(m)) => (y, m),
                        _ => return v.display(),
                    }
                }
                _ => return v.display(),
            };
            if matches!(g, GroupBy::Year) {
                format!("{y}")
            } else {
                format!("{y}-{m:02}")
            }
        }
        GroupBy::Bucket(w) => {
            let n = match v {
                Value::Number(n) => *n,
                Value::Text(t) => t.trim().parse::<f64>().unwrap_or(0.0),
                _ => 0.0,
            };
            if *w <= 0.0 {
                return v.display();
            }
            let b = (n / w).floor() * w;
            let show = |v: f64| {
                if v.fract().abs() < 1e-9 {
                    format!("{}", v as i64)
                } else {
                    format!("{v}")
                }
            };
            format!("{}~{}", show(b), show(b + w))
        }
    }
}

/// 行字段的去重取值（按 spec.group 归一后的标签，与 compute 里的 key 一致）。
/// 切片器用它列出可勾选项；exclude 里的项也照样返回（勾选状态由调用方决定）。
pub fn row_values(sheet: &Sheet, spec: &PivotSpec) -> Vec<String> {
    let (r1, _, r2, _) = spec.source;
    let first = r1.saturating_add(1);
    if first > r2 {
        return Vec::new();
    }
    let mut labels: crate::sort_filter::FxHashSet<String> = Default::default();
    let mut seen_ptrs: crate::sort_filter::FxHashSet<usize> = Default::default();
    let mut seen_nums: crate::sort_filter::FxHashSet<u64> = Default::default();
    let p1 = first / crate::store::PAGE_ROWS;
    let p2 = r2 / crate::store::PAGE_ROWS;
    for (pi, page) in sheet.cells().iter_pages_in(p1, p2) {
        let Some(slab) = page.cols.get(spec.row_field) else {
            continue;
        };
        let base = pi * crate::store::PAGE_ROWS;
        let start = first.saturating_sub(base) as usize;
        let end = ((r2 - base).min(crate::store::PAGE_ROWS - 1) as usize + 1).min(slab.len());
        if start >= end {
            continue;
        }
        let hi = end - 1;
        let start_w = start / 64;
        let end_w = hi / 64;
        let mut add_val = |v: &Value| {
            match v {
                Value::Empty => {}
                Value::Text(s) => {
                    if seen_ptrs.insert(s.as_ptr() as usize) {
                        let key = group_key(v, &spec.group);
                        if !key.is_empty() {
                            labels.insert(key);
                        }
                    }
                }
                Value::Number(n) => {
                    let nb = if *n == 0.0 { 0u64 } else { n.to_bits() };
                    if seen_nums.insert(nb) {
                        let key = group_key(v, &spec.group);
                        if !key.is_empty() {
                            labels.insert(key);
                        }
                    }
                }
                _ => {
                    let key = group_key(v, &spec.group);
                    if !key.is_empty() {
                        labels.insert(key);
                    }
                }
            }
        };
        match slab.as_ref() {
            crate::store::ColSlab::Numbers(ns) => {
                for w in start_w..=end_w {
                    let mut bits = ns.present[w];
                    if w == start_w {
                        bits &= !0u64 << (start % 64);
                    }
                    if w == end_w && (hi % 64) < 63 {
                        bits &= (1u64 << ((hi % 64) + 1)) - 1;
                    }
                    while bits != 0 {
                        let tz = bits.trailing_zeros() as usize;
                        bits &= bits - 1;
                        let n = ns.values[w * 64 + tz];
                        let nb = if n == 0.0 { 0u64 } else { n.to_bits() };
                        if seen_nums.insert(nb) {
                            let key = group_key(&Value::Number(n), &spec.group);
                            if !key.is_empty() {
                                labels.insert(key);
                            }
                        }
                    }
                }
            }
            crate::store::ColSlab::Values(vs) => {
                if let Some((codes, dict)) = vs.dict_text() {
                    let mut page_codes = [false; 129];
                    for &code in &codes[start..end] {
                        page_codes[code as usize] = true;
                    }
                    for (code, seen) in page_codes.iter().enumerate().take(dict.len()).skip(1) {
                        if *seen {
                            add_val(&dict[code]);
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
                        while bits != 0 {
                            let tz = bits.trailing_zeros() as usize;
                            bits &= bits - 1;
                            if let Some(v) = vs.get_val(w * 64 + tz) {
                                add_val(v);
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
                    while bits != 0 {
                        let tz = bits.trailing_zeros() as usize;
                        bits &= bits - 1;
                        if let Some(cell) = cs.get_cell(w * 64 + tz) {
                            add_val(&cell.value);
                        }
                    }
                }
            }
        }
    }
    let mut out: Vec<_> = labels.into_iter().collect();
    out.sort_unstable();
    out
}

/// Streaming scalar accumulator: replaces retaining 3-4 copies of `Vec<PivotVal>` per cell.
#[derive(Clone, Copy, Debug)]
struct PivotAcc {
    nonempty: u64,
    numeric: u64,
    sum: f64,
    min: f64,
    max: f64,
}

impl Default for PivotAcc {
    fn default() -> Self {
        Self {
            nonempty: 0,
            numeric: 0,
            sum: 0.0,
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
        }
    }
}

impl PivotAcc {
    #[inline]
    fn push(&mut self, n: Option<f64>, nonempty: bool, agg: Agg) {
        if agg == Agg::Count {
            if nonempty {
                self.nonempty += 1;
            }
        } else if let Some(v) = n {
            self.nonempty += 1;
            self.numeric += 1;
            self.sum += v;
            self.min = self.min.min(v);
            self.max = self.max.max(v);
        }
    }

    #[inline]
    fn merge(&mut self, other: &Self) {
        self.nonempty += other.nonempty;
        self.numeric += other.numeric;
        self.sum += other.sum;
        self.min = self.min.min(other.min);
        self.max = self.max.max(other.max);
    }

    #[inline]
    fn finish(&self, agg: Agg) -> Value {
        if agg == Agg::Count {
            if self.nonempty == 0 {
                Value::Empty
            } else {
                Value::Number(self.nonempty as f64)
            }
        } else if self.numeric == 0 {
            Value::Empty
        } else {
            match agg {
                Agg::Sum => Value::Number(self.sum),
                Agg::Avg => Value::Number(self.sum / self.numeric as f64),
                Agg::Max => Value::Number(self.max),
                Agg::Min => Value::Number(self.min),
                Agg::Count => unreachable!(),
            }
        }
    }
}

pub fn compute(sheet: &Sheet, spec: &PivotSpec) -> PivotResult {
    let (r1, _, r2, _) = spec.source;
    let header = |c: u32| {
        sheet
            .get(r1, c)
            .map(|x| x.value.display())
            .unwrap_or_else(|| crate::model::col_label(c))
    };
    let row_name = header(spec.row_field);
    let value_name = header(spec.value_field);
    let col_name = spec.col_field.map(&header);
    let value_caption = format!("{}:{}", agg_prefix(spec.agg), value_name);

    // 流式聚合：按 256 行页直接读取列块，并对重复字符串/数值缓存 group_key 结果，避免百万行逐行 String 分配
    let mut row_order: Vec<String> = Vec::new();
    let mut row_map: crate::sort_filter::FxHashMap<String, usize> = Default::default();
    let mut row_ptr_cache: crate::sort_filter::FxHashMap<usize, Option<usize>> = Default::default();
    let mut row_num_cache: crate::sort_filter::FxHashMap<u64, Option<usize>> = Default::default();
    let mut col_order: Vec<String> = Vec::new();
    let mut col_map: crate::sort_filter::FxHashMap<String, usize> = Default::default();
    let mut col_ptr_cache: crate::sort_filter::FxHashMap<usize, usize> = Default::default();
    let mut col_num_cache: crate::sort_filter::FxHashMap<u64, usize> = Default::default();
    let mut cells: Vec<Vec<PivotAcc>> = Vec::new();
    let excluded: HashSet<&str> = spec.exclude.iter().map(String::as_str).collect();

    if spec.col_field.is_none() {
        col_map.insert(value_name.clone(), 0);
        col_order.push(value_name.clone());
    }

    let first = r1.saturating_add(1);
    if first <= r2 {
        let p1 = first / crate::store::PAGE_ROWS;
        let p2 = r2 / crate::store::PAGE_ROWS;
        let is_bucket = matches!(spec.group, GroupBy::Bucket(w) if w > 0.0);
        let page_indices: Vec<u32> = if is_bucket {
            (p1..=p2).collect()
        } else {
            sheet.cells().iter_pages_in(p1, p2).map(|(pi, _)| pi).collect()
        };
        for pi in page_indices {
            let page_opt = sheet.cells().page(pi);
            let Some(row_slab) = page_opt.and_then(|p| p.cols.get(spec.row_field)) else {
                if !is_bucket {
                    continue;
                }
                let base = pi * crate::store::PAGE_ROWS;
                let lo = first.max(base);
                let hi = r2.min(base + crate::store::PAGE_ROWS - 1);
                for r in lo..=hi {
                    let key = group_key(&Value::Empty, &spec.group);
                    if key.is_empty() || excluded.contains(key.as_str()) {
                        continue;
                    }
                    let ci = match spec.col_field {
                        Some(cc) => {
                            let cv = sheet.value_ref(r, cc);
                            let ck = cv.display();
                            *col_map.entry(ck.clone()).or_insert_with(|| {
                                let idx = col_order.len();
                                col_order.push(ck);
                                idx
                            })
                        }
                        None => 0,
                    };
                    let v = sheet.value_ref(r, spec.value_field);
                    let (n, nonempty) = match v.as_ref() {
                        Value::Number(n) => (Some(*n), true),
                        Value::Empty => (None, false),
                        Value::Text(t) => (t.trim().parse::<f64>().ok(), !t.is_empty()),
                        other => (other.display().parse::<f64>().ok(), true),
                    };
                    let ri = *row_map.entry(key.clone()).or_insert_with(|| {
                        let idx = row_order.len();
                        row_order.push(key);
                        cells.push(Vec::new());
                        idx
                    });
                    if cells[ri].len() <= ci {
                        cells[ri].resize(ci + 1, PivotAcc::default());
                    }
                    cells[ri][ci].push(n, nonempty, spec.agg);
                }
                continue;
            };
            let page = page_opt.unwrap();
            let col_slab = spec.col_field.and_then(|cc| page.cols.get(cc));
            let val_slab = page.cols.get(spec.value_field);
            let base = pi * crate::store::PAGE_ROWS;
            let lo = first.saturating_sub(base) as usize;
            let hi_incl = (r2 - base).min(crate::store::PAGE_ROWS - 1) as usize;
            let is_bucket = matches!(spec.group, GroupBy::Bucket(w) if w > 0.0);
            for ro in lo..=hi_incl {
                if !is_bucket && !row_slab.contains(ro as u32) {
                    continue;
                }
                let ri_opt = match row_slab.as_ref() {
                    crate::store::ColSlab::Numbers(ns) if ns.contains(ro as u32) => {
                        let num = ns.values[ro];
                        let nb = if num == 0.0 { 0u64 } else { num.to_bits() };
                        if let Some(&cached) = row_num_cache.get(&nb) {
                            cached
                        } else {
                            let key = group_key(&Value::Number(num), &spec.group);
                            let res = if key.is_empty() || excluded.contains(key.as_str()) {
                                None
                            } else {
                                Some(*row_map.entry(key.clone()).or_insert_with(|| {
                                    let idx = row_order.len();
                                    row_order.push(key);
                                    cells.push(Vec::new());
                                    idx
                                }))
                            };
                            row_num_cache.insert(nb, res);
                            res
                        }
                    }
                    _ => {
                        let rv = row_slab.value_ref(ro as u32);
                        let raw_v = rv.as_deref().unwrap_or(&Value::Empty);
                        match raw_v {
                            Value::Text(s) => {
                                let ptr = s.as_ptr() as usize;
                                if let Some(&cached) = row_ptr_cache.get(&ptr) {
                                    cached
                                } else {
                                    let key = group_key(raw_v, &spec.group);
                                    let res = if key.is_empty() || excluded.contains(key.as_str()) {
                                        None
                                    } else {
                                        Some(*row_map.entry(key.clone()).or_insert_with(|| {
                                            let idx = row_order.len();
                                            row_order.push(key);
                                            cells.push(Vec::new());
                                            idx
                                        }))
                                    };
                                    row_ptr_cache.insert(ptr, res);
                                    res
                                }
                            }
                            Value::Number(num) => {
                                let nb = if *num == 0.0 { 0u64 } else { num.to_bits() };
                                if let Some(&cached) = row_num_cache.get(&nb) {
                                    cached
                                } else {
                                    let key = group_key(raw_v, &spec.group);
                                    let res = if key.is_empty() || excluded.contains(key.as_str()) {
                                        None
                                    } else {
                                        Some(*row_map.entry(key.clone()).or_insert_with(|| {
                                            let idx = row_order.len();
                                            row_order.push(key);
                                            cells.push(Vec::new());
                                            idx
                                        }))
                                    };
                                    row_num_cache.insert(nb, res);
                                    res
                                }
                            }
                            other => {
                                let key = group_key(other, &spec.group);
                                if key.is_empty() || excluded.contains(key.as_str()) {
                                    None
                                } else {
                                    Some(*row_map.entry(key.clone()).or_insert_with(|| {
                                        let idx = row_order.len();
                                        row_order.push(key);
                                        cells.push(Vec::new());
                                        idx
                                    }))
                                }
                            }
                        }
                    }
                };
                let Some(ri) = ri_opt else {
                    continue;
                };
                let ci = match spec.col_field {
                    Some(_) => {
                        let cv = col_slab.and_then(|s| s.value_ref(ro as u32));
                        match cv.as_deref().unwrap_or(&Value::Empty) {
                            Value::Text(t) => {
                                let ptr = t.as_ptr() as usize;
                                if let Some(&idx) = col_ptr_cache.get(&ptr) {
                                    idx
                                } else {
                                    let idx = match col_map.get(t.as_ref()) {
                                        Some(&idx) => idx,
                                        None => {
                                            let idx = col_order.len();
                                            let ck = t.to_string();
                                            col_map.insert(ck.clone(), idx);
                                            col_order.push(ck);
                                            idx
                                        }
                                    };
                                    col_ptr_cache.insert(ptr, idx);
                                    idx
                                }
                            }
                            Value::Number(num) => {
                                let nb = if *num == 0.0 { 0u64 } else { num.to_bits() };
                                if let Some(&idx) = col_num_cache.get(&nb) {
                                    idx
                                } else {
                                    let ck = crate::model::format_number(*num);
                                    let idx = *col_map.entry(ck.clone()).or_insert_with(|| {
                                        let idx = col_order.len();
                                        col_order.push(ck);
                                        idx
                                    });
                                    col_num_cache.insert(nb, idx);
                                    idx
                                }
                            }
                            other => {
                                let ck = other.display();
                                *col_map.entry(ck.clone()).or_insert_with(|| {
                                    let idx = col_order.len();
                                    col_order.push(ck);
                                    idx
                                })
                            }
                        }
                    }
                    None => 0,
                };
                let (n, nonempty) = match val_slab.map(|s| s.as_ref()) {
                    None => (None, false),
                    Some(crate::store::ColSlab::Numbers(ns)) => match ns.get(ro as u32) {
                        Some(v) => (Some(v), true),
                        None => (None, false),
                    },
                    Some(other_slab) => match other_slab.value_ref(ro as u32) {
                        None => (None, false),
                        Some(v) => match v.as_ref() {
                            Value::Number(n) => (Some(*n), true),
                            Value::Empty => (None, false),
                            Value::Text(t) => (t.trim().parse::<f64>().ok(), !t.is_empty()),
                            other => (other.with_display_str(|s| s.parse::<f64>().ok()), true),
                        },
                    },
                };
                if cells[ri].len() <= ci {
                    cells[ri].resize(ci + 1, PivotAcc::default());
                }
                cells[ri][ci].push(n, nonempty, spec.agg);
            }
        }
    }

    let n_cols = if row_order.is_empty() && spec.col_field.is_none() {
        0
    } else {
        col_order.len()
    };
    let col_keys = if n_cols == 0 { Vec::new() } else { col_order };

    let mut headers = vec![row_name.clone()];
    headers.extend(col_keys.iter().cloned());
    if spec.col_field.is_some() {
        headers.push("总计".to_string());
    }

    let mut rows = Vec::with_capacity(row_order.len() + 1);
    let mut grand: Vec<PivotAcc> = vec![PivotAcc::default(); col_keys.len()];
    let mut grand_all = PivotAcc::default();
    let default_acc = PivotAcc::default();
    for (ri, key) in row_order.iter().enumerate() {
        let row_accs = &cells[ri];
        let mut line = Vec::with_capacity(col_keys.len() + 2);
        line.push(Value::Text(key.as_str().into()));
        let mut row_tot = PivotAcc::default();
        for ci in 0..col_keys.len() {
            let acc = row_accs.get(ci).unwrap_or(&default_acc);
            grand[ci].merge(acc);
            row_tot.merge(acc);
            line.push(acc.finish(spec.agg));
        }
        if spec.col_field.is_some() {
            grand_all.merge(&row_tot);
            line.push(row_tot.finish(spec.agg));
        }
        rows.push(line);
    }
    if rows.len() > 1 {
        let mut total = Vec::with_capacity(col_keys.len() + 2);
        total.push(Value::Text("总计".into()));
        for g in &grand {
            total.push(g.finish(spec.agg));
        }
        if spec.col_field.is_some() {
            total.push(grand_all.finish(spec.agg));
        }
        rows.push(total);
    }

    PivotResult {
        headers,
        rows,
        row_name,
        value_name,
        col_name,
        value_caption,
    }
}

/// Excel 值字段标题的前缀
pub fn agg_prefix(agg: Agg) -> &'static str {
    match agg {
        Agg::Sum => "求和项",
        Agg::Count => "计数项",
        Agg::Avg => "平均值项",
        Agg::Max => "最大值项",
        Agg::Min => "最小值项",
    }
}

/// 把结果写到指定位置，返回占用的矩形
pub fn write_to(sheet: &mut Sheet, at: (u32, u32), res: &PivotResult) -> (u32, u32, u32, u32) {
    let (r0, c0) = at;
    // 有列字段时 Excel 的表头是两行：第一行「值字段标题 + 列字段名」，第二行「行字段名 + 列取值 + 总计」
    let head = if let Some(cn) = &res.col_name {
        sheet.set(r0, c0, Cell { value: Value::Text(res.value_caption.as_str().into()), formula: None });
        sheet.set(r0, c0 + 1, Cell { value: Value::Text(cn.as_str().into()), formula: None });
        r0 + 1
    } else {
        r0
    };
    for (i, h) in res.headers.iter().enumerate() {
        sheet.set(
            head,
            c0 + i as u32,
            Cell { value: Value::Text(h.as_str().into()), formula: None },
        );
    }
    for (ri, line) in res.rows.iter().enumerate() {
        for (ci, v) in line.iter().enumerate() {
            sheet.set(head + 1 + ri as u32, c0 + ci as u32, Cell { value: v.clone(), formula: None });
        }
    }
    (
        r0,
        c0,
        head + res.rows.len() as u32,
        c0 + res.headers.len().saturating_sub(1) as u32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sales() -> Sheet {
        let mut s = Sheet::new("销售");
        for (i, h) in ["部门", "月份", "金额"].iter().enumerate() {
            s.set(0, i as u32, Cell { value: Value::Text((*h).into()), formula: None });
        }
        let rows = [
            ("研发", "1月", 100.0),
            ("研发", "2月", 150.0),
            ("销售", "1月", 200.0),
            ("销售", "2月", 50.0),
            ("研发", "1月", 20.0),
        ];
        for (i, (d, m, v)) in rows.iter().enumerate() {
            let r = i as u32 + 1;
            s.set(r, 0, Cell { value: Value::Text((*d).into()), formula: None });
            s.set(r, 1, Cell { value: Value::Text((*m).into()), formula: None });
            s.set(r, 2, Cell { value: Value::Number(*v), formula: None });
        }
        s
    }

    #[test]
    fn pivot_rows_only() {
        let s = sales();
        let spec = PivotSpec {
            source: (0, 0, 5, 2),
            row_field: 0,
            col_field: None,
            value_field: 2,
            agg: Agg::Sum,
            group: GroupBy::None,
            exclude: Vec::new(),
        };
        let res = compute(&s, &spec);
        assert_eq!(res.headers, vec!["部门", "金额"]);
        // BTreeMap 按字符串排序：研发 < 销售
        assert_eq!(res.rows[0][0], Value::Text("研发".into()));
        assert_eq!(res.rows[0][1], Value::Number(270.0));
        assert_eq!(res.rows[1][0], Value::Text("销售".into()));
        assert_eq!(res.rows[1][1], Value::Number(250.0));
        assert_eq!(res.rows[2][0], Value::Text("总计".into()));
        assert_eq!(res.rows[2][1], Value::Number(520.0));
    }

    #[test]
    fn count_ignores_blank_and_counts_text() {
        let mut s = Sheet::default();
        s.set(0, 0, Cell { value: Value::Text("部门".into()), formula: None });
        s.set(0, 1, Cell { value: Value::Text("值".into()), formula: None });
        s.set(1, 0, Cell { value: Value::Text("A".into()), formula: None });
        s.set(1, 1, Cell { value: Value::Number(1.0), formula: None });
        s.set(2, 0, Cell { value: Value::Text("A".into()), formula: None });
        s.set(2, 1, Cell { value: Value::Text("x".into()), formula: None });
        s.set(3, 0, Cell { value: Value::Text("A".into()), formula: None });
        // B4 故意留空：Count 不应把空格算成 0
        s.set(4, 0, Cell { value: Value::Text("B".into()), formula: None });
        // B5 也留空：B 组整组无值，但标签必须保留，Count 显示空白
        let spec = PivotSpec {
            source: (0, 0, 4, 1),
            row_field: 0,
            col_field: None,
            value_field: 1,
            agg: Agg::Count,
            group: GroupBy::None,
            exclude: Vec::new(),
        };
        let res = compute(&s, &spec);
        assert_eq!(res.rows[0][0], Value::Text("A".into()));
        assert_eq!(res.rows[0][1], Value::Number(2.0), "1 和 x 算两个，空格不算");
        assert_eq!(res.rows[1][0], Value::Text("B".into()), "全空分组不能消失");
        assert_eq!(res.rows[1][1], Value::Empty, "全空分组 Count 为空（Excel 实机）");
    }

    #[test]
    fn pivot_with_column_field() {
        let s = sales();
        let spec = PivotSpec {
            source: (0, 0, 5, 2),
            row_field: 0,
            col_field: Some(1),
            value_field: 2,
            agg: Agg::Sum,
            group: GroupBy::None,
            exclude: Vec::new(),
        };
        let res = compute(&s, &spec);
        // 有列字段时 Excel 末尾多一列「总计」
        assert_eq!(res.headers, vec!["部门", "1月", "2月", "总计"]);
        assert_eq!(res.rows[0][1], Value::Number(120.0), "研发 1月 100+20");
        assert_eq!(res.rows[0][2], Value::Number(150.0));
        assert_eq!(res.rows[1][1], Value::Number(200.0));
        assert_eq!(res.rows[1][2], Value::Number(50.0));
        assert_eq!(res.rows[0][3], Value::Number(270.0), "研发 总计 100+150+20");
        assert_eq!(res.rows[1][3], Value::Number(250.0), "销售 总计 200+50");
    }

    #[test]
    fn other_aggregations() {
        let s = sales();
        let mk = |agg| PivotSpec {
            source: (0, 0, 5, 2),
            row_field: 0,
            col_field: None,
            value_field: 2,
            agg,
            group: GroupBy::None,
            exclude: Vec::new(),
};
        assert_eq!(compute(&s, &mk(Agg::Count)).rows[0][1], Value::Number(3.0));
        assert_eq!(compute(&s, &mk(Agg::Max)).rows[0][1], Value::Number(150.0));
        assert_eq!(compute(&s, &mk(Agg::Min)).rows[0][1], Value::Number(20.0));
        let avg = compute(&s, &mk(Agg::Avg));
        if let Value::Number(v) = avg.rows[0][1] {
            assert!((v - 90.0).abs() < 0.001, "(100+150+20)/3 = 90，实际 {v}");
        } else {
            panic!("平均值类型不对");
        }
    }

    #[test]
    fn writes_block() {
        let mut s = sales();
        let spec = PivotSpec {
            source: (0, 0, 5, 2),
            row_field: 0,
            col_field: None,
            value_field: 2,
            agg: Agg::Sum,
            group: GroupBy::None,
            exclude: Vec::new(),
        };
        let res = compute(&s, &spec);
        let rect = write_to(&mut s, (8, 0), &res);
        assert_eq!(rect, (8, 0, 11, 1));
        assert_eq!(s.get(8, 0).unwrap().value.display(), "部门");
        assert_eq!(s.get(9, 0).unwrap().value.display(), "研发");
        assert_eq!(s.get(9, 1).unwrap().value.display(), "270");
    }
}

#[cfg(test)]
mod protection_extra_tests {
    use super::*;
    use crate::model::{Cell, Sheet, Value};

    #[test]
    fn groups_text_dates_by_month_and_excludes() {
        let mut sh = Sheet::new("t");
        sh.set(0, 0, Cell { value: Value::Text("date".into()), formula: None });
        sh.set(0, 1, Cell { value: Value::Text("amount".into()), formula: None });
        for (i, (d, a)) in [("2024-01-05", 100.0), ("2024-01-20", 50.0), ("2024-02-02", 30.0)]
            .iter()
            .enumerate()
        {
            sh.set(i as u32 + 1, 0, Cell { value: Value::Text((*d).into()), formula: None });
            sh.set(i as u32 + 1, 1, Cell { value: Value::Number(*a), formula: None });
        }
        let spec = PivotSpec {
            source: (0, 0, 3, 1),
            row_field: 0,
            col_field: None,
            value_field: 1,
            agg: Agg::Sum,
            group: GroupBy::Month,
            exclude: vec!["2024-02".into()],
        };
        let r = compute(&sh, &spec);
        assert_eq!(r.rows.len(), 1);
        assert_eq!(r.rows[0][0].display(), "2024-01");
        assert_eq!(r.rows[0][1].display(), "150");
    }

    #[test]
    fn bucket_group_labels_ranges() {
        let mut sh = Sheet::new("t");
        sh.set(0, 0, Cell { value: Value::Text("v".into()), formula: None });
        sh.set(0, 1, Cell { value: Value::Text("n".into()), formula: None });
        for (i, n) in [5.0, 42.0, 120.0].iter().enumerate() {
            sh.set(i as u32 + 1, 0, Cell { value: Value::Number(*n), formula: None });
            sh.set(i as u32 + 1, 1, Cell { value: Value::Number(1.0), formula: None });
        }
        let spec = PivotSpec {
            source: (0, 0, 3, 1),
            row_field: 0,
            col_field: None,
            value_field: 1,
            agg: Agg::Count,
            group: GroupBy::Bucket(50.0),
            exclude: Vec::new(),
        };
        let r = compute(&sh, &spec);
        let labels: Vec<String> = r.rows.iter().map(|row| row[0].display()).collect();
        assert_eq!(&labels[..2], &["0~50", "100~150"]);
    }
    #[test]
    fn row_values_lists_distinct_labels() {
        let mut sh = Sheet::new("S");
        for (i, (name, v)) in [("甲", 1.0), ("乙", 2.0), ("甲", 3.0), ("", 4.0)].iter().enumerate() {
            sh.set(
                i as u32 + 1,
                0,
                crate::model::Cell {
                    value: Value::Text((*name).into()),
                    formula: None,
                },
            );
            sh.set(
                i as u32 + 1,
                1,
                crate::model::Cell {
                    value: Value::Number(*v),
                    formula: None,
                },
            );
        }
        sh.set(0, 0, crate::model::Cell { value: Value::Text("名".into()), formula: None });
        sh.set(0, 1, crate::model::Cell { value: Value::Text("值".into()), formula: None });
        let spec = PivotSpec {
            source: (0, 0, 4, 1),
            row_field: 0,
            col_field: None,
            value_field: 1,
            agg: Agg::Sum,
            group: GroupBy::None,
            exclude: vec!["乙".into()],
        };
        // 去重、排序、不含空标签；exclude 里的项也返回（切片器要显示它的勾选状态）
        assert_eq!(row_values(&sh, &spec), vec!["乙".to_string(), "甲".to_string()]);
    }

}
