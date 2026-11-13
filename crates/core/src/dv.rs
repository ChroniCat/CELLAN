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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DvStyle {
    Stop,
    Warning,
    Information,
}

impl DvStyle {
    pub fn label(&self) -> &'static str {
        match self {
            DvStyle::Stop => "停止",
            DvStyle::Warning => "警告",
            DvStyle::Information => "信息",
        }
    }
    pub fn from_xml(s: &str) -> Self {
        match s {
            "warning" => DvStyle::Warning,
            "information" => DvStyle::Information,
            _ => DvStyle::Stop,
        }
    }
    pub fn to_xml(&self) -> &'static str {
        match self {
            DvStyle::Stop => "stop",
            DvStyle::Warning => "warning",
            DvStyle::Information => "information",
        }
    }
}

/// 校验结果的处置：通过、静默放行、停止（留在编辑框）、警告（问是/否/取消）、
/// 信息（先提示再接受）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DvVerdict {
    Accept,
    AcceptSilently,
    Reject,
    AskWarning,
    InformThenAccept,
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
    pub error_style: DvStyle,
    /// showErrorMessage；关掉时 Excel 不拦输入
    pub show_error: bool,
    /// showInputMessage；选中单元格时显示输入提示
    pub show_prompt: bool,
    pub error_title: String,
    pub error_msg: String,
    pub prompt_title: String,
    pub prompt_msg: String,
}

impl DataValidation {
    /// 不合规值的处置。show_error=false 时静默放行（Excel 的行为），
    /// 否则按 errorStyle 分停止/警告/信息。
    pub fn verdict(&self, valid: bool) -> DvVerdict {
        if valid {
            return DvVerdict::Accept;
        }
        if !self.show_error {
            return DvVerdict::AcceptSilently;
        }
        match self.error_style {
            DvStyle::Stop => DvVerdict::Reject,
            DvStyle::Warning => DvVerdict::AskWarning,
            DvStyle::Information => DvVerdict::InformThenAccept,
        }
    }

