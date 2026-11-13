//! 数据验证：整数/小数/序列/日期/时间/文本长度/自定义公式。
//!
//! 语义对齐 Excel：序列类型可以内联写 "甲,乙,丙"，也可以引用区域（$A$1:$A$5）；
//! 校验失败时拒绝写入（Excel 里是弹「重试/取消」，这里保持编辑框并提示原因）。

use crate::model::{Sheet, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DvKind {
    Any,
    Whole,
    Decimal,
    List,
    Date,
    Time,
    TextLength,
    Custom,
}

impl DvKind {
    pub fn label(&self) -> &'static str {
        match self {
            DvKind::Any => "任何值",
            DvKind::Whole => "整数",
            DvKind::Decimal => "小数",
            DvKind::List => "序列",
            DvKind::Date => "日期",
            DvKind::Time => "时间",
            DvKind::TextLength => "文本长度",
            DvKind::Custom => "自定义",
        }
    }
    pub fn from_xml(s: &str) -> Self {
        match s {
            "whole" => DvKind::Whole,
            "decimal" => DvKind::Decimal,
            "list" => DvKind::List,
            "date" => DvKind::Date,
            "time" => DvKind::Time,
            "textLength" => DvKind::TextLength,
            "custom" => DvKind::Custom,
            _ => DvKind::Any,
        }
    }
    pub fn to_xml(&self) -> &'static str {
        match self {
            DvKind::Any => "none",
            DvKind::Whole => "whole",
            DvKind::Decimal => "decimal",
            DvKind::List => "list",
            DvKind::Date => "date",
            DvKind::Time => "time",
            DvKind::TextLength => "textLength",
            DvKind::Custom => "custom",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DvOp {
    Between,
    NotBetween,
    Equal,
    NotEqual,
    Greater,
    Less,
    GreaterEq,
    LessEq,
}

impl DvOp {
    pub fn label(&self) -> &'static str {
        match self {
            DvOp::Between => "介于",
            DvOp::NotBetween => "不介于",
            DvOp::Equal => "等于",
            DvOp::NotEqual => "不等于",
            DvOp::Greater => "大于",
            DvOp::Less => "小于",
            DvOp::GreaterEq => "大于等于",
            DvOp::LessEq => "小于等于",
        }
    }
    pub fn from_xml(s: &str) -> Self {
        match s {
            "notBetween" => DvOp::NotBetween,
            "equal" => DvOp::Equal,
            "notEqual" => DvOp::NotEqual,
            "greaterThan" => DvOp::Greater,
            "lessThan" => DvOp::Less,
            "greaterThanOrEqual" => DvOp::GreaterEq,
            "lessThanOrEqual" => DvOp::LessEq,
            _ => DvOp::Between,
        }
    }
    pub fn to_xml(&self) -> &'static str {
        match self {
            DvOp::Between => "between",
            DvOp::NotBetween => "notBetween",
            DvOp::Equal => "equal",
            DvOp::NotEqual => "notEqual",
            DvOp::Greater => "greaterThan",
            DvOp::Less => "lessThan",
            DvOp::GreaterEq => "greaterThanOrEqual",
            DvOp::LessEq => "lessThanOrEqual",
        }
    }
    pub fn needs_pair(&self) -> bool {
        matches!(self, DvOp::Between | DvOp::NotBetween)
    }
}

#[derive(Clone, Debug)]
pub struct DataValidation {
    pub ranges: Vec<(u32, u32, u32, u32)>,
    pub kind: DvKind,
    pub op: DvOp,
    pub f1: String,
    pub f2: String,
    pub allow_blank: bool,
    /// false 表示隐藏下拉箭头（Excel 的 showDropDown="1" 反而是「不显示」）
    pub show_dropdown: bool,
    pub error_title: String,
    pub error_msg: String,
    pub prompt_title: String,
    pub prompt_msg: String,
}

impl Default for DataValidation {
    fn default() -> Self {
        Self {
            ranges: Vec::new(),
            kind: DvKind::Any,
            op: DvOp::Between,
            f1: String::new(),
            f2: String::new(),
            allow_blank: true,
            show_dropdown: true,
            error_title: "输入值非法".into(),
            error_msg: "此单元格的值不符合验证规则".into(),
            prompt_title: String::new(),
            prompt_msg: String::new(),
        }
    }
}

