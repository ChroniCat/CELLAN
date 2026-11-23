//! 条件格式：解析 <conditionalFormatting> + dxf 格式表，并在渲染时给出叠加样式。
//!
//! 只做「看得见」的部分：比较规则、包含文本、重复/唯一、前 N、高于/低于平均、
//! 公式规则、色阶、数据条。图标集读取但不着色（见 README 的已知限制）。

use crate::model::{Sheet, Value};
use std::collections::HashMap;

/// 差异格式（styles.xml 里的 <dxf>）
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Dxf {
    pub bold: bool,
    pub italic: bool,
    pub color: Option<[u8; 3]>,
    pub fill: Option<[u8; 3]>,
    /// 字体名（dxf 的 <font><name val="…"/>），None 表示不改字体
    pub font: Option<String>,
    /// 差异数字格式（dxf 的 <numFmt formatCode="…"/>），None 表示不改
    pub num_fmt: Option<String>,
    /// 差异边框（任一方向有 style 就记一条），None 表示不改边框
    pub border: Option<DxfBorder>,
}

/// dxf 里的差异边框：左、右、上、下各边独立（Excel 允许四边不同 style/颜色）。
#[derive(Clone, Debug, PartialEq, Default)]
pub struct DxfBorder {
    /// 0=左 1=右 2=上 3=下；`(style, color)`，color 缺省按黑色
    pub sides: [Option<(String, [u8; 3])>; 4],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CfOp {
    Less,
    LessEq,
    Equal,
    NotEqual,
    GreaterEq,
    Greater,
    Between,
    NotBetween,
}

impl CfOp {
    fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "lessThan" => CfOp::Less,
            "lessThanOrEqual" => CfOp::LessEq,
            "equal" => CfOp::Equal,
            "notEqual" => CfOp::NotEqual,
            "greaterThanOrEqual" => CfOp::GreaterEq,
            "greaterThan" => CfOp::Greater,
            "between" => CfOp::Between,
            "notBetween" => CfOp::NotBetween,
            _ => return None,
        })
    }
    pub fn label(&self) -> &'static str {
        match self {
            CfOp::Less => "小于",
            CfOp::LessEq => "小于等于",
            CfOp::Equal => "等于",
            CfOp::NotEqual => "不等于",
            CfOp::GreaterEq => "大于等于",
            CfOp::Greater => "大于",
            CfOp::Between => "介于",
            CfOp::NotBetween => "不介于",
        }
    }
}

/// 条件格式阈值类型（OOXML cfvo/@type）
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CfvoKind {
    Min,
    Max,
    Num,
    Percent,
    Percentile,
    Formula,
}

/// 一个阈值：类型 + 数值（公式型把原文放在 formula）
#[derive(Clone, Debug, PartialEq)]
pub struct Cfvo {
    pub kind: CfvoKind,
    pub val: f64,
    pub formula: String,
}

impl Default for Cfvo {
    fn default() -> Self {
        Cfvo { kind: CfvoKind::Min, val: 0.0, formula: String::new() }
    }
}

impl Cfvo {
    pub fn new(kind: CfvoKind, val: f64) -> Self {
        Cfvo { kind, val, formula: String::new() }
    }
    pub fn min() -> Self {
        Cfvo::new(CfvoKind::Min, 0.0)
    }
    pub fn max() -> Self {
        Cfvo::new(CfvoKind::Max, 0.0)
    }
    pub fn from_type(ty: &str, val: Option<f64>) -> Self {
        let kind = match ty {
            "max" => CfvoKind::Max,
            "num" => CfvoKind::Num,
            "percent" => CfvoKind::Percent,
            "percentile" => CfvoKind::Percentile,
            "formula" => CfvoKind::Formula,
            _ => CfvoKind::Min,
        };
        Cfvo { kind, val: val.unwrap_or(0.0), formula: String::new() }
    }
    pub fn type_name(&self) -> &'static str {
        match self.kind {
            CfvoKind::Min => "min",
            CfvoKind::Max => "max",
            CfvoKind::Num => "num",
            CfvoKind::Percent => "percent",
            CfvoKind::Percentile => "percentile",
            CfvoKind::Formula => "formula",
        }
    }
    /// 求这个阈值在当前块数据下的实际数：percent 相对 min..max，percentile 取包含式分位
    pub fn resolve(&self, min: f64, max: f64, sorted: &[f64]) -> f64 {
        match self.kind {
            CfvoKind::Min => min,
            CfvoKind::Max => max,
            CfvoKind::Num => self.val,
            CfvoKind::Percent => min + (max - min) * (self.val / 100.0),
            CfvoKind::Percentile => percentile_inc(sorted, self.val / 100.0),
            CfvoKind::Formula => self
                .formula
                .trim_start_matches('=')
                .trim()
                .parse::<f64>()
                .unwrap_or(min),
        }
    }
}

