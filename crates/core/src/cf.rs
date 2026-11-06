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

#[derive(Clone, Debug)]
pub enum CfKind {
    CellIs { op: CfOp, a: String, b: String, dxf: usize },
    Text { contains: String, dxf: usize },
    Dup { unique: bool, dxf: usize },
    Top { top: bool, n: u32, percent: bool, dxf: usize },
    Avg { above: bool, dxf: usize },
    Expr { formula: String, dxf: usize },
    Scale { points: Vec<(Option<f64>, [u8; 3])> },
    Bar { color: [u8; 3] },
}

#[derive(Clone, Debug)]
pub struct CfRule {
    pub kind: CfKind,
    pub stop: bool,
    
}

#[derive(Clone, Debug, Default)]
pub struct CfBlock {
    pub ranges: Vec<(u32, u32, u32, u32)>,
    pub rules: Vec<CfRule>,
}

#[derive(Clone, Debug, Default)]
pub struct CfSheet {
    pub blocks: Vec<CfBlock>,
    pub dxfs: Vec<Dxf>,
}

/// 渲染叠加：条件格式作用在基础样式之上
#[derive(Clone, Debug, Default)]
pub struct Overlay {
    pub fill: Option<[u8; 3]>,
    pub color: Option<[u8; 3]>,
    pub bold: bool,
    /// 数据条：比例 0..1 与颜色
    pub bar: Option<(f32, [u8; 3])>,
}

impl Overlay {
    pub fn is_empty(&self) -> bool {
        self.fill.is_none() && self.color.is_none() && !self.bold && self.bar.is_none()
    }
}

/// 每个色阶/数据条块的取值范围（按需计算并缓存）
#[derive(Clone, Debug, Default)]
pub struct CfStats {
    /// block 序号 → (min, max)
    pub ranges: HashMap<usize, (f64, f64)>,
    /// 平均/计数用于 avg、top 规则
    pub averages: HashMap<usize, f64>,
    pub counts: HashMap<usize, usize>,
    /// 每个块的数值升序（前 N 项规则要用；超大块不存）
    pub sorted: HashMap<usize, Vec<f64>>,
}

pub fn compute_stats(cf: &CfSheet, sheet: &Sheet, into: &mut CfStats) {
    into.ranges.clear();
    into.averages.clear();
    into.counts.clear();
    into.sorted.clear();
    for (i, block) in cf.blocks.iter().enumerate() {
        let mut min = f64::INFINITY;
        let mut max = f64::NEG_INFINITY;
        let mut sum = 0.0;
        let mut n = 0usize;
        let mut vals: Vec<f64> = Vec::new();
        for (r1, c1, r2, c2) in block.ranges.iter() {
            for r in *r1..=*r2 {
                for c in *c1..=*c2 {
                    if let Some(Value::Number(v)) = sheet.get(r, c).map(|x| x.value.clone()) {
                        if v.is_finite() {
                            min = min.min(v);
                            max = max.max(v);
                            sum += v;
                            n += 1;
                            if vals.len() < 200_000 {
                                vals.push(v);
                            }
                        }
                    }
                }
            }
        }
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
pub fn overlay_at<F>(cf: &CfSheet, stats: &CfStats, sheet: &Sheet, r: u32, c: u32, mut expr: F) -> Overlay
where
    F: FnMut(&str, u32, u32) -> bool,
{
    let mut out = Overlay::default();
    let raw = sheet.get(r, c).map(|x| x.value.clone()).unwrap_or(Value::Empty);
    for (bi, block) in cf.blocks.iter().enumerate() {
        if !in_block(block, r, c) {
            continue;
        }
        let (ar, ac) = block
            .ranges
            .first()
            .map(|(r1, c1, _, _)| (*r1, *c1))
            .unwrap_or((0, 0));
        for rule in block.rules.iter() {
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
                    // 只对文本/数字做重复计数（范围一般不大）
                    let key = raw.display();
                    if key.is_empty() {
                        continue;
                    }
                    let mut n = 0;
                    for (r1, c1, r2, c2) in block.ranges.iter() {
                        for rr in *r1..=*r2 {
                            for cc in *c1..=*c2 {
                                if let Some(v) = sheet.get(rr, cc) {
                                    if v.value.display() == key {
                                        n += 1;
                                    }
                                }
                            }
                        }
                    }
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
                        if let Some(rgb) = scale_color(points, *v, *min, *max) {
                            if out.fill.is_none() {
                                out.fill = Some(rgb);
                            }
                        }
                    }
                }
                CfKind::Bar { color } => {
                    if let (Value::Number(v), Some((min, max))) = (&raw, stats.ranges.get(&bi)) {
                        let span = (max - min).max(f64::MIN_POSITIVE);
                        let t = ((v - min) / span).clamp(0.0, 1.0) as f32;
                        out.bar = Some((t, *color));
                    }
                }
            }
            if hit && rule.stop {
                return out;
            }
        }
    }
    out
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
        out.bold |= d.bold;
    }
}