pub fn in_ranges(ranges: &[(u32, u32, u32, u32)], r: u32, c: u32) -> bool {
    ranges.iter().any(|(r1, c1, r2, c2)| r >= *r1 && r <= *r2 && c >= *c1 && c <= *c2)
}

/// 找到作用于该格的验证规则
pub fn find<'a>(list: &'a [DataValidation], r: u32, c: u32) -> Option<&'a DataValidation> {
    list.iter().find(|d| in_ranges(&d.ranges, r, c))
}

/// 序列来源：内联 "甲,乙" 或引用区域
pub fn list_items(dv: &DataValidation, sheet: &Sheet) -> Vec<String> {
    let src = dv.f1.trim();
    if src.is_empty() {
        return Vec::new();
    }
    if let Some(rest) = src.strip_prefix('"').and_then(|x| x.strip_suffix('"')) {
        return rest
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
    }
    // 区域引用
    let clean: String = src.chars().filter(|c| *c != '$' && *c != '=').collect();
    if let Some((a, b)) = clean.split_once(':') {
        if let (Some((r1, c1)), Some((r2, c2))) = (parse_a1(a), parse_a1(b)) {
            let mut out = Vec::new();
            for r in r1.min(r2)..=r1.max(r2) {
                for c in c1.min(c2)..=c1.max(c2) {
                    if let Some(v) = sheet.get(r, c) {
                        let t = v.value.display();
                        if !t.is_empty() {
                            out.push(t);
                        }
                    }
                }
            }
            return out;
        }
    }
    Vec::new()
}

/// 从工作簿上下文解析列表来源：内联、命名区域、跨表区域、单值区域。
/// 返回 `None` 表示配置无法解析；调用方必须拒绝并给出诊断，不能当成“无限制”。
pub fn list_items_wb(wb: &crate::model::Workbook, si: usize, dv: &DataValidation) -> Option<Vec<String>> {
    let src = dv.f1.trim();
    if src.is_empty() {
        return Some(Vec::new());
    }
    if let Some(rest) = src.strip_prefix('"').and_then(|x| x.strip_suffix('"')) {
        return Some(rest.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect());
    }
    let mut expr = src.trim().trim_start_matches('=').trim();
    // 命名区域（工作簿级或本表级）；名称不区分大小写。
    let has_sheet = expr.contains('!');
    if !has_sheet && !expr.contains(':') {
        if let Some((_, _, target)) = wb
            .names
            .iter()
            .rev()
            .find(|(n, scope, _)| n.eq_ignore_ascii_case(expr) && (scope.is_none() || *scope == Some(si)))
        {
            expr = target.trim();
        }
    }
    if let Some(rest) = expr.strip_prefix('"').and_then(|x| x.strip_suffix('"')) {
        return Some(rest.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect());
    }
    // { "甲", "乙" } 形式的动态数组字面量。
    if let Some(body) = expr.strip_prefix('{').and_then(|x| x.strip_suffix('}')) {
        return Some(
            body.split(',')
                .map(|s| s.trim().trim_matches('"').trim_matches('\'').to_string())
                .filter(|s| !s.is_empty())
                .collect(),
        );
    }
    let (sheet_name, range) = match expr.rsplit_once('!') {
        Some((s, r)) => (Some(s.trim().trim_matches('\'').to_string()), r.trim()),
        None => (None, expr),
    };
    let target_sheet = match sheet_name.as_deref() {
        Some(n) if !n.is_empty() => wb.sheets.iter().find(|s| s.name == n)?,
        _ => wb.sheet(si),
    };
    let clean: String = range.chars().filter(|c| *c != '$').collect();
    let (a, b) = clean.split_once(':').unwrap_or((clean.as_str(), clean.as_str()));
    let (r1, c1) = parse_a1(a)?;
    let (r2, c2) = parse_a1(b)?;
    let mut out = Vec::new();
    for r in r1.min(r2)..=r1.max(r2) {
        for c in c1.min(c2)..=c1.max(c2) {
            if let Some(v) = target_sheet.get(r, c) {
                let t = v.value.display();
                if !t.is_empty() {
                    out.push(t);
                }
            }
        }
    }
    Some(out)
}