/// Excel PERCENTILE.INC：k=(n-1)*p 线性插值
pub fn percentile_inc(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    if sorted.len() == 1 {
        return sorted[0];
    }
    let p = p.clamp(0.0, 1.0);
    let k = (sorted.len() - 1) as f64 * p;
    let lo = k.floor() as usize;
    let hi = k.ceil() as usize;
    if lo == hi {
        sorted[lo]
    } else {
        sorted[lo] + (sorted[hi] - sorted[lo]) * (k - lo as f64)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum CfKind {
    CellIs { op: CfOp, a: String, b: String, dxf: usize },
    Text { contains: String, dxf: usize },
    Dup { unique: bool, dxf: usize },
    Top { top: bool, n: u32, percent: bool, dxf: usize },
    Avg { above: bool, dxf: usize },
    Expr { formula: String, dxf: usize },
    Scale { points: Vec<(Cfvo, [u8; 3])> },
    Bar { color: [u8; 3], min: Cfvo, max: Cfvo },
    /// 图标集：set 是 OOXML 的 iconSet 名，thresholds 是各档下限（min/max/percent/…）
    IconSet { set: String, reverse: bool, thresholds: Vec<Cfvo>, show_value: bool },
}

impl CfKind {
    /// 管理窗口里显示的一句话说明
    pub fn describe(&self) -> String {
        match self {
            CfKind::CellIs { op, a, b, .. } => {
                let name = match op {
                    CfOp::Greater => "大于",
                    CfOp::Less => "小于",
                    CfOp::GreaterEq => "大于等于",
                    CfOp::LessEq => "小于等于",
                    CfOp::Equal => "等于",
                    CfOp::NotEqual => "不等于",
                    CfOp::Between => "介于",
                    CfOp::NotBetween => "不介于",
                };
                if matches!(op, CfOp::Between | CfOp::NotBetween) {
                    format!("{name} {a} 和 {b}")
                } else {
                    format!("{name} {a}")
                }
            }
            CfKind::Text { contains, .. } => format!("文本包含\"{contains}\""),
            CfKind::Dup { unique, .. } => {
                if *unique { "唯一值".into() } else { "重复值".into() }
            }
            CfKind::Top { top, n, percent, .. } => {
                let dir = if *top { "前" } else { "后" };
                if *percent {
                    format!("{dir} {n}%")
                } else {
                    format!("{dir} {n} 项")
                }
            }
            CfKind::Avg { above, .. } => {
                if *above { "高于平均值".into() } else { "低于平均值".into() }
            }
            CfKind::Expr { formula, .. } => format!("公式 {formula}"),
            CfKind::Scale { .. } => "色阶".into(),
            CfKind::Bar { .. } => "数据条".into(),
            CfKind::IconSet { set, .. } => format!("图标集 {set}"),
        }
    }
}

/// 图标集按「形状 + 级数」归类，渲染时据此画
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconStyle {
    Arrows,
    TrafficLights,
    Rating,
    Flags,
    Symbols,
    Quarters,
    RedToBlack,
    Boxes,
}

#[derive(Clone, Debug, PartialEq)]
pub struct IconMark {
    pub style: IconStyle,
    pub levels: u8,
    /// 0 = 最低档
    pub idx: u8,
}

impl IconMark {
    /// 把 iconSet 名字拆成 (样式, 级数)
    pub fn from_set(name: &str) -> Option<(IconStyle, u8)> {
        let n = name.trim();
        let levels = n.chars().next()?.to_digit(10)? as u8;
        let rest = &n[1..];
        let style = if rest.starts_with("Arrows") {
            IconStyle::Arrows
        } else if rest.starts_with("TrafficLights") {
            IconStyle::TrafficLights
        } else if rest.starts_with("Rating") {
            IconStyle::Rating
        } else if rest.starts_with("Flags") {
            IconStyle::Flags
        } else if rest.starts_with("Symbols") || rest.starts_with("Signs") {
            IconStyle::Symbols
        } else if rest.starts_with("Quarters") {
            IconStyle::Quarters
        } else if rest.starts_with("RedToBlack") {
            IconStyle::RedToBlack
        } else {
            IconStyle::Boxes
        };
        Some((style, levels.max(2)))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct CfRule {
    pub kind: CfKind,
    pub stop: bool,
    /// Excel 的 priority：越小越先求值；0 表示未指定（保存时补号）
    pub priority: u32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CfBlock {
    pub ranges: Vec<(u32, u32, u32, u32)>,
    pub rules: Vec<CfRule>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CfSheet {
    pub blocks: Vec<CfBlock>,
    pub dxfs: Vec<Dxf>,
    /// 表里存在 x14 扩展条件格式（数据条/图标集扩展）；本实现不建模，保存会丢，
    /// 界面上必须显示「不能无损编辑」的警告。
    pub unknown_ext: bool,
    /// 差异格式里含未建模的边框方向（对角/内部/起止边）；保存会丢，必须提示。
    pub dxf_unsupported: bool,
}

/// 渲染叠加：条件格式作用在基础样式之上
#[derive(Clone, Debug, Default)]
pub struct Overlay {
    pub fill: Option<[u8; 3]>,
    pub color: Option<[u8; 3]>,
    pub bold: bool,
    /// 条件格式里指定的字体名（dxf 的 <font>）
    pub font: Option<String>,
    /// 数据条：比例 0..1 与颜色
    pub bar: Option<(f32, [u8; 3])>,
    /// 图标集
    pub icon: Option<IconMark>,
    /// 条件格式带来的差异边框（填充/字体之外的第四个维度）
    pub border: Option<DxfBorder>,
}

impl Overlay {
    pub fn is_empty(&self) -> bool {
        self.fill.is_none()
            && self.color.is_none()
            && !self.bold
            && self.font.is_none()
            && self.bar.is_none()
            && self.icon.is_none()
            && self.border.is_none()
    }
}

/// 每个色阶/数据条块的取值范围（按需计算并缓存）
#[derive(Clone, Debug, Default)]
pub struct CfStats {
    inputs: Option<Vec<BlockInputs>>,
    /// block 序号 → (min, max)
    pub ranges: HashMap<usize, (f64, f64)>,
    /// 平均/计数用于 avg、top 规则
    pub averages: HashMap<usize, f64>,
    pub counts: HashMap<usize, usize>,
    /// 每个块的数值升序（前 N 项规则要用；超大块不存）
    pub sorted: HashMap<usize, Vec<f64>>,
    /// 重复值/唯一值规则预聚合频次表（block 序号 → 显示文本 → 频次），避免逐格整列扫描
    pub dup_counts: HashMap<usize, HashMap<String, usize>>,
}

#[derive(Clone, Debug)]
pub(crate) struct PageSlice {
    pub page: u32,
    pub col: u32,
    pub start: usize,
    pub end: usize,
    pub cells: std::sync::Arc<Vec<crate::Cell>>,
}

impl PartialEq for PageSlice {
    fn eq(&self, other: &Self) -> bool {
        (self.page, self.col, self.start, self.end) == (other.page, other.col, other.start, other.end)
            && (std::sync::Arc::ptr_eq(&self.cells, &other.cells)
                || self.cells[self.start..self.end] == other.cells[other.start..other.end])
    }
}

#[derive(Clone, Debug, PartialEq)]
struct BlockInputs {
    numeric: bool,
    sort: bool,
    duplicates: bool,
    ranges: Vec<((u32, u32, u32, u32), Vec<PageSlice>)>,
}

fn requirements(block: &CfBlock) -> (bool, bool, bool) {
    let mut numeric = false;
    let mut sort = false;
    let mut duplicates = false;
    for rule in &block.rules {
        match &rule.kind {
            CfKind::Top { .. } => { numeric = true; sort = true; }
            CfKind::Avg { .. } => numeric = true,
            CfKind::Scale { points } => {
                numeric = true;
                sort |= points.iter().any(|(t, _)| t.kind == CfvoKind::Percentile);
            }
            CfKind::IconSet { thresholds, .. } => {
                numeric = true;
                sort |= thresholds.iter().any(|t| t.kind == CfvoKind::Percentile);
            }
            CfKind::Bar { min, max, .. } => {
                numeric = true;
                sort |= min.kind == CfvoKind::Percentile || max.kind == CfvoKind::Percentile;
            }
            CfKind::Dup { .. } => duplicates = true,
            _ => {}
        }
    }
    (numeric, sort, duplicates)
}

/// Compare only the stored pages touched by statistical rules. Most edits can be
/// dismissed by identity; a change outside a partial page compares at most 256 cells.
pub fn refresh_stats(sheet: &Sheet, into: &mut CfStats) -> bool {
    let inputs: Vec<_> = sheet.cf.blocks.iter().map(|block| {
        let (numeric, sort, duplicates) = requirements(block);
        let ranges = if numeric || duplicates {
            block.ranges.iter().map(|&rect| (rect, sheet.cf_pages(rect))).collect()
        } else { Vec::new() };
        BlockInputs { numeric, sort, duplicates, ranges }
    }).collect();
    if into.inputs.as_ref() == Some(&inputs) { return false; }
    compute_stats(&sheet.cf, sheet, into);
    into.inputs = Some(inputs);
    true
}

pub fn compute_stats(cf: &CfSheet, sheet: &Sheet, into: &mut CfStats) {
    into.inputs = None;
    into.ranges.clear();
    into.averages.clear();
    into.counts.clear();
    into.sorted.clear();
    into.dup_counts.clear();
    if cf.blocks.is_empty() {
        return;
    }
    let bounds = sheet.used_bounds();
    let sheet_len = sheet.len();
    for (i, block) in cf.blocks.iter().enumerate() {
        let (needs_num, needs_sort, needs_dup) = requirements(block);
        if !needs_num && !needs_dup {
            continue;
        }
        let Some((ur1, uc1, ur2, uc2)) = bounds else {
            if needs_num {
                into.counts.insert(i, 0);
            }
            if needs_dup {
                into.dup_counts.insert(i, HashMap::new());
            }
            continue;
        };
        let mut clamped: Vec<(u32, u32, u32, u32)> = Vec::with_capacity(block.ranges.len());
        let mut total_area: u64 = 0;
        for &(r1, c1, r2, c2) in &block.ranges {
            let cr1 = r1.max(ur1);
            let cr2 = r2.min(ur2);
            let cc1 = c1.max(uc1);
            let cc2 = c2.min(uc2);
            if cr1 <= cr2 && cc1 <= cc2 {
                clamped.push((cr1, cc1, cr2, cc2));
                total_area = total_area.saturating_add(
                    (cr2 - cr1 + 1) as u64 * (cc2 - cc1 + 1) as u64,
                );
            }
        }
        let mut min = f64::INFINITY;
        let mut max = f64::NEG_INFINITY;
        let mut sum = 0.0;
        let mut n = 0usize;
        let mut vals: Vec<f64> = Vec::new();
        let mut dups: HashMap<String, usize> = HashMap::new();

        let mut visit = |cell: &crate::model::Cell| {
            if needs_num {
                if let Value::Number(v) = &cell.value {
                    if v.is_finite() {
                        min = min.min(*v);
                        max = max.max(*v);
                        sum += *v;
                        n += 1;
                        if needs_sort {
                            vals.push(*v);
                        }
                    }
                }
            }
            if needs_dup {
                let key = cell.value.display();
                if !key.is_empty() {
                    *dups.entry(key).or_insert(0) += 1;
                }
            }
        };

        if clamped.is_empty() {
            // 块范围完全落在非空区之外
        } else if total_area > (sheet_len as u64).saturating_mul(2).max(256)
            || clamped.len() > 1
        {
            for (cref, cell) in sheet.iter() {
                if in_block(block, cref.row, cref.col) {
                    visit(cell);
                }
            }
        } else {
            let (cr1, cc1, cr2, cc2) = clamped[0];
            for r in cr1..=cr2 {
                for c in cc1..=cc2 {
                    if let Some(cell) = sheet.get(r, c) {
                        visit(cell);
                    }
                }
            }
        }
        if needs_num {
            if n > 0 {
                into.ranges.insert(i, (min, max));
                into.averages.insert(i, sum / n as f64);
            }
            into.counts.insert(i, n);
            if !vals.is_empty() {
                vals.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                into.sorted.insert(i, vals);
            }
        }
        if needs_dup {
            into.dup_counts.insert(i, dups);
        }
    }
}

/// 判断某格是否落在块范围内
fn in_block(block: &CfBlock, r: u32, c: u32) -> bool {
    block
        .ranges
        .iter()
        .any(|(r1, c1, r2, c2)| r >= *r1 && r <= *r2 && c >= *c1 && c <= *c2)
}

/// 解析公式里的比较值：数字或带引号的文本
fn parse_lit(s: &str) -> Option<Value> {
    let s = s.trim();
    if let Some(inner) = s.strip_prefix('"').and_then(|x| x.strip_suffix('"')) {
        return Some(Value::Text(inner.into()));
    }
    if let Some(inner) = s.strip_prefix("DATE(").and_then(|x| x.strip_suffix(')')) {
        // DATE(y,m,d) → 序列号（Excel 1900 体系）
        let parts: Vec<&str> = inner.split(',').collect();
        if parts.len() == 3 {
            let y = parts[0].trim().parse::<i32>().ok()?;
            let m = parts[1].trim().parse::<u32>().ok()?;
            let d = parts[2].trim().parse::<u32>().ok()?;
            return crate::formula::date_to_serial_pub(y, m, d).map(Value::Number);
        }
    }
    s.parse::<f64>().ok().map(Value::Number)
}

fn cmp_values(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.partial_cmp(y),
        (Value::Text(x), Value::Text(y)) => Some(x.to_lowercase().cmp(&y.to_lowercase())),
        (Value::Bool(x), Value::Bool(y)) => Some(x.cmp(y)),
        _ => None,
    }
}

fn test_op(op: CfOp, v: &Value, a: &Value, b: &Value) -> bool {
    let ordering = cmp_values(v, a);
    match op {
        CfOp::Less => matches!(ordering, Some(std::cmp::Ordering::Less)),
        CfOp::LessEq => matches!(ordering, Some(std::cmp::Ordering::Less) | Some(std::cmp::Ordering::Equal)),
        CfOp::Equal => matches!(ordering, Some(std::cmp::Ordering::Equal)),
        CfOp::NotEqual => matches!(ordering, Some(o) if o != std::cmp::Ordering::Equal),
        CfOp::GreaterEq => matches!(ordering, Some(std::cmp::Ordering::Greater) | Some(std::cmp::Ordering::Equal)),
        CfOp::Greater => matches!(ordering, Some(std::cmp::Ordering::Greater)),
        CfOp::Between => {
            let lo = cmp_values(v, a);
            let hi = cmp_values(v, b);
            matches!(lo, Some(std::cmp::Ordering::Greater) | Some(std::cmp::Ordering::Equal))
                && matches!(hi, Some(std::cmp::Ordering::Less) | Some(std::cmp::Ordering::Equal))
        }
        CfOp::NotBetween => {
            let lo = cmp_values(v, a);
            let hi = cmp_values(v, b);
            !(matches!(lo, Some(std::cmp::Ordering::Greater) | Some(std::cmp::Ordering::Equal))
                && matches!(hi, Some(std::cmp::Ordering::Less) | Some(std::cmp::Ordering::Equal)))
        }
    }
}

fn lerp_color(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    [
        (a[0] as f32 + (b[0] as f32 - a[0] as f32) * t).round() as u8,
        (a[1] as f32 + (b[1] as f32 - a[1] as f32) * t).round() as u8,
        (a[2] as f32 + (b[2] as f32 - a[2] as f32) * t).round() as u8,
    ]
}

/// 计算某格的条件格式叠加。
/// `expr` 是公式规则的求值器：传入公式原文与当前格坐标，返回是否命中。
pub fn overlay_at<F>(cf: &CfSheet, stats: &CfStats, sheet: &Sheet, r: u32, c: u32, expr: F) -> Overlay
where
    F: FnMut(&str, u32, u32) -> bool,
{
    RowRules::new(cf, r).overlay_at(stats, sheet, c, expr)
}

/// Visible cells on the same row share rule priority and the set of candidate blocks.
pub struct RowRules<'a> {
    cf: &'a CfSheet,
    pub row: u32,
    order: Vec<(u32, usize, usize)>,
}

impl<'a> RowRules<'a> {
    pub fn new(cf: &'a CfSheet, row: u32) -> Self {
        let mut order = Vec::new();
        for (bi, block) in cf.blocks.iter().enumerate() {
            if block.ranges.iter().any(|&(r1, _, r2, _)| r1 <= row && row <= r2) {
                order.extend(block.rules.iter().enumerate().map(|(ri, rule)| (rule.priority, bi, ri)));
            }
        }
        order.sort_unstable();
        Self { cf, row, order }
    }

    pub fn overlay_at<F>(&self, stats: &CfStats, sheet: &Sheet, c: u32, mut expr: F) -> Overlay
    where F: FnMut(&str, u32, u32) -> bool {
        let cf = self.cf;
        let r = self.row;
        let mut out = Overlay::default();
        if self.order.is_empty() { return out; }
        let raw = sheet.get(r, c).map(|x| x.value.clone()).unwrap_or(Value::Empty);
        for &(_, bi, ri) in &self.order {
            let block = &cf.blocks[bi];
            if !in_block(block, r, c) { continue; }
            let rule = &block.rules[ri];
            let (ar, ac) = block
                .ranges
                .first()
                .map(|(r1, c1, _, _)| (*r1, *c1))
                .unwrap_or((0, 0));
            let mut hit = false;
            match &rule.kind {
                CfKind::CellIs { op, a, b, dxf } => {
                    let av = parse_lit(a);
                    let bv = parse_lit(b);
                    let need_b = matches!(op, CfOp::Between | CfOp::NotBetween);
                    if let Some(av) = av {
                        if need_b && bv.is_none() {
                            continue;
                        }
                        // 未填第二个值时用第一个值占位（Excel 里介于必须两个值）
                        let bv = bv.unwrap_or_else(|| av.clone());
                        if !matches!(raw, Value::Empty) && test_op(*op, &raw, &av, &bv) {
                            apply_dxf(cf, *dxf, &mut out);
                            hit = true;
                        }
                    }
                }
                CfKind::Text { contains, dxf } => {
                    if let Value::Text(t) = &raw {
                        if t.to_lowercase().contains(&contains.to_lowercase()) {
                            apply_dxf(cf, *dxf, &mut out);
                            hit = true;
                        }
                    }
                }
                CfKind::Dup { unique, dxf } => {
                    let key = raw.display();
                    if key.is_empty() {
                        continue;
                    }
                    let n = if let Some(dups) = stats.dup_counts.get(&bi) {
                        dups.get(&key).copied().unwrap_or(0)
                    } else {
                        let mut cnt = 0usize;
                        for (cref, cell) in sheet.iter() {
                            if in_block(block, cref.row, cref.col) && cell.value.display() == key {
                                cnt += 1;
                            }
                        }
                        cnt
                    };
                    let want = if *unique { n == 1 } else { n > 1 };
                    if want {
                        apply_dxf(cf, *dxf, &mut out);
                        hit = true;
                    }
                }
                CfKind::Top { top, n, percent, dxf } => {
                    if let (Value::Number(v), Some(vals)) = (&raw, stats.sorted.get(&bi)) {
                        if vals.is_empty() {
                            continue;
                        }
                        let k = if *percent {
                            ((vals.len() as f64) * (*n as f64) / 100.0).ceil().max(1.0) as usize
                        } else {
                            (*n as usize).max(1)
                        }
                        .min(vals.len());
                        let threshold = if *top { vals[vals.len() - k] } else { vals[k - 1] };
                        let ok = if *top { *v >= threshold } else { *v <= threshold };
                        if ok {
                            apply_dxf(cf, *dxf, &mut out);
                            hit = true;
                        }
                    }
                }
                CfKind::Avg { above, dxf } => {
                    if let (Value::Number(v), Some(avg)) = (&raw, stats.averages.get(&bi)) {
                        let ok = if *above { v > avg } else { v < avg };
                        if ok {
                            apply_dxf(cf, *dxf, &mut out);
                            hit = true;
                        }
                    }
                }
                CfKind::Expr { formula, dxf } => {
                    // 公式相对块左上角书写，计算时按相对位移平移
                    if expr(formula, r.saturating_sub(ar), c.saturating_sub(ac)) {
                        apply_dxf(cf, *dxf, &mut out);
                        hit = true;
                    }
                }
                CfKind::Scale { points } => {
                    if let (Value::Number(v), Some((min, max))) = (&raw, stats.ranges.get(&bi)) {
                        let empty: Vec<f64> = Vec::new();
                        let sorted = stats.sorted.get(&bi).unwrap_or(&empty);
                        let abs: Vec<(f64, [u8; 3])> =
                            points.iter().map(|(t, c)| (t.resolve(*min, *max, sorted), *c)).collect();
                        if let Some(rgb) = scale_color(&abs, *v) {
                            if out.fill.is_none() {
                                out.fill = Some(rgb);
                            }
                        }
                    }
                }
                CfKind::IconSet { set, reverse, thresholds, .. } => {
                    if let (Value::Number(v), Some((min, max))) = (&raw, stats.ranges.get(&bi)) {
                        let (min, max) = (*min, *max);
                        if let Some((style, levels)) = IconMark::from_set(set) {
                            // 阈值是「各档下限」；第一档通常是最小值
                            let empty: Vec<f64> = Vec::new();
                            let sorted = stats.sorted.get(&bi).unwrap_or(&empty);
                            let mut idx = 0u8;
                            for (i, th) in thresholds.iter().enumerate().skip(1) {
                                let t = th.resolve(min, max, sorted);
                                {
                                    if *v >= t {
                                        idx = i as u8;
                                    } else {
                                        break;
                                    }
                                }
                            }
                            let _ = (min, max);
                            let idx = if *reverse { levels.saturating_sub(1 + idx) } else { idx };
                            out.icon = Some(IconMark {
                                style,
                                levels,
                                idx: idx.min(levels.saturating_sub(1)),
                            });
                        }
                    }
                }
                CfKind::Bar { color, min: lo, max: hi } => {
                    if let (Value::Number(v), Some((min, max))) = (&raw, stats.ranges.get(&bi)) {
                        let empty: Vec<f64> = Vec::new();
                        let sorted = stats.sorted.get(&bi).unwrap_or(&empty);
                        let a = lo.resolve(*min, *max, sorted);
                        let b = hi.resolve(*min, *max, sorted);
                        let span = (b - a).max(f64::MIN_POSITIVE);
                        let t = ((v - a) / span).clamp(0.0, 1.0) as f32;
                        out.bar = Some((t, *color));
                    }
                }
            }
            if hit && rule.stop {
                return out;
            }
        }
        out
    }
}

fn apply_dxf(cf: &CfSheet, idx: usize, out: &mut Overlay) {
    if let Some(d) = cf.dxfs.get(idx) {
        if let Some(f) = d.fill {
            if out.fill.is_none() {
                out.fill = Some(f);
            }
        }
        if let Some(c) = d.color {
            if out.color.is_none() {
                out.color = Some(c);
            }
        }
        if let Some(f) = &d.font {
            if out.font.is_none() {
                out.font = Some(f.clone());
            }
        }
        if let Some(b) = &d.border {
            if out.border.is_none() {
                out.border = Some(b.clone());
            }
        }
        out.bold |= d.bold;
    }
}

fn scale_color(points: &[(f64, [u8; 3])], v: f64) -> Option<[u8; 3]> {
    if points.len() < 2 {
        return None;
    }
    let lo = points.first().map(|p| p.0).unwrap_or(0.0);
    let hi = points.last().map(|p| p.0).unwrap_or(0.0);
    if hi <= lo {
        return Some(points[0].1);
    }
    if v <= lo {
        return Some(points[0].1);
    }
    if v >= hi {
        return Some(points[points.len() - 1].1);
    }
    // 多段线性插值（2 色或 3 色）
    for w in points.windows(2) {
        let (p0, c0) = (&w[0].0, w[0].1);
        let (p1, c1) = (&w[1].0, w[1].1);
        let (p0, p1) = (*p0, *p1);
        if v >= p0 && v <= p1 && p1 > p0 {
            return Some(lerp_color(c0, c1, ((v - p0) / (p1 - p0)) as f32));
        }
    }
    Some(points[points.len() - 1].1)
}

/// 把公式里的相对引用按 (dr, dc) 平移（A1 → B2 之类）
pub fn shift_formula(f: &str, r: u32, c: u32) -> String {
    if r == 0 && c == 0 {
        return f.to_string();
    }
    let chars: Vec<char> = f.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    let mut in_str = false;
    let mut bracket_depth = 0usize;
    while i < chars.len() {
        let ch = chars[i];
        if ch == '"' {
            in_str = !in_str;
            out.push(ch);
            i += 1;
            continue;
        }
        if !in_str && ch == '[' {
            bracket_depth += 1;
            out.push(ch);
            i += 1;
            continue;
        }
        if !in_str && ch == ']' {
            bracket_depth = bracket_depth.saturating_sub(1);
            out.push(ch);
            i += 1;
            continue;
        }
        // 整行引用 `1:1` / `$1:$1`：复制时只随行偏移，横向复制不变。
        // 必须在 A1 分支之前处理，否则以数字开头的行引用会被跳过；
        // 结构化引用方括号里的 `[1:3]` 是列名，不是行引用。
        if !in_str
            && bracket_depth == 0
            && (ch == '$' || ch.is_ascii_digit())
            && (i == 0 || !(chars[i - 1].is_ascii_alphanumeric() || chars[i - 1] == '_'))
        {
            let mut j = i;
            let a_abs = chars[j] == '$';
            j += usize::from(a_abs);
            let a0 = j;
            while j < chars.len() && chars[j].is_ascii_digit() {
                j += 1;
            }
            let a1 = j;
            if j > a0 && j < chars.len() && chars[j] == ':' {
                j += 1;
                let b_abs = j < chars.len() && chars[j] == '$';
                j += usize::from(b_abs);
                let b0 = j;
                while j < chars.len() && chars[j].is_ascii_digit() {
                    j += 1;
                }
                let b1 = j;
                let end_ok = j == chars.len()
                    || !(chars[j].is_ascii_alphanumeric()
                        || chars[j] == '_'
                        || chars[j] == '('
                        || chars[j] == '$');
                if j > b0 && end_ok {
                    let parse = |s: &[char]| -> Option<u32> {
                        s.iter().collect::<String>().parse::<u32>().ok()
                    };
                    if let (Some(r1), Some(r2)) =
                        (parse(&chars[a0..a1]), parse(&chars[b0..b1]))
                    {
                        let nr1 = if a_abs { r1 } else { r1.saturating_add(r) };
                        let nr2 = if b_abs { r2 } else { r2.saturating_add(r) };
                        if nr1 == 0 || nr2 == 0 || nr1 > 1_048_576 || nr2 > 1_048_576 {
                            out.push_str("#REF!");
                        } else {
                            out.push_str(&format!(
                                "{}{}:{}{}",
                                if a_abs { "$" } else { "" },
                                nr1,
                                if b_abs { "$" } else { "" },
                                nr2
                            ));
                        }
                        i = j;
                        continue;
                    }
                }
            }
        }
        let starts_ref = !in_str && (ch == '$' || ch.is_ascii_alphabetic());
        // 前一个字符不能是字母数字或下划线（避免把名字截断）
        let prev_ok = i == 0 || !(chars[i - 1].is_ascii_alphanumeric() || chars[i - 1] == '_');
        if starts_ref && prev_ok {
            let mut j = i;
            let mut abs_col = false;
            if chars[j] == '$' {
                abs_col = true;
                j += 1;
            }
            let col_start = j;
            let mut col = 0u32;
            while j < chars.len() && chars[j].is_ascii_alphabetic() {
                // 超长名字（如 ISNUMBER、NOT）会让 col 溢出：饱和相加，随后由
                // col <= 16384 的检查把它判成「不是引用」，按普通字符原样输出。
                col = col
                    .saturating_mul(26)
                    .saturating_add(chars[j].to_ascii_uppercase() as u32 - 'A' as u32 + 1);
                j += 1;
            }
            let has_col = j > col_start;
            let mut abs_row = false;
            if has_col && j < chars.len() && chars[j] == '$' {
                abs_row = true;
                j += 1;
            }
            let row_start = j;
            let mut row = 0u32;
            while j < chars.len() && chars[j].is_ascii_digit() {
                row = row * 10 + (chars[j] as u32 - '0' as u32);
                j += 1;
            }
            let has_row = j > row_start;
            let next_ok = j >= chars.len() || !(chars[j].is_ascii_alphanumeric() || chars[j] == '_' || chars[j] == '(');
            if has_col && has_row && next_ok && col > 0 && col <= 16_384 {
                let nc = if abs_col { col } else { col + c };
                let nr = if abs_row { row } else { row + r };
                if nc >= 1 && nc <= 16_384 {
                    out.push_str(&format!(
                        "{}{}{}{}",
                        if abs_col { "$" } else { "" },
                        crate::model::col_label(nc - 1),
                        if abs_row { "$" } else { "" },
                        nr.max(1)
                    ));
                    i = j;
                    continue;
                }
            }
        }
        out.push(ch);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Cell;

    fn sheet_with(vals: &[(u32, u32, f64)]) -> Sheet {
        let mut s = Sheet::new("T");
        for (r, c, v) in vals {
            s.set(*r, *c, Cell { value: Value::Number(*v), formula: None });
        }
        s
    }

    #[test]
    fn comparison_rule() {
        let cf = CfSheet {
            blocks: vec![CfBlock {
                ranges: vec![(0, 0, 9, 0)],
                rules: vec![CfRule {
                    kind: CfKind::CellIs { op: CfOp::Greater, a: "100".into(), b: String::new(), dxf: 0 },
                    stop: false, priority: 1,
                }],
            }],
            dxfs: vec![Dxf { fill: Some([255, 199, 206]), color: Some([156, 0, 6]), ..Default::default() }],
            unknown_ext: false,
            dxf_unsupported: false,
        };
        let sh = sheet_with(&[(0, 0, 50.0), (1, 0, 150.0)]);
        let mut st = CfStats::default();
        compute_stats(&cf, &sh, &mut st);
        assert!(overlay_at(&cf, &st, &sh, 0, 0, |_, _, _| false).is_empty());
        let o = overlay_at(&cf, &st, &sh, 1, 0, |_, _, _| false);
        assert_eq!(o.fill, Some([255, 199, 206]));
        assert_eq!(o.color, Some([156, 0, 6]));
    }

    #[test]
    fn color_scale_endpoints() {
        let cf = CfSheet {
            blocks: vec![CfBlock {
                ranges: vec![(0, 0, 2, 0)],
                rules: vec![CfRule {
                    kind: CfKind::Scale {
                        points: vec![
                            (Cfvo::min(), [248, 105, 107]),
                            (Cfvo::new(CfvoKind::Percentile, 50.0), [255, 235, 132]),
                            (Cfvo::max(), [99, 190, 123]),
                        ],
                    },
                    stop: false, priority: 1,
                }],
            }],
            dxfs: vec![],
            unknown_ext: false,
            dxf_unsupported: false,
        };
        let sh = sheet_with(&[(0, 0, 0.0), (1, 0, 50.0), (2, 0, 100.0)]);
        let mut st = CfStats::default();
        compute_stats(&cf, &sh, &mut st);
        assert_eq!(overlay_at(&cf, &st, &sh, 0, 0, |_, _, _| false).fill, Some([248, 105, 107]));
        assert_eq!(overlay_at(&cf, &st, &sh, 1, 0, |_, _, _| false).fill, Some([255, 235, 132]));
        assert_eq!(overlay_at(&cf, &st, &sh, 2, 0, |_, _, _| false).fill, Some([99, 190, 123]));
        // 中间值应该是插值出来的
        let mid = overlay_at(&cf, &st, &sh, 0, 0, |_, _, _| false).fill.unwrap();
        assert_eq!(mid, [248, 105, 107]);
    }

    #[test]
    fn data_bar_ratio() {
        let cf = CfSheet {
            blocks: vec![CfBlock {
                ranges: vec![(0, 0, 1, 0)],
                rules: vec![CfRule { kind: CfKind::Bar { color: [99, 142, 198], min: Cfvo::min(), max: Cfvo::max() }, stop: false, priority: 1 }],
            }],
            dxfs: vec![],
            unknown_ext: false,
            dxf_unsupported: false,
        };
        let sh = sheet_with(&[(0, 0, 10.0), (1, 0, 30.0)]);
        let mut st = CfStats::default();
        compute_stats(&cf, &sh, &mut st);
        assert_eq!(overlay_at(&cf, &st, &sh, 0, 0, |_, _, _| false).bar.unwrap().0, 0.0);
        assert_eq!(overlay_at(&cf, &st, &sh, 1, 0, |_, _, _| false).bar.unwrap().0, 1.0);
    }

    #[test]
    fn text_and_dup_rules() {
        let cf = CfSheet {
            blocks: vec![CfBlock {
                ranges: vec![(0, 0, 2, 0)],
                rules: vec![
                    CfRule { kind: CfKind::Text { contains: "急".into(), dxf: 0 }, stop: false, priority: 1 },
                ],
            }],
            dxfs: vec![Dxf { fill: Some([255, 235, 156]), ..Default::default() }],
            unknown_ext: false,
            dxf_unsupported: false,
        };
        let mut sh = Sheet::new("T");
        sh.set(0, 0, Cell { value: Value::Text("很急".into()), formula: None });
        sh.set(1, 0, Cell { value: Value::Text("普通".into()), formula: None });
        let mut st = CfStats::default();
        compute_stats(&cf, &sh, &mut st);
        assert_eq!(overlay_at(&cf, &st, &sh, 0, 0, |_, _, _| false).fill, Some([255, 235, 156]));
        assert!(overlay_at(&cf, &st, &sh, 1, 0, |_, _, _| false).fill.is_none());
    }

    #[test]
    fn stats_reuse_pages_outside_rule_and_rebuild_for_changed_input_or_rule() {
        let mut sh = sheet_with(&[(10, 0, 5.0), (20, 0, 25.0)]);
        sh.cf.blocks.push(CfBlock {
            ranges: vec![(10, 0, 20, 0)],
            rules: vec![CfRule { kind: CfKind::Bar { color: [0; 3], min: Cfvo::min(), max: Cfvo::max() }, stop: false, priority: 1 }],
        });
        let mut st = CfStats::default();
        assert!(refresh_stats(&sh, &mut st));
        assert!(st.sorted.is_empty());
        for (row, col) in [(11, 1), (4, 0), (1000, 0)] {
            sh.set(row, col, Cell { value: Value::Number(99.0), formula: None });
            assert!(!refresh_stats(&sh, &mut st));
        }
        sh.set(15, 0, Cell { value: Value::Number(80.0), formula: None });
        assert!(refresh_stats(&sh, &mut st));
        assert_eq!(st.ranges[&0], (5.0, 80.0));
        sh.cf.blocks[0].rules[0].kind = CfKind::Top { top: true, n: 1, percent: false, dxf: 0 };
        assert!(refresh_stats(&sh, &mut st));
        assert_eq!(st.sorted[&0], [5.0, 25.0, 80.0]);
    }

    #[test]
    fn formula_relative_shift() {
        assert_eq!(shift_formula("A1>100", 2, 0), "A3>100");
        assert_eq!(shift_formula("$A$1>B1", 1, 1), "$A$1>C2");
        assert_eq!(shift_formula("SUM(A1:A3)>0", 0, 1), "SUM(B1:B3)>0");
        assert_eq!(shift_formula("\"A1\"&A1", 1, 0), "\"A1\"&A2");
        // Long function names used to overflow the column accumulator in debug builds.
        assert_eq!(shift_formula("NOT(ISERROR(A1))", 1, 0), "NOT(ISERROR(A2))");
    }
}

// ---------------------------------------------------------------------------
// XML 解析
// ---------------------------------------------------------------------------

use quick_xml::events::Event;
use quick_xml::Reader;

/// 把 "A1:B5 C3:D4" 这样的 sqref 拆成矩形列表
pub fn parse_sqref(s: &str) -> Vec<(u32, u32, u32, u32)> {
    let mut out = Vec::new();
    for part in s.split_whitespace() {
        let (a, b) = match part.split_once(':') {
            Some((x, y)) => (x, y),
            None => (part, part),
        };
        if let (Some((r1, c1)), Some((r2, c2))) = (parse_a1(a), parse_a1(b)) {
            out.push((r1.min(r2), c1.min(c2), r1.max(r2), c1.max(c2)));
        }
    }
    out
}

fn parse_a1(s: &str) -> Option<(u32, u32)> {
    let clean: String = s.chars().filter(|c| *c != '$').collect();
    let mut col = 0u32;
    let mut row = 0u32;
    let mut seen_digit = false;
    for ch in clean.chars() {
        if ch.is_ascii_alphabetic() && !seen_digit {
            col = col * 26 + (ch.to_ascii_uppercase() as u32 - 'A' as u32 + 1);
        } else if ch.is_ascii_digit() {
            seen_digit = true;
            row = row * 10 + (ch as u32 - '0' as u32);
        } else {
            return None;
        }
    }
    if col == 0 || row == 0 {
        return None;
    }
    Some((row - 1, col - 1))
}

/// 取元素属性（quick-xml 给的名字可能带命名空间前缀，按后缀匹配）
fn attr_val(e: &quick_xml::events::BytesStart<'_>, key: &str) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref().ends_with(key))
        .map(|a| a.value.to_string())
}

fn hex_rgb(s: &str) -> Option<[u8; 3]> {
    let s = s.trim().trim_start_matches('#');
    let s = if s.len() == 8 { &s[2..] } else { s };
    if s.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some([r, g, b])
}

/// 解析一张工作表里的 <conditionalFormatting>
/// 是否含本实现未建模的扩展条件格式（x14 的 extLst / x14:conditionalFormatting）
pub fn xml_has_cf_ext(xml: &str) -> bool {
    let mut r = Reader::from_str(xml);
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let n = e.name();
                let n = n.as_ref();
                if n.ends_with("extLst") || n.ends_with("conditionalFormatting") && n.contains(':') {
                    return true;
                }
            }
            Ok(Event::Eof) | Err(_) => return false,
            _ => {}
        }
    }
}

pub fn parse_cf_blocks(xml: &str) -> Vec<CfBlock> {
    let mut out: Vec<CfBlock> = Vec::new();
    let mut r = Reader::from_str(xml);
    // 当前块、当前规则的临时状态
    let mut cur_block: Option<CfBlock> = None;
    let mut cur_rule: Option<(String, HashMap<String, String>)> = None;
    let mut formulas: Vec<String> = Vec::new();
    let mut scale_colors: Vec<[u8; 3]> = Vec::new();
    let mut cfvo: Vec<Cfvo> = Vec::new();
    let mut cfvo_open: Option<Cfvo> = None;
    let mut bar_color: Option<[u8; 3]> = None;
    let mut icon_set_name = String::new();
    let mut icon_reverse = false;
    let mut icon_show = true;
    let mut last_color: Option<[u8; 3]> = None;
    let mut text_buf = String::new();
    let mut depth_rule = 0usize;

    loop {
        match r.read_event() {
            Ok(Event::Start(e)) => {
                let name = e.name();
                let name = name.as_ref();
                if name.ends_with("conditionalFormatting") && !name.contains(':') {
                    let mut ranges = Vec::new();
                    for a in e.attributes().flatten() {
                        if a.key.as_ref().ends_with("sqref") {
                            ranges = parse_sqref(&a.value.to_string());
                        }
                    }
                    cur_block = Some(CfBlock { ranges, rules: Vec::new() });
                } else if name.ends_with("cfRule") && !name.contains(':') {
                    let mut attrs = HashMap::new();
                    for a in e.attributes().flatten() {
                        attrs.insert(a.key.as_ref().to_string(), crate::model::xml_unescape(&a.value.to_string()));
                    }
                    formulas.clear();
                    scale_colors.clear();
                    cfvo.clear();
                    bar_color = None;
                    last_color = None;
                    icon_set_name.clear();
                    icon_reverse = false;
                    icon_show = true;
                    cur_rule = Some((String::new(), attrs));
                    depth_rule = 1;
                } else if depth_rule > 0 && name.ends_with("iconSet") {
                    depth_rule += 1;
                    for a in e.attributes().flatten() {
                        let k = a.key.as_ref();
                        let v = crate::model::xml_unescape(&a.value.to_string());
                        if k == "iconSet" {
                            icon_set_name = v;
                        } else if k == "reverse" {
                            icon_reverse = v == "1" || v == "true";
                        } else if k == "showValue" {
                            icon_show = v != "0" && v != "false";
                        }
                    }
                } else if depth_rule > 0 {
                    depth_rule += 1;
                    if name.ends_with("formula") {
                        text_buf.clear();
                    } else if name.ends_with("color") {
                        for a in e.attributes().flatten() {
                            if a.key.as_ref().ends_with("rgb") {
                                last_color = hex_rgb(&a.value.to_string());
                                let is_scale = cur_rule.as_ref().map(|(_, at)| at.get("type").map(|t| t.as_str()) == Some("colorScale")).unwrap_or(false);
                                if is_scale {
                                    if let Some(c) = last_color {
                                        scale_colors.push(c);
                                    }
                                }
                                if cur_rule.as_ref().map(|(_, at)| at.get("type").map(|t| t.as_str()) == Some("dataBar")).unwrap_or(false) {
                                    bar_color = last_color;
                                }
                            }
                        }
                    } else if name.ends_with("cfvo") {
                        // <cfvo type="percent" val="50"/> 或 <cfvo type="formula">…</cfvo>
                        let mut ty = String::new();
                        let mut val: Option<f64> = None;
                        for a in e.attributes().flatten() {
                            let k = a.key.as_ref();
                            let v = crate::model::xml_unescape(&a.value.to_string());
                            if k.ends_with("val") {
                                val = v.parse::<f64>().ok();
                            } else if k.ends_with("type") {
                                ty = v;
                            }
                        }
                        let cv = Cfvo::from_type(&ty, val);
                        text_buf.clear();
                        cfvo_open = Some(cv);
                    }
                }
            }
            Ok(Event::Empty(e)) => {
                let name = e.name();
                let name = name.as_ref();
                if depth_rule > 0 {
                    if name.ends_with("color") {
                        for a in e.attributes().flatten() {
                            if a.key.as_ref().ends_with("rgb") {
                                last_color = hex_rgb(&a.value.to_string());
                                let is_scale = cur_rule.as_ref().map(|(_, at)| at.get("type").map(|t| t.as_str()) == Some("colorScale")).unwrap_or(false);
                                if is_scale {
                                    if let Some(c) = last_color {
                                        scale_colors.push(c);
                                    }
                                }
                                if cur_rule.as_ref().map(|(_, at)| at.get("type").map(|t| t.as_str()) == Some("dataBar")).unwrap_or(false) {
                                    bar_color = last_color;
                                }
                            }
                        }
                    } else if name.ends_with("cfvo") {
                        let mut ty = String::new();
                        let mut val: Option<f64> = None;
                        for a in e.attributes().flatten() {
                            let k = a.key.as_ref();
                            let v = crate::model::xml_unescape(&a.value.to_string());
                            if k.ends_with("val") {
                                val = v.parse::<f64>().ok();
                            } else if k.ends_with("type") {
                                ty = v;
                            }
                        }
                        cfvo.push(Cfvo::from_type(&ty, val));
                        // <color> 紧跟在 cfvo 后面：先记着，交给下面统一收
                    }
                }
            }
            // quick_xml 把 &gt; / &quot; 单独发成 GeneralRef，必须接住，否则公式里的 > 会丢
            Ok(Event::GeneralRef(e)) => {
                if depth_rule > 0 {
                    let name: String = e.into_inner().to_string();
                    text_buf.push_str(&crate::model::resolve_entity(&name));
                }
            }
            Ok(Event::Text(t)) => {
                if depth_rule > 0 {
                    let raw: &str = &t;
                    text_buf.push_str(&quick_xml::escape::unescape(raw).unwrap_or_else(|_| raw.into()));
                }
            }
            Ok(Event::End(e)) => {
                let name = e.name();
                let name = name.as_ref();
                if name.ends_with("cfvo") {
                    if let Some(mut cv) = cfvo_open.take() {
                        cv.formula = text_buf.trim().to_string();
                        text_buf.clear();
                        cfvo.push(cv);
                    }
                } else if name.ends_with("cfRule") {
                    if let (Some((_, attrs)), Some(block)) = (cur_rule.take(), cur_block.as_mut()) {
                        if let Some(mut kind) = build_rule(&attrs, &formulas, &cfvo, &last_color, bar_color, &icon_set_name, icon_reverse, icon_show) {
                            if let CfKind::Scale { points } = &mut kind {
                                if points.is_empty() {
                                    *points = cfvo
                                        .iter()
                                        .zip(scale_colors.iter())
                                        .map(|(v, c)| (v.clone(), *c))
                                        .collect();
                                }
                            }
                            let priority = attrs
                                .get("priority")
                                .and_then(|v| v.parse::<u32>().ok())
                                .unwrap_or(0);
                            block.rules.push(CfRule {
                                kind,
                                stop: attrs.get("stopIfTrue").map(|v| v == "1" || v == "true").unwrap_or(false),
                                priority,
                             });
                        }
                    }
                    depth_rule = 0;
                } else if depth_rule > 0 {
                    if name.ends_with("formula") {
                        formulas.push(text_buf.trim().to_string());
                        text_buf.clear();
                    }
                    depth_rule = depth_rule.saturating_sub(1);
                } else if name.ends_with("conditionalFormatting") {
                    if let Some(b) = cur_block.take() {
                        if !b.rules.is_empty() {
                            out.push(b);
                        }
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }
    out
}

fn build_rule(
    attrs: &HashMap<String, String>,
    formulas: &[String],
    cfvo: &[Cfvo],
    last_color: &Option<[u8; 3]>,
    bar_color: Option<[u8; 3]>,
    icon_set: &str,
    icon_reverse: bool,
    icon_show: bool,
) -> Option<CfKind> {
    let ty = attrs.get("type").map(|s| s.as_str()).unwrap_or("");
    let dxf = attrs.get("dxfId").and_then(|v| v.parse::<usize>().ok()).unwrap_or(usize::MAX);
    let f0 = formulas.first().cloned().unwrap_or_default();
    Some(match ty {
        "cellIs" => {
            let op = CfOp::from_str(attrs.get("operator").map(|s| s.as_str()).unwrap_or("equal"))?;
            CfKind::CellIs {
                op,
                a: f0,
                b: formulas.get(1).cloned().unwrap_or_default(),
                dxf,
            }
        }
        "containsText" | "beginsWith" | "endsWith" => CfKind::Text {
            contains: attrs.get("text").cloned().unwrap_or_default(),
            dxf,
        },
        "duplicateValues" => CfKind::Dup { unique: false, dxf },
        "uniqueValues" => CfKind::Dup { unique: true, dxf },
        "top10" => CfKind::Top {
            top: attrs.get("bottom").map(|v| v != "1").unwrap_or(true),
            n: attrs.get("rank").and_then(|v| v.parse().ok()).unwrap_or(10),
            percent: attrs.get("percent").map(|v| v == "1").unwrap_or(false),
            dxf,
        },
        "aboveAverage" => CfKind::Avg {
            above: attrs.get("aboveAverage").map(|v| v != "0").unwrap_or(true),
            dxf,
        },
        "expression" => CfKind::Expr { formula: f0, dxf },
        "colorScale" => {
            // cfvo/color 交替：从 attrs 无法拿到，交给 scale_points（见调用方）
            CfKind::Scale { points: Vec::new() }
        }
        "dataBar" => CfKind::Bar {
            color: bar_color.or(*last_color).unwrap_or([99, 142, 198]),
            min: cfvo.first().cloned().unwrap_or_else(Cfvo::min),
            max: cfvo.get(1).cloned().unwrap_or_else(Cfvo::max),
        },
        "iconSet" => CfKind::IconSet {
            set: if icon_set.is_empty() {
                "3Arrows".to_string()
            } else {
                icon_set.to_string()
            },
            reverse: icon_reverse,
            thresholds: cfvo.to_vec(),
            show_value: icon_show,
        },
        _ => return None,
    })
}

/// 解析 xl/styles.xml 里的 <dxfs>
pub fn parse_dxfs(xml: &str) -> Vec<Dxf> {
    let mut out = Vec::new();
    let mut r = Reader::from_str(xml);
    let mut in_dxfs = false;
    let mut cur: Option<Dxf> = None;
    let mut in_font = false;
    let mut in_fill = false;
    let mut in_border = false;
    /// 正在读对角/内部这类未建模边框：其中的 color 不能落进填充
    let mut skip_color = false;
    let mut side: Option<usize> = None;

    fn side_index(name: &str) -> Option<usize> {
        match name {
            "left" => Some(0),
            "right" => Some(1),
            "top" => Some(2),
            "bottom" => Some(3),
            _ => None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn on_open(
        e: &quick_xml::events::BytesStart<'_>,
        empty: bool,
        in_dxfs: &mut bool,
        cur: &mut Option<Dxf>,
        in_font: &mut bool,
        in_fill: &mut bool,
        in_border: &mut bool,
        skip_color: &mut bool,
        side: &mut Option<usize>,
    ) {
        let name = e.name();
        let name = name.as_ref().to_string();
        let name = name.as_str();
        if name.ends_with("dxfs") {
            *in_dxfs = true;
            return;
        }
        if *in_dxfs && name.ends_with("dxf") {
            *cur = Some(Dxf::default());
            return;
        }
        let Some(d) = cur.as_mut() else { return };
        if name.ends_with("font") {
            *in_font = true;
        }
        if name == "fill" || name.ends_with(":fill") {
            *in_fill = true;
        }
        if name == "border" || name.ends_with(":border") {
            *in_border = true;
        }
        if *in_border
            && matches!(
                name.to_ascii_lowercase().rsplit(':').next().unwrap_or(""),
                "diagonal" | "vertical" | "horizontal" | "start" | "end"
            )
        {
            // 未建模方向：读样式存在性即可，颜色不参与填充/四边
            *skip_color = true;
        }
        if name.ends_with("numFmt") {
            if let Some(fc) = attr_val(e, "formatCode") {
                d.num_fmt = Some(fc);
            }
            return;
        }
        let lname0 = name.to_ascii_lowercase();
        if *in_font && lname0 == "name" {
            if let Some(v) = attr_val(e, "val") {
                d.font = Some(crate::style::normalize_font(&v));
            }
            return;
        }
        if *in_font && (lname0 == "b" || lname0.ends_with(":b")) {
            d.bold = true;
            return;
        }
        if *in_font && (lname0 == "i" || lname0.ends_with(":i")) {
            d.italic = true;
            return;
        }
        let lname = name.to_ascii_lowercase();
        let local = lname.rsplit(':').next().unwrap_or("");
        if *in_border
            && matches!(local, "diagonal" | "vertical" | "horizontal" | "start" | "end")
        {
            // 未建模方向：样式存在性由 dxfs_unsupported 告警，这里不改四边、不碰填充
            *skip_color = true;
            return;
        }
        if let Some(idx) = side_index(&lname) {
            if let Some(style) = attr_val(e, "style") {
                if !style.is_empty() && style != "none" {
                    let color = d
                        .border
                        .as_ref()
                        .and_then(|b| b.sides[idx].clone())
                        .map(|(_, c)| c)
                        .unwrap_or([0, 0, 0]);
                    d.border.get_or_insert_with(Default::default).sides[idx] = Some((style, color));
                    if !empty {
                        *side = Some(idx);
                    }
                }
            }
            return;
        }
        if lname.ends_with("color") {
            for a in e.attributes().flatten() {
                if a.key.as_ref().ends_with("rgb") {
                    if let Some(c) = hex_rgb(&a.value.to_string()) {
                        if *in_font {
                            d.color = Some(c);
                        } else if *skip_color {
                            // 对角/内部边框的颜色：不写进填充
                        } else if *in_border {
                            if let Some(idx) = *side {
                                if let Some(b) = d.border.as_mut() {
                                    if let Some(slot) = b.sides[idx].as_mut() {
                                        slot.1 = c;
                                    }
                                }
                            }
                        } else if *in_fill {
                            d.fill = Some(c);
                        }
                    }
                }
            }
        }
    }

    loop {
        match r.read_event() {
            Ok(Event::Start(e)) => on_open(
                &e, false, &mut in_dxfs, &mut cur, &mut in_font, &mut in_fill, &mut in_border,
                &mut skip_color, &mut side,
            ),
            Ok(Event::Empty(e)) => on_open(
                &e, true, &mut in_dxfs, &mut cur, &mut in_font, &mut in_fill, &mut in_border,
                &mut skip_color, &mut side,
            ),
            Ok(Event::End(e)) => {
                let name = e.name().as_ref().to_string();
                let name = name.as_str();
                let local = name.rsplit(':').next().unwrap_or("");
                if name.ends_with("font") {
                    in_font = false;
                } else if name == "fill" || name.ends_with(":fill") {
                    in_fill = false;
                } else if name == "border" || name.ends_with(":border") {
                    in_border = false;
                    skip_color = false;
                } else if matches!(local, "diagonal" | "vertical" | "horizontal" | "start" | "end") {
                    skip_color = false;
                } else if side_index(name).is_some() {
                    side = None;
                } else if name.ends_with("dxf") {
                    side = None;
                    in_font = false;
                    if let Some(d) = cur.take() {
                        out.push(d);
                    }
                } else if name.ends_with("dxfs") {
                    in_dxfs = false;
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    out
}

/// 差异格式里出现未建模的边框方向（对角/内部/起止）：保存会丢，需要提示。
pub fn dxfs_unsupported(xml: &str) -> bool {
    let mut r = Reader::from_str(xml);
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) | Ok(Event::Empty(e)) => {
                let name = e.name();
                let name = name.as_ref().to_ascii_lowercase();
                if matches!(
                    name.as_str(),
                    "diagonal" | "vertical" | "horizontal" | "start" | "end" | "outline"
                ) && attr_val(&e, "style")
                    .map(|s| !s.is_empty() && s != "none")
                    .unwrap_or(false)
                {
                    return true;
                }
            }
            Ok(Event::Eof) | Err(_) => return false,
            _ => {}
        }
    }
}
#[cfg(test)]
mod parse_tests {
    use super::*;

    #[test]
    fn sqref_lists() {
        assert_eq!(parse_sqref("A1:B2"), vec![(0, 0, 1, 1)]);
        assert_eq!(parse_sqref("A2:A10 C2:C10"), vec![(1, 0, 9, 0), (1, 2, 9, 2)]);
        assert_eq!(parse_sqref("B5"), vec![(4, 1, 4, 1)]);
    }

    #[test]
    fn parses_cell_is_and_scale() {
        let xml = r#"
        <worksheet><conditionalFormatting sqref="A2:A10">
          <cfRule type="cellIs" dxfId="0" priority="1" operator="greaterThan"><formula>100</formula></cfRule>
          <cfRule type="colorScale" priority="2">
            <colorScale>
              <cfvo type="min"/><color rgb="FFF8696B"/>
              <cfvo type="percentile" val="50"/><color rgb="FFFFEB84"/>
              <cfvo type="max"/><color rgb="FF63BE7B"/>
            </colorScale>
          </cfRule>
        </conditionalFormatting></worksheet>"#;
        let blocks = parse_cf_blocks(xml);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].ranges, vec![(1, 0, 9, 0)]);
        assert_eq!(blocks[0].rules.len(), 2);
        match &blocks[0].rules[0].kind {
            CfKind::CellIs { op, a, dxf, .. } => {
                assert_eq!(*op, CfOp::Greater);
                assert_eq!(a, "100");
                assert_eq!(*dxf, 0);
            }
            other => panic!("规则类型不对: {other:?}"),
        }
    }

    #[test]
    fn parses_dxfs() {
        let xml = r#"<styleSheet><dxfs count="2">
          <dxf><font><color rgb="FF9C0006"/></font><fill><patternFill><bgColor rgb="FFFFC7CE"/></patternFill></fill></dxf>
          <dxf><font><b/><color rgb="FF006100"/></font><fill><patternFill><bgColor rgb="FFC6EFCE"/></patternFill></fill></dxf>
        </dxfs></styleSheet>"#;
        let d = parse_dxfs(xml);
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].color, Some([156, 0, 6]));
        assert_eq!(d[0].fill, Some([255, 199, 206]));
        assert!(d[1].bold);
        assert_eq!(d[1].fill, Some([198, 239, 206]));
    }
}