    /// 选中格要显示的输入提示；开关关掉或内容为空时不显示。
    pub fn prompt_text(&self) -> Option<String> {
        if !self.show_prompt {
            return None;
        }
        let t = self.prompt_title.trim();
        let m = self.prompt_msg.trim();
        if t.is_empty() && m.is_empty() {
            return None;
        }
        Some(match (t.is_empty(), m.is_empty()) {
            (false, false) => format!("{t}\n{m}"),
            (false, true) => t.to_string(),
            _ => m.to_string(),
        })
    }
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
            error_style: DvStyle::Stop,
            show_error: true,
            show_prompt: true,
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

/// 数据验证公式的基准是规则范围左上角；相对/混合引用要按这个基准平移。
pub fn range_anchor(dv: &DataValidation, r: u32, c: u32) -> (u32, u32) {
    dv.ranges
        .iter()
        .find(|(r1, c1, r2, c2)| r >= *r1 && r <= *r2 && c >= *c1 && c <= *c2)
        .map(|(r1, c1, _, _)| (*r1, *c1))
        .unwrap_or((0, 0))
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

/// 内联序列 `"甲,乙"`（Excel 只认英文双引号）。
fn inline_items(src: &str) -> Option<Vec<String>> {
    let rest = src.trim().strip_prefix('"')?.strip_suffix('"')?;
    Some(rest.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
}

/// 按 `!` 拆工作表前缀：跳过单引号表名（`''` 是转义的单引号）和双引号字符串。
/// 这样 `'预算(2026)'!$A$1:$A$2` 的括号不会被当成函数调用。
fn split_sheet_prefix(text: &str) -> (Option<String>, String) {
    let chars: Vec<char> = text.trim().chars().collect();
    let mut in_quote = false;
    let mut last: Option<usize> = None;
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '\'' => {
                if in_quote && i + 1 < chars.len() && chars[i + 1] == '\'' {
                    i += 2;
                    continue;
                }
                in_quote = !in_quote;
            }
            '!' if !in_quote => {
                if last.is_none() {
                    last = Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    match last {
        Some(idx) => {
            let head: String = chars[..idx].iter().collect();
            let tail: String = chars[idx + 1..].iter().collect();
            (Some(unquote_sheet_name(head.trim())), tail.trim().to_string())
        }
        None => (None, text.trim().to_string()),
    }
}

/// 把一个端点拆成（列字母，行数字）。
fn split_endpoint(s: &str) -> Option<(String, String)> {
    let mut letters = String::new();
    let mut digits = String::new();
    for ch in s.chars() {
        if ch.is_ascii_alphabetic() && digits.is_empty() {
            letters.push(ch);
        } else if ch.is_ascii_digit() {
            digits.push(ch);
        } else {
            return None;
        }
    }
    if letters.is_empty() && digits.is_empty() {
        return None;
    }
    Some((letters, digits))
}

/// 解析 A1 / A1:B2 / A:A / 1:1 / $A$1:$B$2 → 0 基矩形。
fn parse_rect(text: &str) -> Option<(u32, u32, u32, u32)> {
    const MAXR: u32 = 1_048_575;
    const MAXC: u32 = 16_383;
    let clean: String = text
        .chars()
        .filter(|c| *c != '$' && !c.is_whitespace())
        .collect();
    if clean.is_empty() {
        return None;
    }
    let (a, b) = match clean.split_once(':') {
        Some((a, b)) => (a, b),
        None => (clean.as_str(), clean.as_str()),
    };
    let (al, ad) = split_endpoint(a)?;
    let (bl, bd) = split_endpoint(b)?;
    let col_of = |letters: &str| -> Option<u32> {
        if letters.is_empty() {
            None
        } else {
            Some(parse_a1(&format!("{letters}1"))?.1)
        }
    };
    // 行号写坏了就是非法引用：不能因为溢出退化成"整列"
    let row_of = |digits: &str| -> Result<Option<u32>, ()> {
        if digits.is_empty() {
            Ok(None)
        } else {
            let n: u32 = digits.parse().map_err(|_| ())?;
            let zero = n.checked_sub(1).ok_or(())?;
            if zero > MAXR {
                return Err(());
            }
            Ok(Some(zero))
        }
    };
    let ac = col_of(&al);
    let ar = row_of(&ad).ok()?;
    let bc = col_of(&bl);
    let br = row_of(&bd).ok()?;
    // 行/列必须成对出现：A:5 这种混合端点不合法
    if ar.is_some() != br.is_some() || ac.is_some() != bc.is_some() {
        return None;
    }
    let (r1, r2) = match (ar, br) {
        (Some(x), Some(y)) => (x.min(y), x.max(y)),
        _ => (0, MAXR),
    };
    let (c1, c2) = match (ac, bc) {
        (Some(x), Some(y)) => (x.min(y), x.max(y)),
        _ => (0, MAXC),
    };
    if ar.is_none() && ac.is_none() {
        return None;
    }
    Some((r1, c1, r2, c2))
}

/// 取矩形里的非空显示文本；大区域（整列）走稀疏迭代，不逐格扫一百万行。
fn collect_rect(sheet: &Sheet, rect: (u32, u32, u32, u32)) -> Vec<String> {
    let (r1, c1, r2, c2) = rect;
    let area = (r2 - r1 + 1) as u64 * (c2 - c1 + 1) as u64;
    if area <= 65_536 {
        let mut out = Vec::new();
        for r in r1..=r2 {
            for c in c1..=c2 {
                if let Some(v) = sheet.get(r, c) {
                    let t = v.value.display();
                    if !t.is_empty() {
                        out.push(t);
                    }
                }
            }
        }
        out
    } else {
        let mut hits: Vec<(u32, u32, String)> = sheet
            .iter()
            .filter(|(k, _)| k.row >= r1 && k.row <= r2 && k.col >= c1 && k.col <= c2)
            .map(|(k, v)| (k.row, k.col, v.value.display()))
            .filter(|(_, _, t)| !t.is_empty())
            .collect();
        hits.sort_by_key(|(r, c, _)| (*r, *c));
        hits.into_iter().map(|(_, _, t)| t).collect()
    }
}

fn count_rect(sheet: &Sheet, rect: (u32, u32, u32, u32), numeric_only: bool) -> f64 {
    let (r1, c1, r2, c2) = rect;
    sheet
        .iter()
        .filter(|(k, _)| k.row >= r1 && k.row <= r2 && k.col >= c1 && k.col <= c2)
        .filter(|(_, v)| {
            if numeric_only {
                matches!(v.value, Value::Number(_))
            } else {
                !matches!(v.value, Value::Empty)
            }
        })
        .count() as f64
}

/// 列表来源的解析结果：是引用就取项，像引用但坏了就报配置错误，否则交给动态公式。
enum ListSrc {
    Items(Vec<String>),
    Invalid,
    NotRef,
}

fn static_source(wb: &crate::model::Workbook, si: usize, text: &str) -> ListSrc {
    let (sheet_name, range) = split_sheet_prefix(text);
    // 区域部分必须整段就是一个引用（A1、A1:B2、A:A）；否则这是公式，不是引用。
    let Some(rect) = parse_rect(&range) else {
        return ListSrc::NotRef;
    };
    let sheet = match sheet_name.as_deref() {
        Some(n) if !n.is_empty() => match wb.sheets.iter().find(|s| s.name == n) {
            Some(s) => s,
            None => return ListSrc::Invalid,
        },
        _ => wb.sheet(si),
    };
    ListSrc::Items(collect_rect(sheet, rect))
}

/// 整个表达式就是一个函数调用时取参数文本：`OFFSET(a,b)` → `a,b`。
fn func_args<'a>(expr: &'a str, name: &str) -> Option<&'a str> {
    let t = expr.trim();
    let head = t.get(..name.len())?;
    if !head.eq_ignore_ascii_case(name) {
        return None;
    }
    let rest = t.get(name.len()..)?.trim_start();
    let rest = rest.strip_prefix('(')?;
    let rest = rest.strip_suffix(')')?;
    Some(rest)
}

/// 按顶层逗号切参数（跳过字符串和嵌套括号）。
fn split_args(args: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut depth = 0i32;
    let mut in_str = false;
    for ch in args.chars() {
        match ch {
            '"' => {
                in_str = !in_str;
                cur.push(ch);
            }
            '(' | '{' if !in_str => {
                depth += 1;
                cur.push(ch);
            }
            ')' | '}' if !in_str => {
                depth -= 1;
                cur.push(ch);
            }
            ',' if !in_str && depth == 0 => {
                out.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(ch),
        }
    }
    if !cur.trim().is_empty() || !out.is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

/// 求一个整数参数：字面量、COUNTA/COUNT/ROWS/COLUMNS(区域)，以及它们的 + - * / 组合。
fn eval_int(wb: &crate::model::Workbook, default_sheet: &Sheet, expr: &str) -> Option<i64> {
    let t = expr.trim();
    if t.is_empty() {
        return None;
    }
    if let Ok(v) = t.parse::<i64>() {
        return Some(v);
    }
    if let Some(rest) = t.strip_prefix('-') {
        return eval_int(wb, default_sheet, rest.trim()).map(|v| -v);
    }
    for op in ['+', '-', '*', '/'] {
        if let Some(idx) = top_level_op(t, op) {
            let a = eval_int(wb, default_sheet, &t[..idx])?;
            let b = eval_int(wb, default_sheet, &t[idx + 1..])?;
            return match op {
                '+' => Some(a + b),
                '-' => Some(a - b),
                '*' => Some(a * b),
                '/' if b != 0 => Some(a / b),
                _ => None,
            };
        }
    }
    for (fname, mode) in [("COUNTA", 0), ("COUNT", 1), ("ROWS", 2), ("COLUMNS", 3)] {
        if let Some(args) = func_args(t, fname) {
            let (sheet_name, range) = split_sheet_prefix(args.trim());
            let sheet = match sheet_name.as_deref() {
                Some(n) if !n.is_empty() => wb.sheets.iter().find(|s| s.name == n)?,
                _ => default_sheet,
            };
            let rect = parse_rect(&range)?;
            return Some(match mode {
                0 => count_rect(sheet, rect, false) as i64,
                1 => count_rect(sheet, rect, true) as i64,
                2 => (rect.2 as i64 - rect.0 as i64 + 1) as i64,
                _ => (rect.3 as i64 - rect.1 as i64 + 1) as i64,
            });
        }
    }
    None
}

/// 顶层（不在括号/字符串里）的运算符位置；取最右边，保证左结合。
fn top_level_op(expr: &str, want: char) -> Option<usize> {
    let mut depth = 0i32;
    let mut in_str = false;
    let mut hit: Option<usize> = None;
    let mut prev: Option<char> = None;
    for (i, ch) in expr.char_indices() {
        match ch {
            '"' => in_str = !in_str,
            '(' if !in_str => depth += 1,
            ')' if !in_str => depth -= 1,
            c if !in_str && depth == 0 && c == want => {
                // 一元正负号不是运算符
                if (want == '+' || want == '-')
                    && matches!(prev, None | Some('+') | Some('-') | Some('*') | Some('/') | Some('('))
                {
                    prev = Some(c);
                    continue;
                }
                hit = Some(i);
            }
            _ => {}
        }
        prev = Some(ch);
    }
    hit
}

/// 动态来源：INDIRECT("区域") 与 OFFSET(锚点,行,列[,高[,宽]])。
/// 本机求值器没有整块数组求值，这里按区域语义解析，不用数组字面量冒充公式。
/// 去掉空格和 $ 后的规范形，用来判断 FILTER 的条件是不是"同一区域非空"。
fn norm_ref(s: &str) -> String {
    s.chars()
        .filter(|c| *c != '$' && !c.is_whitespace())
        .collect::<String>()
        .to_ascii_uppercase()
}

/// `FILTER(区域, 区域<>"")` —— Excel 里最常见的动态列表写法（去掉空白）。
fn is_nonblank_filter(range: &str, cond: &str) -> bool {
    let want = format!("{}<>\"\"", norm_ref(range));
    norm_ref(cond) == want
}

/// 数字优先、其余按文本排序，和 Excel 的 SORT 默认升序一致。
fn sort_items(items: &mut [String]) {
    items.sort_by(|a, b| match (a.parse::<f64>(), b.parse::<f64>()) {
        (Ok(x), Ok(y)) => x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal),
        (Ok(_), Err(_)) => std::cmp::Ordering::Less,
        (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
        (Err(_), Err(_)) => a.cmp(b),
    });
}

/// 把来源文本解析成候选（静态引用或动态公式），供 UNIQUE/SORT 之类包装函数复用。
fn resolve_any(wb: &crate::model::Workbook, si: usize, text: &str) -> Option<Vec<String>> {
    match static_source(wb, si, text) {
        ListSrc::Items(v) => Some(v),
        ListSrc::Invalid => None,
        ListSrc::NotRef => dynamic_source(wb, si, text),
    }
}

fn dynamic_source(wb: &crate::model::Workbook, si: usize, expr: &str) -> Option<Vec<String>> {
    // FILTER(区域, 区域<>"")：现代 Excel 的动态列表写法
    if let Some(args) = func_args(expr, "FILTER") {
        let parts = split_args(args);
        if parts.len() != 2 {
            return None;
        }
        if is_nonblank_filter(&parts[0], &parts[1]) {
            return resolve_any(wb, si, &parts[0]);
        }
        return None;
    }
    // UNIQUE(来源)：去重，保留首次出现的顺序
    if let Some(args) = func_args(expr, "UNIQUE") {
        let parts = split_args(args);
        let mut items = resolve_any(wb, si, parts.first()?)?;
        let mut seen = std::collections::HashSet::new();
        items.retain(|it| seen.insert(it.clone()));
        return Some(items);
    }
    // SORT(来源[, 排序索引, 方向])：默认按第一列升序
    if let Some(args) = func_args(expr, "SORT") {
        let parts = split_args(args);
        let mut items = resolve_any(wb, si, parts.first()?)?;
        let desc = parts
            .get(2)
            .map(|d| d.trim() == "-1")
            .unwrap_or(false);
        sort_items(&mut items);
        if desc {
            items.reverse();
        }
        return Some(items);
    }
    if let Some(args) = func_args(expr, "INDIRECT") {
        let parts = split_args(args);
        let text = parts.first()?.trim().trim_matches('"');
        return match static_source(wb, si, text) {
            ListSrc::Items(v) => Some(v),
            _ => None,
        };
    }
    if let Some(args) = func_args(expr, "OFFSET") {
        let parts = split_args(args);
        if parts.len() < 3 {
            return None;
        }
        // OFFSET 的其余参数在「公式所在表」里求值：跨表 reference 只决定从哪张表取数，
        // 不会把 COUNTA($A:$A) 这类不带表名的参数换到 reference 的表上。
        // 本机 Excel 16.0 实测：主表 A 列 2 个值、数据表 5 个值时
        // =ROWS(OFFSET(数据!$A$1,0,0,COUNTA($A:$A),1)) 得 2。
        let ctx = wb.sheet(si);
        let (mut sheet_name, anchor_txt) = split_sheet_prefix(&parts[0]);
        let anchor = match parse_rect(&anchor_txt) {
            Some(r) => r,
            None => {
                // reference 允许是名称：名称指向区域时按区域处理
                let target = lookup_name(wb, si, anchor_txt.trim())?;
                let t = target.trim().trim_start_matches('=').trim();
                let (name_sheet, inner) = split_sheet_prefix(t);
                if let Some(n) = name_sheet {
                    sheet_name = Some(n);
                }
                parse_rect(&inner)?
            }
        };
        let sheet = match sheet_name.as_deref() {
            Some(n) if !n.is_empty() => wb.sheets.iter().find(|s| s.name == n)?,
            _ => wb.sheet(si),
        };
        let (ar, ac, ar2, ac2) = anchor;
        // 省略 height/width 时继承 reference 自身的行列数（不是 1）
        let def_h = ar2 as i64 - ar as i64 + 1;
        let def_w = ac2 as i64 - ac as i64 + 1;
        let rows = eval_int(wb, ctx, &parts[1])?;
        let cols = eval_int(wb, ctx, &parts[2])?;
        let h = if parts.len() > 3 && !parts[3].trim().is_empty() {
            eval_int(wb, ctx, &parts[3])?
        } else {
            def_h
        };
        let w = if parts.len() > 4 && !parts[4].trim().is_empty() {
            eval_int(wb, ctx, &parts[4])?
        } else {
            def_w
        };
        if h == 0 || w == 0 {
            return Some(Vec::new());
        }
        let r0 = ar as i64 + rows;
        let c0 = ac as i64 + cols;
        // 负数 height/width 向反方向展开：OFFSET($A$3,0,0,-2,1) = $A$2:$A$3
        let (r1, r2) = if h > 0 { (r0, r0 + h - 1) } else { (r0 + h + 1, r0) };
        let (c1, c2) = if w > 0 { (c0, c0 + w - 1) } else { (c0 + w + 1, c0) };
        if r1 < 0 || c1 < 0 || r2 > 1_048_575 || c2 > 16_383 {
            // 越界在 Excel 里是 #REF!，这里按配置无效拒绝
            return None;
        }
        return Some(collect_rect(sheet, (r1 as u32, c1 as u32, r2 as u32, c2 as u32)));
    }
    None
}

fn lookup_name(wb: &crate::model::Workbook, si: usize, name: &str) -> Option<String> {
    let mut global: Option<&str> = None;
    for (n, scope, target) in wb.names.iter().rev() {
        if !n.eq_ignore_ascii_case(name) {
            continue;
        }
        if *scope == Some(si) {
            return Some(target.clone());
        }
        if scope.is_none() && global.is_none() {
            global = Some(target);
        }
    }
    global.map(|s| s.to_string())
}

/// 从工作簿上下文解析列表来源：内联、区域引用、名称、INDIRECT/OFFSET 动态来源。
/// `r/c` 是正在校验的目标单元格：Excel 的数据验证公式以规则范围左上角为基准，
/// 每个单元格看到的是平移后的 Formula1（本机 Excel COM 实测：C2:C3 用 =$A1:$A2，
/// C3 的 Formula1 为 =$A2:$A3，C3 接受 A3 的值、拒绝 A2 的值）。
/// 返回 `None` 表示配置无法解析；调用方必须拒绝并给出诊断，不能当成“无限制”。
pub fn list_items_wb(
    wb: &crate::model::Workbook,
    si: usize,
    dv: &DataValidation,
    r: u32,
    c: u32,
) -> Option<Vec<String>> {
    let src = dv.f1.trim();
    if src.is_empty() {
        return Some(Vec::new());
    }
    if let Some(items) = inline_items(src) {
        return Some(items);
    }
    let mut expr = src.strip_prefix('=').unwrap_or(src).trim().to_string();
    for _ in 0..8 {
        // 裸名称才查名称表；带 ! 或 : 的是引用，带括号的是公式
        if expr.contains('!') || expr.contains(':') || expr.contains('(') {
            break;
        }
        let Some(target) = lookup_name(wb, si, &expr) else {
            break;
        };
        expr = target.trim().trim_start_matches('=').trim().to_string();
        if let Some(items) = inline_items(&expr) {
            return Some(items);
        }
    }
    let (ar, ac) = range_anchor(dv, r, c);
    let shifted = crate::cf::shift_formula(&expr, r - ar, c - ac);
    match static_source(wb, si, &shifted) {
        ListSrc::Items(v) => Some(v),
        ListSrc::Invalid => None,
        ListSrc::NotRef => dynamic_source(wb, si, &shifted),
    }
}

fn unquote_sheet_name(s: &str) -> String {
    let s = s.trim();
    if s.len() >= 2 && s.starts_with('\'') && s.ends_with('\'') {
        // Excel 的 O'Brien 写作 'O''Brien'!A1
        s[1..s.len() - 1].replace("''", "'")
    } else {
        s.to_string()
    }
}

fn parse_a1(s: &str) -> Option<(u32, u32)> {
    let mut col = 0u32;
    let mut row = 0u32;
    let mut digits = false;
    for ch in s.chars() {
        if ch.is_ascii_alphabetic() && !digits {
            col = col
                .checked_mul(26)?
                .checked_add(ch.to_ascii_uppercase() as u32 - 'A' as u32 + 1)?;
            if col > 16_384 {
                return None;
            }
        } else if ch.is_ascii_digit() {
            digits = true;
            row = row
                .checked_mul(10)?
                .checked_add(ch as u32 - '0' as u32)?;
            if row > 1_048_576 {
                return None;
            }
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
    if dv.kind == DvKind::List {
        let items = list_items_wb(wb, si, dv, r, c).unwrap_or_default();
        validate_impl(dv, value, wb.sheet(si), r, c, Some(&items), formula_eval)
    } else {
        validate_impl(dv, value, wb.sheet(si), r, c, None, formula_eval)
    }
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
                        "showErrorMessage" => dv.show_error = v == "1" || v == "true",
                        "showInputMessage" => dv.show_prompt = v == "1" || v == "true",
                        "errorStyle" => dv.error_style = DvStyle::from_xml(&v),
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
                        "showErrorMessage" => dv.show_error = v == "1" || v == "true",
                        "showInputMessage" => dv.show_prompt = v == "1" || v == "true",
                        "errorStyle" => dv.error_style = DvStyle::from_xml(&v),
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
            // quick_xml 把 &quot; 这类实体单独发成 GeneralRef；不接住的话
            // 内联序列 "甲,乙" 的引号会整个丢掉，重开后来源就解析不出来了。
            Ok(Event::GeneralRef(e)) if field.is_some() => {
                let name: String = e.into_inner().to_string();
                buf.push_str(&crate::model::resolve_entity(&name));
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
    fn threshold_follows_referenced_cell_and_list_skips_blanks() {
        use crate::model::{Cell, Value, Workbook};
        let mut wb = Workbook::new_blank();
        wb.sheets[0].set(0, 0, Cell { value: Value::Number(10.0), formula: None });
        let mut list = crate::model::Sheet::default();
        list.name = "列表".into();
        list.set(0, 0, Cell { value: Value::Text("甲".into()), formula: None });
        list.set(1, 0, Cell { value: Value::Text("乙".into()), formula: None });
        // A3/A4 留空：列表区域里的空白不得变成候选，也不得整段失效
        wb.sheets.push(list);

        // 单值区域
        let single = DataValidation { kind: DvKind::List, f1: "=列表!$A$1".into(), ..Default::default() };
        assert_eq!(
            list_items_wb(&wb, 0, &single, 0, 0).as_deref(),
            Some(&["甲".to_string()][..])
        );
        // 含空白的多格区域
        let gap = DataValidation { kind: DvKind::List, f1: "=列表!$A$1:$A$4".into(), ..Default::default() };
        assert_eq!(
            list_items_wb(&wb, 0, &gap, 0, 0).as_deref(),
            Some(&["甲".to_string(), "乙".to_string()][..]),
            "空白跳过，不能变成空候选"
        );
        let mut ev = |_: &str, _: u32, _: u32| crate::formula::FValue::Bool(true);
        assert!(validate_wb(&wb, 0, &gap, &Value::Text("乙".into()), 0, 0, &mut ev));
        // 空白输入由 allow_blank 决定，不走候选列表
        let strict = DataValidation { allow_blank: false, ..gap.clone() };
        assert!(!validate_wb(&wb, 0, &strict, &Value::Empty, 0, 0, &mut ev), "allow_blank=false 时空值拒绝");

        // 阈值引用 A1：改 A1 后同一规则立即跟着变
        let bound = DataValidation {
            kind: DvKind::Whole,
            op: DvOp::GreaterEq,
            f1: "=$A$1".into(),
            ..Default::default()
        };
        let check = |wb: &Workbook, v: f64| {
            let mut calc = crate::formula::Calc::new(wb);
            let mut ev = |f: &str, rr: u32, cc: u32| {
                let shifted = crate::cf::shift_formula(f.trim_start_matches('='), rr, cc);
                crate::formula::parse(&shifted)
                    .map(|n| calc.eval_node(&n, 0))
                    .unwrap_or(crate::formula::FValue::Error(crate::formula::ErrKind::Value))
            };
            validate_wb(wb, 0, &bound, &Value::Number(v), 0, 2, &mut ev)
        };
        assert!(check(&wb, 10.0), "A1=10 时 10 通过");
        assert!(!check(&wb, 9.0), "A1=10 时 9 拒绝");
        wb.sheets[0].set(0, 0, Cell { value: Value::Number(100.0), formula: None });
        assert!(check(&wb, 100.0), "A1 改成 100 后 100 通过");
        assert!(!check(&wb, 11.0), "A1 改成 100 后 11 拒绝");
    }

    #[test]
    fn unsafe_list_sources_do_not_panic_or_turn_into_values() {
        let wb = crate::model::Workbook::new_blank();
        for f1 in [
            "=SUMPRODUCT(1,2)",
            "=ABCDEFGHIJKLMNOPQRSTUVWXYZ",
            "=A1:A2:A3",
            "=A999999999999999999999",
        ] {
            let dv = DataValidation { kind: DvKind::List, f1: f1.into(), ..Default::default() };
            assert!(list_items_wb(&wb, 0, &dv, 0, 0).is_none(), "不应把 {f1} 当列表来源");
        }
    }

    /// 动态列表来源按 Excel 语义解析：OFFSET/INDIRECT/FILTER(非空)/名称，以及相对来源按
    /// 规则范围左上角平移。基准值来自本机 Excel COM 实测（见 docs 证据）。
    #[test]
    fn dynamic_list_sources_follow_excel_semantics() {
        use crate::model::{Cell, Sheet, Value, Workbook};
        let mut wb = Workbook::new_blank();
        for (i, v) in [10.0, 20.0, 30.0].iter().enumerate() {
            wb.sheets[0].set(i as u32, 0, Cell { value: Value::Number(*v), formula: None });
        }
        let mut list = Sheet::default();
        list.name = "列表".into();
        for (i, t) in ["甲", "乙", "丙"].iter().enumerate() {
            list.set(i as u32, 0, Cell { value: Value::Text((*t).into()), formula: None });
        }
        wb.sheets.push(list);
        let mut paren = Sheet::default();
        paren.name = "预算(2026)".into();
        paren.set(0, 0, Cell { value: Value::Text("甲".into()), formula: None });
        paren.set(1, 0, Cell { value: Value::Text("乙".into()), formula: None });
        wb.sheets.push(paren);
        wb.names.push(("动态".into(), None, "=OFFSET(列表!$A$1,0,0,COUNTA(列表!$A$1:$A$3),1)".into()));

        let off = DataValidation {
            kind: DvKind::List,
            f1: "=OFFSET(列表!$A$1,0,0,COUNTA(列表!$A$1:$A$4),1)".into(),
            ..Default::default()
        };
        assert_eq!(
            list_items_wb(&wb, 0, &off, 0, 0).as_deref(),
            Some(&["甲".to_string(), "乙".to_string(), "丙".to_string()][..])
        );

        let ind = DataValidation {
            kind: DvKind::List,
            f1: "=INDIRECT(\"列表!A1:A2\")".into(),
            ..Default::default()
        };
        assert_eq!(
            list_items_wb(&wb, 0, &ind, 0, 0).as_deref(),
            Some(&["甲".to_string(), "乙".to_string()][..])
        );

        let filt = DataValidation {
            kind: DvKind::List,
            f1: "=FILTER(列表!$A$1:$A$4,列表!$A$1:$A$4<>\"\")".into(),
            ..Default::default()
        };
        assert_eq!(
            list_items_wb(&wb, 0, &filt, 0, 0).as_deref(),
            Some(&["甲".to_string(), "乙".to_string(), "丙".to_string()][..])
        );

        let named = DataValidation { kind: DvKind::List, f1: "=动态".into(), ..Default::default() };
        assert_eq!(
            list_items_wb(&wb, 0, &named, 0, 0).as_deref(),
            Some(&["甲".to_string(), "乙".to_string(), "丙".to_string()][..])
        );

        let par = DataValidation {
            kind: DvKind::List,
            f1: "='预算(2026)'!$A$1:$A$2".into(),
            ..Default::default()
        };
        assert_eq!(
            list_items_wb(&wb, 0, &par, 0, 0).as_deref(),
            Some(&["甲".to_string(), "乙".to_string()][..]),
            "带括号的表名不能被当成动态公式拒绝"
        );
        let mut ev = |_: &str, _: u32, _: u32| crate::formula::FValue::Bool(true);
        assert!(validate_wb(&wb, 0, &par, &Value::Text("乙".into()), 0, 0, &mut ev));
        assert!(!validate_wb(&wb, 0, &par, &Value::Text("丙".into()), 0, 0, &mut ev));

        let rel = DataValidation {
            kind: DvKind::List,
            f1: "=$A1:$A2".into(),
            ranges: vec![(1, 2, 2, 2)],
            ..Default::default()
        };
        assert_eq!(
            list_items_wb(&wb, 0, &rel, 1, 2).as_deref(),
            Some(&["10".to_string(), "20".to_string()][..]),
            "C2 看 A1:A2"
        );
        assert_eq!(
            list_items_wb(&wb, 0, &rel, 2, 2).as_deref(),
            Some(&["20".to_string(), "30".to_string()][..]),
            "C3 看 A2:A3（和 Excel 的 Formula1 平移一致）"
        );
        let mut ev2 = |_: &str, _: u32, _: u32| crate::formula::FValue::Bool(true);
        assert!(validate_wb(&wb, 0, &rel, &Value::Number(30.0), 2, 2, &mut ev2));
        assert!(!validate_wb(&wb, 0, &rel, &Value::Number(10.0), 2, 2, &mut ev2));
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

#[cfg(test)]
mod c07_offset_tests {
    use super::*;
    use crate::model::{Cell, Sheet, Value, Workbook};

    fn blank() -> Workbook {
        Workbook::new_blank()
    }

    fn put(sheet: &mut Sheet, r: u32, c: u32, v: Value) {
        sheet.set(r, c, Cell { value: v, formula: None });
    }

    fn list(f1: &str) -> DataValidation {
        DataValidation { kind: DvKind::List, f1: f1.into(), ..Default::default() }
    }

    /// OFFSET 省略 height/width 时继承 reference 的行列数；负数 height 反向展开。
    /// Excel 16.0 实测：ROWS(OFFSET($A$1:$A$3,0,0))=3、
    /// ROWS(OFFSET($A$3,0,0,-2,1))=2（$A$2:$A$3）。
    #[test]
    fn offset_inherits_reference_size_and_allows_negative_span() {
        let mut wb = blank();
        for (i, t) in ["甲", "乙", "丙", "丁"].iter().enumerate() {
            put(&mut wb.sheets[0], i as u32, 0, Value::Text((*t).into()));
        }
        assert_eq!(
            list_items_wb(&wb, 0, &list("=OFFSET($A$1:$A$3,0,0)"), 0, 0).as_deref(),
            Some(&["甲".to_string(), "乙".to_string(), "丙".to_string()][..]),
            "省略 height 必须继承 reference 的 3 行"
        );
        assert_eq!(
            list_items_wb(&wb, 0, &list("=OFFSET($A$1:$A$3,1,0)"), 0, 0).as_deref(),
            Some(&["乙".to_string(), "丙".to_string(), "丁".to_string()][..]),
            "平移 1 行后仍是 3 行高"
        );
        assert_eq!(
            list_items_wb(&wb, 0, &list("=OFFSET($A$3,0,0,-2,1)"), 0, 0).as_deref(),
            Some(&["乙".to_string(), "丙".to_string()][..]),
            "height=-2 从 A3 向上取 A2:A3"
        );
        assert_eq!(
            list_items_wb(&wb, 0, &list("=OFFSET($A$1,0,0,2,2)"), 0, 0).as_deref(),
            Some(&["甲".to_string(), "乙".to_string()][..]),
            "单格 reference + 显式 2x2 只取 A 列两格非空"
        );
    }

    /// 跨表 reference 只决定取哪张表，其余参数仍按公式所在表求值。
    /// Excel 16.0 实测：主表 A 列 2 个值、数据表 5 个值时
    /// =ROWS(OFFSET(数据!$A$1,0,0,COUNTA($A:$A),1)) 得 2。
    #[test]
    fn offset_arguments_keep_the_rule_sheet_context() {
        let mut wb = blank();
        put(&mut wb.sheets[0], 0, 0, Value::Number(1.0));
        put(&mut wb.sheets[0], 1, 0, Value::Number(2.0));
        let mut data = Sheet::default();
        data.name = "数据".into();
        for (i, v) in [100.0, 200.0, 300.0, 400.0, 500.0].iter().enumerate() {
            put(&mut data, i as u32, 0, Value::Number(*v));
        }
        wb.sheets.push(data);

        let dv = list("=OFFSET(数据!$A$1,0,0,COUNTA($A:$A),1)");
        assert_eq!(
            list_items_wb(&wb, 0, &dv, 0, 0).as_deref(),
            Some(&["100".to_string(), "200".to_string()][..]),
            "高度取主表 COUNTA=2，不是数据表的 5；400/500 不能被接受"
        );
        // 明确带表名的 COUNTA 才用数据表
        let dv2 = list("=OFFSET(数据!$A$1,0,0,COUNTA(数据!$A:$A),1)");
        assert_eq!(
            list_items_wb(&wb, 0, &dv2, 0, 0).as_deref(),
            Some(&["100".to_string(), "200".to_string(), "300".to_string(), "400".to_string(), "500".to_string()][..])
        );
    }
}