fn parse_a1(s: &str) -> Option<(u32, u32)> {
    let mut col = 0u32;
    let mut row = 0u32;
    let mut digits = false;
    for ch in s.chars() {
        if ch.is_ascii_alphabetic() && !digits {
            col = col * 26 + (ch.to_ascii_uppercase() as u32 - 'A' as u32 + 1);
        } else if ch.is_ascii_digit() {
            digits = true;
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

fn num_of(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => Some(*n),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        Value::Text(t) => t.trim().parse::<f64>().ok(),
        _ => None,
    }
}

fn parse_bound(s: &str) -> Option<f64> {
    let s = s.trim().trim_start_matches('=');
    if let Ok(v) = s.parse::<f64>() {
        return Some(v);
    }
    // DATE(2026,1,1)
    if let Some(inner) = s.strip_prefix("DATE(").and_then(|x| x.strip_suffix(')')) {
        let p: Vec<&str> = inner.split(',').collect();
        if p.len() == 3 {
            let y = p[0].trim().parse::<i32>().ok()?;
            let m = p[1].trim().parse::<u32>().ok()?;
            let d = p[2].trim().parse::<u32>().ok()?;
            return crate::formula::date_to_serial_pub(y, m, d);
        }
    }
    None
}

fn cmp_ok(op: DvOp, v: f64, a: f64, b: Option<f64>) -> bool {
    match op {
        DvOp::Between => v >= a && v <= b.unwrap_or(a),
        DvOp::NotBetween => !(v >= a && v <= b.unwrap_or(a)),
        DvOp::Equal => (v - a).abs() < 1e-9,
        DvOp::NotEqual => (v - a).abs() >= 1e-9,
        DvOp::Greater => v > a,
        DvOp::Less => v < a,
        DvOp::GreaterEq => v >= a,
        DvOp::LessEq => v <= a,
    }
}

/// 求值一个数值边界。先尝试字面量/日期，再走公式求值器；
/// 支持命名区域、`=A1*2` 这类阈值，而不是把无法解析的公式静默当 0。
fn bound_value<F>(expr: &str, r: u32, c: u32, formula_eval: &mut F) -> Option<f64>
where
    F: FnMut(&str, u32, u32) -> crate::formula::FValue,
{
    let raw = expr.trim().trim_start_matches('=').trim();
    if let Some(n) = parse_bound(raw) {
        return Some(n);
    }
    match formula_eval(expr, r, c) {
        crate::formula::FValue::Number(n) => Some(n),
        crate::formula::FValue::Bool(b) => Some(if b { 1.0 } else { 0.0 }),
        _ => None,
    }
}

/// 校验一个值是否合法。`formula_eval` 同时用于自定义公式和公式阈值。
/// 这个入口只带当前表上下文；需要命名区域/跨表列表时用 `validate_wb`。
pub fn validate<F>(dv: &DataValidation, value: &Value, sheet: &Sheet, r: u32, c: u32, formula_eval: F) -> bool
where
    F: FnMut(&str, u32, u32) -> crate::formula::FValue,
{
    validate_impl(dv, value, sheet, r, c, None, formula_eval)
}

/// 带工作簿上下文的校验入口：列表来源可解析为命名区域或跨表区域。
pub fn validate_wb<F>(
    wb: &crate::model::Workbook,
    si: usize,
    dv: &DataValidation,
    value: &Value,
    r: u32,
    c: u32,
    formula_eval: F,
) -> bool
where
    F: FnMut(&str, u32, u32) -> crate::formula::FValue,
{
    let items = list_items_wb(wb, si, dv).unwrap_or_default();
    validate_impl(dv, value, wb.sheet(si), r, c, Some(&items), formula_eval)
}

fn validate_impl<F>(
    dv: &DataValidation,
    value: &Value,
    sheet: &Sheet,
    r: u32,
    c: u32,
    resolved_list: Option<&[String]>,
    mut formula_eval: F,
) -> bool
where
    F: FnMut(&str, u32, u32) -> crate::formula::FValue,
{
    if matches!(value, Value::Empty) {
        return dv.allow_blank;
    }
    match dv.kind {
        DvKind::Any => true,
        DvKind::List => {
            let items = match resolved_list {
                Some(v) => v.to_vec(),
                None => list_items(dv, sheet),
            };
            // 带工作簿上下文时，空列表意味着来源无法解析或确实没有候选；
            // 不能像旧逻辑那样把非法值全部放行。
            if items.is_empty() {
                return resolved_list.is_none();
            }
            let text = value.display();
            items.iter().any(|i| i == &text)
        }
        DvKind::Custom => {
            let f = dv.f1.trim();
            if f.is_empty() {
                return true;
            }
            matches!(formula_eval(f, r, c), crate::formula::FValue::Bool(true))
        }
        DvKind::TextLength => {
            let len = value.display().chars().count() as f64;
            let Some(a) = bound_value(&dv.f1, r, c, &mut formula_eval) else { return false };
            let b = if dv.op.needs_pair() {
                bound_value(&dv.f2, r, c, &mut formula_eval)
            } else {
                None
            };
            if dv.op.needs_pair() && b.is_none() {
                return false;
            }
            cmp_ok(dv.op, len, a, b)
        }
        DvKind::Whole | DvKind::Decimal | DvKind::Date | DvKind::Time => {
            let Some(v) = num_of(value) else { return false };
            if dv.kind == DvKind::Whole && (v.fract() != 0.0) {
                return false;
            }
            let Some(a) = bound_value(&dv.f1, r, c, &mut formula_eval) else { return false };
            let b = if dv.op.needs_pair() {
                bound_value(&dv.f2, r, c, &mut formula_eval)
            } else {
                None
            };
            if dv.op.needs_pair() && b.is_none() {
                return false;
            }
            cmp_ok(dv.op, v, a, b)
        }
    }
}

/// 生成错误提示
pub fn error_text(dv: &DataValidation) -> String {
    if dv.error_msg.is_empty() {
        format!("输入值非法（{}）", dv.kind.label())
    } else {
        format!("{}：{}", dv.error_title, dv.error_msg)
    }
}

// ---------------------------------------------------------------------------
// XML 解析
// ---------------------------------------------------------------------------

use quick_xml::events::Event;
use quick_xml::Reader;

pub fn parse_validations(xml: &str) -> Vec<DataValidation> {
    let mut out: Vec<DataValidation> = Vec::new();
    let mut r = Reader::from_str(xml);
    let mut cur: Option<DataValidation> = None;
    let mut has_range = false;
    let mut field: Option<String> = None;
    let mut buf = String::new();
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) if e.name().as_ref().ends_with("dataValidation") => {
                let mut dv = DataValidation::default();
                has_range = false;
                let mut show_dropdown_attr: Option<bool> = None;
                for a in e.attributes().flatten() {
                    let k = a.key.as_ref().to_string();
                    let v = crate::model::xml_unescape(&a.value.to_string());
                    match k.as_str() {
                        "type" => dv.kind = DvKind::from_xml(&v),
                        "operator" => dv.op = DvOp::from_xml(&v),
                        "allowBlank" => dv.allow_blank = v == "1" || v == "true",
                        "showDropDown" => show_dropdown_attr = Some(v == "1" || v == "true"),
                        "sqref" => {
                            dv.ranges = crate::cf::parse_sqref(&v);
                            has_range = !dv.ranges.is_empty();
                        }
                        "formula1" => dv.f1 = v,
                        "formula2" => dv.f2 = v,
                        "errorTitle" => dv.error_title = v,
                        "error" => dv.error_msg = v,
                        "promptTitle" => dv.prompt_title = v,
                        "prompt" => dv.prompt_msg = v,
                        _ => {}
                    }
                }
                dv.show_dropdown = !show_dropdown_attr.unwrap_or(false);
                cur = Some(dv);
            }
            Ok(Event::Empty(e)) if e.name().as_ref().ends_with("dataValidation") => {
                let mut dv = DataValidation::default();
                has_range = false;
                let mut show_dropdown_attr: Option<bool> = None;
                for a in e.attributes().flatten() {
                    let k = a.key.as_ref().to_string();
                    let v = crate::model::xml_unescape(&a.value.to_string());
                    match k.as_str() {
                        "type" => dv.kind = DvKind::from_xml(&v),
                        "operator" => dv.op = DvOp::from_xml(&v),
                        "allowBlank" => dv.allow_blank = v == "1" || v == "true",
                        "showDropDown" => show_dropdown_attr = Some(v == "1" || v == "true"),
                        "sqref" => {
                            dv.ranges = crate::cf::parse_sqref(&v);
                            has_range = !dv.ranges.is_empty();
                        }
                        "formula1" => dv.f1 = v,
                        "formula2" => dv.f2 = v,
                        "errorTitle" => dv.error_title = v,
                        "error" => dv.error_msg = v,
                        "promptTitle" => dv.prompt_title = v,
                        "prompt" => dv.prompt_msg = v,
                        _ => {}
                    }
                }
                dv.show_dropdown = !show_dropdown_attr.unwrap_or(false);
                if has_range {
                    out.push(dv);
                }
            }
            Ok(Event::Start(e))
                if cur.is_some()
                    && matches!(
                        e.name().as_ref() as &str,
                        x if x.ends_with("formula1") || x.ends_with("formula2")
                    ) =>
            {
                field = Some(e.name().as_ref().to_string());
                buf.clear();
            }
            Ok(Event::Text(t)) if field.is_some() => {
                let raw: &str = &t;
                buf.push_str(&crate::model::xml_unescape(raw));
            }
            Ok(Event::End(e)) if e.name().as_ref().ends_with("dataValidation") => {
                if let Some(dv) = cur.take() {
                    if has_range {
                        out.push(dv);
                    }
                }
            }
            Ok(Event::End(e)) if field.is_some() => {
                if let Some(dv) = cur.as_mut() {
                    let is_f1 = e.name().as_ref().ends_with("formula1");
                    if is_f1 {
                        dv.f1 = buf.trim().to_string();
                    } else if e.name().as_ref().ends_with("formula2") {
                        dv.f2 = buf.trim().to_string();
                    }
                }
                field = None;
                buf.clear();
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Cell;

    fn sheet() -> Sheet {
        let mut s = Sheet::new("T");
        s.set(0, 0, Cell { value: Value::Text("甲".into()), formula: None });
        s.set(1, 0, Cell { value: Value::Text("乙".into()), formula: None });
        s
    }

    #[test]
    fn list_inline_and_range() {
        let sh = sheet();
        let mut dv = DataValidation::default();
        dv.kind = DvKind::List;
        dv.f1 = "\"红,绿,蓝\"".into();
        assert_eq!(list_items(&dv, &sh), vec!["红", "绿", "蓝"]);
        dv.f1 = "$A$1:$A$2".into();
        assert_eq!(list_items(&dv, &sh), vec!["甲", "乙"]);
        // 校验
        let items_dv = DataValidation { kind: DvKind::List, f1: "\"红,绿\"".into(), ..Default::default() };
        assert!(validate(&items_dv, &Value::Text("红".into()), &sh, 0, 0, |_, _, _| crate::formula::FValue::Bool(true)));
        assert!(!validate(&items_dv, &Value::Text("黄".into()), &sh, 0, 0, |_, _, _| crate::formula::FValue::Bool(true)));
        assert!(validate(&items_dv, &Value::Empty, &sh, 0, 0, |_, _, _| crate::formula::FValue::Bool(true)), "空值默认允许");
    }

    #[test]
    fn whole_and_decimal_bounds() {
        let sh = sheet();
        let dv = DataValidation {
            kind: DvKind::Whole,
            op: DvOp::Between,
            f1: "1".into(),
            f2: "10".into(),
            ..Default::default()
        };
        assert!(validate(&dv, &Value::Number(5.0), &sh, 0, 0, |_, _, _| crate::formula::FValue::Bool(true)));
        assert!(!validate(&dv, &Value::Number(11.0), &sh, 0, 0, |_, _, _| crate::formula::FValue::Bool(true)));
        assert!(!validate(&dv, &Value::Number(5.5), &sh, 0, 0, |_, _, _| crate::formula::FValue::Bool(true)), "整型不接受小数");
        assert!(!validate(&dv, &Value::Text("abc".into()), &sh, 0, 0, |_, _, _| crate::formula::FValue::Bool(true)));

        let dv2 = DataValidation {
            kind: DvKind::Decimal,
            op: DvOp::Greater,
            f1: "0".into(),
            ..Default::default()
        };
        assert!(validate(&dv2, &Value::Number(0.5), &sh, 0, 0, |_, _, _| crate::formula::FValue::Bool(true)));
        assert!(!validate(&dv2, &Value::Number(0.0), &sh, 0, 0, |_, _, _| crate::formula::FValue::Bool(true)));
    }

    #[test]
    fn text_length_and_custom() {
        let sh = sheet();
        let dv = DataValidation {
            kind: DvKind::TextLength,
            op: DvOp::LessEq,
            f1: "3".into(),
            ..Default::default()
        };
        assert!(validate(&dv, &Value::Text("abc".into()), &sh, 0, 0, |_, _, _| crate::formula::FValue::Bool(true)));
        assert!(!validate(&dv, &Value::Text("abcd".into()), &sh, 0, 0, |_, _, _| crate::formula::FValue::Bool(true)));

        let custom = DataValidation { kind: DvKind::Custom, f1: "MOD(A1,2)=0".into(), ..Default::default() };
        assert!(validate(&custom, &Value::Number(4.0), &sh, 0, 0, |_, _, _| crate::formula::FValue::Bool(true)));
        assert!(!validate(&custom, &Value::Number(4.0), &sh, 0, 0, |_, _, _| crate::formula::FValue::Bool(false)));
    }

    #[test]
    fn workbook_lists_and_formula_bounds() {
        use crate::model::{Cell, Value, Workbook};
        let mut wb = Workbook::new_blank();
        wb.sheets[0].set(0, 0, Cell { value: Value::Number(1.0), formula: None });
        let mut list = crate::model::Sheet::default();
        list.name = "列表".into();
        list.set(0, 0, Cell { value: Value::Text("甲".into()), formula: None });
        list.set(1, 0, Cell { value: Value::Text("乙".into()), formula: None });
        wb.sheets.push(list);
        wb.names.push(("甲乙".into(), None, "'列表'!$A$1:$A$2".into()));

        let named = DataValidation { kind: DvKind::List, f1: "=甲乙".into(), ..Default::default() };
        assert!(validate_wb(&wb, 0, &named, &Value::Text("甲".into()), 0, 0, |_, _, _| crate::formula::FValue::Bool(true)));
        assert!(!validate_wb(&wb, 0, &named, &Value::Text("丙".into()), 0, 0, |_, _, _| crate::formula::FValue::Bool(true)));

        // 跨表区域，不经过名称
        let cross = DataValidation { kind: DvKind::List, f1: "列表!$A$1:$A$2".into(), ..Default::default() };
        assert!(validate_wb(&wb, 0, &cross, &Value::Text("乙".into()), 0, 0, |_, _, _| crate::formula::FValue::Bool(true)));
        assert!(!validate_wb(&wb, 0, &cross, &Value::Text("甲2".into()), 0, 0, |_, _, _| crate::formula::FValue::Bool(true)));

        // 无法解析的名称必须拒绝，不能退回“全部允许”
        let bad = DataValidation { kind: DvKind::List, f1: "=不存在".into(), ..Default::default() };
        assert!(!validate_wb(&wb, 0, &bad, &Value::Text("甲".into()), 0, 0, |_, _, _| crate::formula::FValue::Bool(true)));

        // 公式阈值 =A1；A1=1
        let mut calc = crate::formula::Calc::new(&wb);
        let bound = DataValidation {
            kind: DvKind::Decimal,
            op: DvOp::GreaterEq,
            f1: "=$A$1".into(),
            ..Default::default()
        };
        let mut eval = |f: &str, rr: u32, cc: u32| {
            let shifted = crate::cf::shift_formula(f.trim_start_matches('='), rr, cc);
            crate::formula::parse(&shifted)
                .map(|n| calc.eval_node(&n, 0))
                .unwrap_or(crate::formula::FValue::Error(crate::formula::ErrKind::Value))
        };
        assert!(!validate_wb(&wb, 0, &bound, &Value::Number(0.0), 0, 1, &mut eval));
        assert!(validate_wb(&wb, 0, &bound, &Value::Number(2.0), 0, 1, &mut eval));
    }

    #[test]
    fn parses_xml() {
        let xml = r#"<dataValidations count="2">
            <dataValidation type="list" allowBlank="1" sqref="B2:B10" formula1="&quot;甲,乙,丙&quot;"/>
            <dataValidation type="whole" operator="between" sqref="C2 C5" formula1="1" formula2="100" showDropDown="1" errorTitle="越界" error="只能填 1-100"/>
        </dataValidations>"#;
        let list = parse_validations(xml);
        assert_eq!(list.len(), 2);
        assert_eq!(list[0].kind, DvKind::List);
        assert_eq!(list[0].ranges, vec![(1, 1, 9, 1)]);
        assert!(list[0].show_dropdown, "没有 showDropDown 属性时应显示箭头");
        assert_eq!(list[1].op, DvOp::Between);
        assert_eq!(list[1].ranges.len(), 2);
        assert!(!list[1].show_dropdown, "showDropDown=1 表示隐藏箭头");
        assert_eq!(list[1].error_title, "越界");
    }
}