fn scale_color(points: &[(Option<f64>, [u8; 3])], v: f64, min: f64, max: f64) -> Option<[u8; 3]> {
    if points.len() < 2 {
        return None;
    }
    let lo = points.first().map(|p| p.0.unwrap_or(min)).unwrap_or(min);
    let hi = points.last().map(|p| p.0.unwrap_or(max)).unwrap_or(max);
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
        let (p0, c0) = (&w[0].0.unwrap_or(lo), w[0].1);
        let (p1, c1) = (&w[1].0.unwrap_or(hi), w[1].1);
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
    while i < chars.len() {
        let ch = chars[i];
        if ch == '"' {
            in_str = !in_str;
            out.push(ch);
            i += 1;
            continue;
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
                col = col * 26 + (chars[j].to_ascii_uppercase() as u32 - 'A' as u32 + 1);
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
                    stop: false,
                }],
            }],
            dxfs: vec![Dxf { fill: Some([255, 199, 206]), color: Some([156, 0, 6]), ..Default::default() }],
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
                            (None, [248, 105, 107]),
                            (Some(50.0), [255, 235, 132]),
                            (None, [99, 190, 123]),
                        ],
                    },
                    stop: false,
                }],
            }],
            dxfs: vec![],
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
                rules: vec![CfRule { kind: CfKind::Bar { color: [99, 142, 198] }, stop: false }],
            }],
            dxfs: vec![],
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
                    CfRule { kind: CfKind::Text { contains: "急".into(), dxf: 0 }, stop: false },
                ],
            }],
            dxfs: vec![Dxf { fill: Some([255, 235, 156]), ..Default::default() }],
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
    fn formula_relative_shift() {
        assert_eq!(shift_formula("A1>100", 2, 0), "A3>100");
        assert_eq!(shift_formula("$A$1>B1", 1, 1), "$A$1>C2");
        assert_eq!(shift_formula("SUM(A1:A3)>0", 0, 1), "SUM(B1:B3)>0");
        assert_eq!(shift_formula("\"A1\"&A1", 1, 0), "\"A1\"&A2");
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
pub fn parse_cf_blocks(xml: &str) -> Vec<CfBlock> {
    let mut out: Vec<CfBlock> = Vec::new();
    let mut r = Reader::from_str(xml);
    // 当前块、当前规则的临时状态
    let mut cur_block: Option<CfBlock> = None;
    let mut cur_rule: Option<(String, HashMap<String, String>)> = None;
    let mut formulas: Vec<String> = Vec::new();
    let mut scale_colors: Vec<[u8; 3]> = Vec::new();
    let mut cfvo: Vec<Option<f64>> = Vec::new();
    let mut bar_color: Option<[u8; 3]> = None;
    let mut last_color: Option<[u8; 3]> = None;
    let mut text_buf = String::new();
    let mut depth_rule = 0usize;

    loop {
        match r.read_event() {
            Ok(Event::Start(e)) => {
                let name = e.name();
                let name = name.as_ref();
                if name.ends_with("conditionalFormatting") {
                    let mut ranges = Vec::new();
                    for a in e.attributes().flatten() {
                        if a.key.as_ref().ends_with("sqref") {
                            ranges = parse_sqref(&a.value.to_string());
                        }
                    }
                    cur_block = Some(CfBlock { ranges, rules: Vec::new() });
                } else if name.ends_with("cfRule") {
                    let mut attrs = HashMap::new();
                    for a in e.attributes().flatten() {
                        attrs.insert(a.key.as_ref().to_string(), a.value.to_string());
                    }
                    formulas.clear();
                    scale_colors.clear();
                    cfvo.clear();
                    bar_color = None;
                    last_color = None;
                    cur_rule = Some((String::new(), attrs));
                    depth_rule = 1;
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
                        // <cfvo type="percent" val="50"/>
                        let mut val: Option<f64> = None;
                        let mut is_min = false;
                        let mut is_max = false;
                        for a in e.attributes().flatten() {
                            let k = a.key.as_ref();
                            let v = a.value.to_string();
                            if k.ends_with("val") {
                                val = v.parse::<f64>().ok();
                            } else if k.ends_with("type") {
                                is_min = v == "min";
                                is_max = v == "max";
                            }
                        }
                        cfvo.push(if is_min || is_max { None } else { val });
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
                        let mut val: Option<f64> = None;
                        let mut is_edge = false;
                        for a in e.attributes().flatten() {
                            let k = a.key.as_ref();
                            let v = a.value.to_string();
                            if k.ends_with("val") {
                                val = v.parse::<f64>().ok();
                            } else if k.ends_with("type") {
                                is_edge = v == "min" || v == "max";
                            }
                        }
                        cfvo.push(if is_edge { None } else { val });
                        // <color> 紧跟在 cfvo 后面：先记着，交给下面统一收
                    }
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
                if name.ends_with("cfRule") {
                    if let (Some((_, attrs)), Some(block)) = (cur_rule.take(), cur_block.as_mut()) {
                        if let Some(mut kind) = build_rule(&attrs, &formulas, &cfvo, &last_color, bar_color) {
                            if let CfKind::Scale { points } = &mut kind {
                                if points.is_empty() {
                                    *points = cfvo
                                        .iter()
                                        .zip(scale_colors.iter())
                                        .map(|(v, c)| (*v, *c))
                                        .collect();
                                }
                            }
                            block.rules.push(CfRule {
                                kind,
                                stop: attrs.get("stopIfTrue").map(|v| v == "1" || v == "true").unwrap_or(false),
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
    cfvo: &[Option<f64>],
    last_color: &Option<[u8; 3]>,
    bar_color: Option<[u8; 3]>,
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
        "dataBar" => CfKind::Bar { color: bar_color.or(*last_color).unwrap_or([99, 142, 198]) },
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
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) => {
                let name = e.name();
                let name = name.as_ref();
                if name.ends_with("dxfs") {
                    in_dxfs = true;
                } else if in_dxfs && name.ends_with("dxf") {
                    cur = Some(Dxf::default());
                } else if cur.is_some() && name.ends_with("font") {
                    in_font = true;
                } else if let Some(d) = cur.as_mut() {
                    if name.ends_with("color") {
                        for a in e.attributes().flatten() {
                            if a.key.as_ref().ends_with("rgb") {
                                if let Some(c) = hex_rgb(&a.value.to_string()) {
                                    if in_font {
                                        d.color = Some(c);
                                    } else {
                                        d.fill = Some(c);
                                    }
                                }
                            }
                        }
                    } else if name.ends_with("b") {
                        d.bold = true;
                    } else if name.ends_with("i") {
                        d.italic = true;
                    }
                }
            }
            Ok(Event::Empty(e)) => {
                let name = e.name();
                let name = name.as_ref();
                if let Some(d) = cur.as_mut() {
                    if name.to_ascii_lowercase().ends_with("color") {
                        for a in e.attributes().flatten() {
                            if a.key.as_ref().ends_with("rgb") {
                                if let Some(c) = hex_rgb(&a.value.to_string()) {
                                    if in_font {
                                        d.color = Some(c);
                                    } else {
                                        d.fill = Some(c);
                                    }
                                }
                            }
                        }
                    } else if name == "b" || name.ends_with(":b") {
                        d.bold = true;
                    } else if name == "i" || name.ends_with(":i") {
                        d.italic = true;
                    }
                }
            }
            Ok(Event::End(e)) => {
                let name = e.name();
                let name = name.as_ref();
                if name.ends_with("font") {
                    in_font = false;
                } else if name.ends_with("dxf") {
                    if let Some(d) = cur.take() {
                        out.push(d);
                    }
                } else if name.ends_with("dxfs") {
                    in_dxfs = false;
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }
    out
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
