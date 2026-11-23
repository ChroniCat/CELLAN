//! 排序与筛选。
//!
//! 排序按 Excel 的规矩：整行搬运（值、公式、样式一起走），空格永远排在最后；
//! 数字按大小、文本按不区分大小写的字典序、布尔按真假。
//! 筛选不删数据，只把不匹配的行隐藏起来（隐藏 = 行高 0，和 Excel 一致）。

use crate::model::{Cell, Sheet, Value};
use std::collections::{HashMap, HashSet};

/// 排序键：数字 < 文本 < 布尔，空格单独处理
fn key_of(v: &Value) -> Option<(u8, f64, String)> {
    match v {
        Value::Number(n) => Some((0, *n, String::new())),
        Value::Text(t) => Some((1, 0.0, t.to_lowercase())),
        Value::Bool(b) => Some((2, if *b { 1.0 } else { 0.0 }, String::new())),
        // 错误值排在最后（Excel 同序），同类之间按错误文本稳定排序
        Value::Error(e) => Some((3, 0.0, e.text().to_string())),
        Value::Empty => None,
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

pub(crate) fn sort_order(
    sheet: &Sheet, region: (u32, u32, u32, u32), key: u32, asc: bool, horizontal: bool,
) -> Result<Vec<u32>, String> {
    let (r1, c1, r2, c2) = region;
    let (first, last, lo, hi) = if horizontal { (c1, c2, r1, r2) } else { (r1, r2, c1, c2) };
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
    let mut lower_cache: HashMap<usize, String> = HashMap::new();
    let mut lines: Vec<_> = (first..=last)
        .map(|line| {
            let (r, c) = if horizontal { (key, line) } else { (line, key) };
            let v = sheet.value_ref(r, c);
            let k = match v {
                Value::Number(n) => Some((0u8, *n, String::new())),
                Value::Text(t) => {
                    let ptr = std::sync::Arc::as_ptr(t) as *const () as usize;
                    let low = lower_cache
                        .entry(ptr)
                        .or_insert_with(|| t.to_lowercase())
                        .clone();
                    Some((1u8, 0.0, low))
                }
                Value::Bool(b) => Some((2u8, if *b { 1.0 } else { 0.0 }, String::new())),
                Value::Error(e) => Some((3u8, 0.0, e.text().to_string())),
                Value::Empty => None,
            };
            (k, line)
        })
        .collect();
    lines.sort_by(|a, b| match (&a.0, &b.0) {
        (None, None) => std::cmp::Ordering::Equal,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (Some(_), None) => std::cmp::Ordering::Less,
        (Some(x), Some(y)) => {
            let ord =
                x.0.cmp(&y.0)
                    .then_with(|| x.1.partial_cmp(&y.1).unwrap_or(std::cmp::Ordering::Equal))
                    .then_with(|| x.2.cmp(&y.2));
            if asc { ord } else { ord.reverse() }
        }
    });
    Ok(lines.into_iter().map(|(_, line)| line).collect())
}

/// 区域第一列是不是表头列：整列都是非空文本，且右侧出现过数字（按行排序时跳过它）
pub fn has_header_col(sheet: &Sheet, r1: u32, c1: u32, r2: u32, c2: u32) -> bool {
    if c2 <= c1 {
        return false;
    }
    let all_text = (r1..=r2).all(|r| {
        matches!(
            sheet.get(r, c1).map(|x| &x.value),
            Some(Value::Text(t)) if !t.is_empty()
        )
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
        matches!(
            sheet.get(r1, c).map(|x| &x.value),
            Some(Value::Text(t)) if !t.is_empty()
        )
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
            && (c1..=c2).contains(&k.col) && matches!(cell.value, Value::Number(_)))
    } else {
        (r1..=r2).any(|r| (c1..=c2).any(|c| matches!(sheet.get(r, c).map(|x| &x.value), Some(Value::Number(_)))))
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
/// 使用 `Store` 页级 `row_mask` 与 `col_nonempty` 位图检测空行/空列，跳过百万空行无需逐格查询。
pub fn current_region(sheet: &Sheet, r: u32, c: u32) -> (u32, u32, u32, u32) {
    let Some((ur1, uc1, ur2, uc2)) = sheet.used_bounds() else {
        return (r, c, r, c);
    };
    let min_r = ur1.min(r);
    let max_r = ur2.max(r);
    let min_c = uc1.min(c);
    let max_c = uc2.max(c);
    let cells = sheet.cells();
    let is_blank_row = |rr: u32| !cells.row_has_any_in(rr, min_c, max_c);
    let is_blank_col = |cc: u32| !cells.col_has_any_in(cc, min_r, max_r);

    let mut r1 = r;
    while r1 > min_r && !is_blank_row(r1 - 1) {
        r1 -= 1;
    }
    let mut r2 = r;
    while r2 < max_r && !is_blank_row(r2 + 1) {
        r2 += 1;
    }
    let mut c1 = c;
    while c1 > min_c && !is_blank_col(c1 - 1) {
        c1 -= 1;
    }
    let mut c2 = c;
    while c2 < max_c && !is_blank_col(c2 + 1) {
        c2 += 1;
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

/// 一条筛选条件
#[derive(Clone, Debug, PartialEq)]
pub struct Criterion {
    pub op: CmpOp,
    pub val: String,
    /// 只有 Between 用
    pub val2: Option<String>,
}

impl Criterion {
    pub fn new(op: CmpOp, val: impl Into<String>) -> Self {
        Self {
            op,
            val: val.into(),
            val2: None,
        }
    }

    pub fn between(a: impl Into<String>, b: impl Into<String>) -> Self {
        Self {
            op: CmpOp::Between,
            val: a.into(),
            val2: Some(b.into()),
        }
    }

    /// 这条条件是否放行某个显示文本
    pub fn matches(&self, text: &str) -> bool {
        match self.op {
            CmpOp::Contains => text.to_lowercase().contains(&self.val.to_lowercase()),
            CmpOp::NotContains => !text.to_lowercase().contains(&self.val.to_lowercase()),
            CmpOp::Begins => text.to_lowercase().starts_with(&self.val.to_lowercase()),
            CmpOp::Ends => text.to_lowercase().ends_with(&self.val.to_lowercase()),
            _ => {
                // 数值优先，转不动就按文本比
                if let (Ok(a), Ok(b)) = (text.trim().parse::<f64>(), self.val.trim().parse::<f64>()) {
                    return match self.op {
                        CmpOp::Eq => a == b,
                        CmpOp::Ne => a != b,
                        CmpOp::Gt => a > b,
                        CmpOp::Ge => a >= b,
                        CmpOp::Lt => a < b,
                        CmpOp::Le => a <= b,
                        CmpOp::Between => {
                            let hi = self
                                .val2
                                .as_deref()
                                .and_then(|v| v.trim().parse::<f64>().ok())
                                .unwrap_or(b);
                            a >= b.min(hi) && a <= b.max(hi)
                        }
                        _ => true,
                    };
                }
                let t = text.to_lowercase();
                let v = self.val.to_lowercase();
                match self.op {
                    CmpOp::Eq => t == v,
                    CmpOp::Ne => t != v,
                    CmpOp::Gt => t > v,
                    CmpOp::Ge => t >= v,
                    CmpOp::Lt => t < v,
                    CmpOp::Le => t <= v,
                    CmpOp::Between => {
                        let hi = self.val2.as_deref().unwrap_or("").to_lowercase();
                        let (lo, hi) = if v <= hi { (v, hi) } else { (hi, v) };
                        t >= lo && t <= hi
                    }
                    _ => true,
                }
            }
        }
    }
}

/// 一个筛选器：区域 + 每列允许的值 / 每列的条件
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Filter {
    pub range: (u32, u32, u32, u32),
    /// 列号 → 允许出现文本值；没有条目的列不过滤（CoW 共享，避免 Sheet 克隆深拷几万唯一值）
    pub allows: crate::model::CellMetadata<u32, HashSet<String>>,
    /// 列号 → 条件（文本筛选/数字筛选）；和 allows 是「与」关系
    pub criteria: HashMap<u32, Criterion>,
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
        Self {
            op: c.op,
            num1,
            num2,
            lower1,
            lower2,
            ascii_only,
        }
    }

    #[inline]
    fn matches_num(&self, a: f64, b: f64) -> bool {
        match self.op {
            CmpOp::Eq => a == b,
            CmpOp::Ne => a != b,
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

    fn matches_str(&self, text: &str) -> bool {
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

    #[inline]
    fn matches_value(&self, v: &Value) -> bool {
        match v {
            Value::Number(n) if !self.op.is_text() => {
                if let Some(b) = self.num1 {
                    self.matches_num(*n, b)
                } else {
                    self.matches_str(&v.display())
                }
            }
            Value::Text(t) => self.matches_str(t),
            Value::Empty => self.matches_str(""),
            Value::Bool(b) => self.matches_str(if *b { "TRUE" } else { "FALSE" }),
            Value::Error(e) => self.matches_str(e.text()),
            Value::Number(_) => self.matches_str(&v.display()),
        }
    }
}

impl Filter {
    pub fn new(r1: u32, c1: u32, r2: u32, c2: u32) -> Self {
        Self {
            range: (r1, c1, r2, c2),
            allows: Default::default(),
            criteria: HashMap::new(),
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
                        Value::Number(_) => allows.contains(&v.display()),
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
            for (key, cell) in sheet.iter_rect((first, req.col, r2, req.col)) {
                if req.passes(&cell.value)
                    && rules
                        .iter()
                        .enumerate()
                        .all(|(i, r)| i == req_idx || r.passes(sheet.value_ref(key.row, r.col)))
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
                for (key, cell) in sheet.iter_rect((first, rule.col, r2, rule.col)) {
                    if !rule.passes(&cell.value) {
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

    /// 这列有没有任何筛选（值列表或条件）
    pub fn is_filtered(&self, col: u32) -> bool {
        self.allows.get(&col).map(|a| !a.is_empty()).unwrap_or(false)
            || self.criteria.contains_key(&col)
    }
}

/// 某列在筛选区域里的去重值（按显示文本，排序后返回）
pub fn unique_values(sheet: &Sheet, range: (u32, u32, u32, u32), col: u32) -> Vec<String> {
    FilterColumn::new(sheet, range, col).choices().0
}

/// Shares the source column's pages; a pending popup never retains drawings or other columns.
#[derive(Clone)]
pub struct FilterColumn {
    pages: Vec<(u32, std::sync::Arc<Vec<Cell>>)>,
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
        let mut unique = HashSet::new();
        let mut populated = 0;
        for (base, cells) in self.pages {
            if !keep() { return None; }
            let start = self.header.saturating_add(1).saturating_sub(base) as usize;
            if self.last < base || start >= cells.len() { continue; }
            let end = ((self.last - base) as usize + 1).min(cells.len());
            for cell in cells.iter().take(end).skip(start) {
                if !cell.is_blank() {
                    populated += 1;
                    unique.insert(cell.value.display());
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
}
