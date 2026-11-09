//! Excel 公式：词法、语法、求值
//!
//! 语法优先级（低 → 高），与 Excel 一致：
//!   比较(= <> < > <= >=) < 连接(&) < 加减 < 乘除 < 乘方 < 一元负号 < 百分号 < 区间(:)

pub mod array;
pub mod deps;
pub mod dist;
pub mod extra;
pub mod functions;
pub mod value;

pub use value::{ErrKind, FValue};

use crate::model::{Cell, CellRef, Sheet, Workbook};
use std::collections::{HashMap, HashSet};

// ---------------------------------------------------------------- AST

#[derive(Clone, Debug, PartialEq)]
pub struct RefExpr {
    pub sheet: Option<String>,
    pub row: i64, // 0-based
    pub col: i64,
    pub row_abs: bool,
    pub col_abs: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Pow,
    Concat,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    /// 省略的实参：`VLOOKUP(x, r, 3, )` 里的空位
    Empty,
    Name(String),
    Number(f64),
    Text(String),
    Bool(bool),
    Err(ErrKind),
    Ref(RefExpr),
    Range(RefExpr, RefExpr),
    Func(String, Vec<Node>),
    Unary(bool, Box<Node>), // true = 负号
    Percent(Box<Node>),
    Binary(BinOp, Box<Node>, Box<Node>),
    /// 数组常量 {1,2;3,4}
    Array(usize, usize, Vec<FValue>),
}

// ---------------------------------------------------------------- 词法

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Num(f64),
    Str(String),
    Ident(String),
    Op(&'static str),
    LParen,
    RParen,
    LBrace,
    RBrace,
    Comma,
    Semi,
    Colon,
    Bang,
    Percent,
}

fn lex(src: &str) -> Result<Vec<Tok>, ErrKind> {
    let b: Vec<char> = src.chars().collect();
    let mut i = 0usize;
    let mut out = Vec::new();
    while i < b.len() {
        let c = b[i];
        match c {
            ' ' | '\t' | '\n' | '\r' => i += 1,
            '(' => {
                out.push(Tok::LParen);
                i += 1;
            }
            ')' => {
                out.push(Tok::RParen);
                i += 1;
            }
            ',' => {
                out.push(Tok::Comma);
                i += 1;
            }
            '{' => {
                out.push(Tok::LBrace);
                i += 1;
            }
            '}' => {
                out.push(Tok::RBrace);
                i += 1;
            }
            ';' => {
                out.push(Tok::Semi);
                i += 1;
            }
            ':' => {
                out.push(Tok::Colon);
                i += 1;
            }
            '!' => {
                out.push(Tok::Bang);
                i += 1;
            }
            // 带引号的表名：'参数与字典'!A1，里面的 '' 是转义的引号
            '\'' => {
                let mut name = String::new();
                i += 1;
                loop {
                    if i >= b.len() {
                        return Err(ErrKind::Value);
                    }
                    if b[i] == '\'' {
                        if i + 1 < b.len() && b[i + 1] == '\'' {
                            name.push('\'');
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    name.push(b[i]);
                    i += 1;
                }
                out.push(Tok::Ident(name));
            }
            '%' => {
                out.push(Tok::Percent);
                i += 1;
            }
            '"' => {
                let mut s = String::new();
                i += 1;
                loop {
                    if i >= b.len() {
                        return Err(ErrKind::Value);
                    }
                    if b[i] == '"' {
                        if i + 1 < b.len() && b[i + 1] == '"' {
                            s.push('"');
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    s.push(b[i]);
                    i += 1;
                }
                out.push(Tok::Str(s));
            }
            '<' => {
                if i + 1 < b.len() && b[i + 1] == '=' {
                    out.push(Tok::Op("<="));
                    i += 2;
                } else if i + 1 < b.len() && b[i + 1] == '>' {
                    out.push(Tok::Op("<>"));
                    i += 2;
                } else {
                    out.push(Tok::Op("<"));
                    i += 1;
                }
            }
            '>' => {
                if i + 1 < b.len() && b[i + 1] == '=' {
                    out.push(Tok::Op(">="));
                    i += 2;
                } else {
                    out.push(Tok::Op(">"));
                    i += 1;
                }
            }
            '=' => {
                out.push(Tok::Op("="));
                i += 1;
            }
            '+' | '-' | '*' | '/' | '^' | '&' => {
                let s = match c {
                    '+' => "+",
                    '-' => "-",
                    '*' => "*",
                    '/' => "/",
                    '^' => "^",
                    _ => "&",
                };
                out.push(Tok::Op(s));
                i += 1;
            }
            '#' => {
                // 错误字面量 #N/A #REF! 等
                let start = i;
                i += 1;
                while i < b.len() && (b[i].is_alphanumeric() || b[i] == '/' || b[i] == '!' || b[i] == '_') {
                    i += 1;
                }
                let lit: String = b[start..i].iter().collect();
                let e = match lit.to_ascii_uppercase().as_str() {
                    "#NULL!" => ErrKind::Null,
                    "#DIV/0!" => ErrKind::Div0,
                    "#VALUE!" => ErrKind::Value,
                    "#REF!" => ErrKind::Ref,
                    "#NAME?" => ErrKind::Name,
                    "#NUM!" => ErrKind::Num,
                    "#N/A" => ErrKind::Na,
                    _ => return Err(ErrKind::Name),
                };
                out.push(Tok::Ident(format!("\u{0}ERR{}=", e as u8))); // 用特殊前缀表示错误字面量
            }
            _ => {
                if c.is_ascii_digit() || (c == '.' && i + 1 < b.len() && b[i + 1].is_ascii_digit()) {
                    let start = i;
                    while i < b.len() && (b[i].is_ascii_digit() || b[i] == '.') {
                        i += 1;
                    }
                    // 科学计数法
                    if i < b.len() && (b[i] == 'e' || b[i] == 'E') {
                        let save = i;
                        i += 1;
                        if i < b.len() && (b[i] == '+' || b[i] == '-') {
                            i += 1;
                        }
                        if i < b.len() && b[i].is_ascii_digit() {
                            while i < b.len() && b[i].is_ascii_digit() {
                                i += 1;
                            }
                        } else {
                            i = save;
                        }
                    }
                    let s: String = b[start..i].iter().collect();
                    out.push(Tok::Num(s.parse().map_err(|_| ErrKind::Value)?));
                } else if c.is_alphabetic() || c == '_' || c == '$' || c == '\\' {
                    let start = i;
                    while i < b.len()
                        && (b[i].is_alphanumeric()
                            || b[i] == '_'
                            || b[i] == '.'
                            || b[i] == '$'
                            || b[i] == '\\')
                    {
                        i += 1;
                    }
                    out.push(Tok::Ident(b[start..i].iter().collect()));
                } else {
                    return Err(ErrKind::Value);
                }
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------- 语法

struct Parser {
    toks: Vec<Tok>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }
    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        self.pos += 1;
        t
    }
    /// 空格在 Excel 里是交集运算符：A1:A2 B1:B2。
    /// 我们的求值器不做区域相交，直接按 Excel 的"无重叠"结果返回 #NULL!。
    fn eats_intersection(&mut self) -> bool {
        if let Some(Tok::Ident(name)) = self.peek() {
            if parse_a1(name).is_some() {
                self.pos += 1;
                if self.peek() == Some(&Tok::Colon) {
                    self.pos += 1;
                    if matches!(self.peek(), Some(Tok::Ident(_))) {
                        self.pos += 1;
                    }
                }
                return true;
            }
        }
        false
    }

    fn eat(&mut self, t: &Tok) -> bool {
        if self.peek() == Some(t) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn parse_expr(&mut self) -> Result<Node, ErrKind> {
        self.parse_cmp()
    }

    fn parse_cmp(&mut self) -> Result<Node, ErrKind> {
        let mut left = self.parse_concat()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Op("=")) => BinOp::Eq,
                Some(Tok::Op("<>")) => BinOp::Ne,
                Some(Tok::Op("<")) => BinOp::Lt,
                Some(Tok::Op("<=")) => BinOp::Le,
                Some(Tok::Op(">")) => BinOp::Gt,
                Some(Tok::Op(">=")) => BinOp::Ge,
                _ => break,
            };
            self.pos += 1;
            let right = self.parse_concat()?;
            left = Node::Binary(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_concat(&mut self) -> Result<Node, ErrKind> {
        let mut left = self.parse_add()?;
        while matches!(self.peek(), Some(Tok::Op("&"))) {
            self.pos += 1;
            let right = self.parse_add()?;
            left = Node::Binary(BinOp::Concat, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_add(&mut self) -> Result<Node, ErrKind> {
        let mut left = self.parse_mul()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Op("+")) => BinOp::Add,
                Some(Tok::Op("-")) => BinOp::Sub,
                _ => break,
            };
            self.pos += 1;
            let right = self.parse_mul()?;
            left = Node::Binary(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_mul(&mut self) -> Result<Node, ErrKind> {
        let mut left = self.parse_pow()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Op("*")) => BinOp::Mul,
                Some(Tok::Op("/")) => BinOp::Div,
                _ => break,
            };
            self.pos += 1;
            let right = self.parse_pow()?;
            left = Node::Binary(op, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_pow(&mut self) -> Result<Node, ErrKind> {
        let mut left = self.parse_unary()?;
        while matches!(self.peek(), Some(Tok::Op("^"))) {
            self.pos += 1;
            let right = self.parse_unary()?;
            left = Node::Binary(BinOp::Pow, Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    fn parse_unary(&mut self) -> Result<Node, ErrKind> {
        if matches!(self.peek(), Some(Tok::Op("-"))) {
            self.pos += 1;
            let e = self.parse_unary()?;
            return Ok(Node::Unary(true, Box::new(e)));
        }
        if matches!(self.peek(), Some(Tok::Op("+"))) {
            self.pos += 1;
            return self.parse_unary();
        }
        let mut node = self.parse_primary()?;
        while matches!(self.peek(), Some(Tok::Percent)) {
            self.pos += 1;
            node = Node::Percent(Box::new(node));
        }
        Ok(node)
    }

    fn parse_primary(&mut self) -> Result<Node, ErrKind> {
        match self.next() {
            Some(Tok::Num(n)) => Ok(Node::Number(n)),
            Some(Tok::Str(s)) => Ok(Node::Text(s)),
            Some(Tok::LParen) => {
                let e = self.parse_expr()?;
                if !self.eat(&Tok::RParen) {
                    return Err(ErrKind::Value);
                }
                Ok(e)
            }
            Some(Tok::LBrace) => {
                let mut rows: Vec<Vec<FValue>> = vec![Vec::new()];
                loop {
                    let v = match self.next() {
                        Some(Tok::Num(n)) => FValue::number(n),
                        Some(Tok::Str(s)) => FValue::Text(s),
                        Some(Tok::Op("-")) => match self.next() {
                            Some(Tok::Num(n)) => FValue::number(-n),
                            _ => return Err(ErrKind::Value),
                        },
                        Some(Tok::Ident(id)) if id.eq_ignore_ascii_case("TRUE") => FValue::Bool(true),
                        Some(Tok::Ident(id)) if id.eq_ignore_ascii_case("FALSE") => FValue::Bool(false),
                        _ => return Err(ErrKind::Value),
                    };
                    rows.last_mut().unwrap().push(v);
                    match self.next() {
                        Some(Tok::Comma) => continue,
                        Some(Tok::Semi) => {
                            rows.push(Vec::new());
                            continue;
                        }
                        Some(Tok::RBrace) => break,
                        _ => return Err(ErrKind::Value),
                    }
                }
                let cols = rows.iter().map(|r| r.len()).max().unwrap_or(0);
                let nrows = rows.len();
                let mut data = Vec::with_capacity(nrows * cols);
                for r in &rows {
                    for c in 0..cols {
                        data.push(r.get(c).cloned().unwrap_or(FValue::Empty));
                    }
                }
                Ok(Node::Array(nrows, cols, data))
            }
            Some(Tok::Ident(name)) => {
                // 错误字面量
                if let Some(rest) = name.strip_prefix('\u{0}') {
                    if let Some(code) = rest.strip_prefix("ERR") {
                        let code = code.trim_end_matches('=').parse::<u8>().map_err(|_| ErrKind::Name)?;
                        return Ok(Node::Err(match code {
                            0 => ErrKind::Null,
                            1 => ErrKind::Div0,
                            2 => ErrKind::Value,
                            3 => ErrKind::Ref,
                            4 => ErrKind::Name,
                            5 => ErrKind::Num,
                            _ => ErrKind::Na,
                        }));
                    }
                }
                // 函数调用
                if matches!(self.peek(), Some(Tok::LParen)) {
                    self.pos += 1;
                    let mut args = Vec::new();
                    if !self.eat(&Tok::RParen) {
                        loop {
                            // 允许省略参数（如 IF(A1,,1)）
                            if matches!(self.peek(), Some(Tok::Comma)) {
                                args.push(Node::Empty);
                            } else if matches!(self.peek(), Some(Tok::RParen)) {
                                // 末尾省略：VLOOKUP(x, r, 3, )
                                args.push(Node::Empty);
                                self.pos += 1;
                                break;
                            } else {
                                args.push(self.parse_expr()?);
                            }
                            if self.eat(&Tok::Comma) {
                                continue;
                            }
                            if self.eat(&Tok::RParen) {
                                break;
                            }
                            return Err(ErrKind::Value);
                        }
                    }
                    let mut node = Node::Func(name.to_ascii_uppercase(), args);
                    // LAMBDA(x,x*x)(7)：函数值后面直接跟实参
                    while matches!(self.peek(), Some(Tok::LParen)) {
                        self.pos += 1;
                        let mut call = vec![node];
                        if !self.eat(&Tok::RParen) {
                            loop {
                                call.push(self.parse_expr()?);
                                if self.eat(&Tok::Comma) {
                                    continue;
                                }
                                if self.eat(&Tok::RParen) {
                                    break;
                                }
                                return Err(ErrKind::Value);
                            }
                        }
                        node = Node::Func("__CALL__".to_string(), call);
                    }
                    return Ok(node);
                }
                // TRUE / FALSE
                let up = name.to_ascii_uppercase();
                if up == "TRUE" {
                    return Ok(Node::Bool(true));
                }
                if up == "FALSE" {
                    return Ok(Node::Bool(false));
                }
                // 区域引用
                self.parse_ref_from(name)
            }
            _ => Err(ErrKind::Value),
        }
    }

    /// name 可能是 "A1"、"$A$1"、"Sheet1"（后面跟 !）
    fn parse_ref_from(&mut self, name: String) -> Result<Node, ErrKind> {
        if matches!(self.peek(), Some(Tok::Bang)) {
            self.pos += 1;
            let Some(Tok::Ident(cell)) = self.next() else {
                return Err(ErrKind::Ref);
            };
            let start = parse_a1(&cell).ok_or(ErrKind::Ref)?;
            let mut re = start;
            re.sheet = Some(name);
            if self.eat(&Tok::Colon) {
                let Some(Tok::Ident(cell2)) = self.next() else {
                    return Err(ErrKind::Ref);
                };
                let mut end = parse_a1(&cell2).ok_or(ErrKind::Ref)?;
                end.sheet = re.sheet.clone();
                return Ok(Node::Range(re, end));
            }
            if self.eats_intersection() {
                return Ok(Node::Err(ErrKind::Null));
            }
            return Ok(Node::Ref(re));
        }

        let start = match parse_a1(&name) {
            Some(r) => r,
            // 不是 A1 形式的标识符 → 当作名字（LET 绑定或未定义名称）
            None => return Ok(Node::Name(name.to_ascii_uppercase())),
        };
        if self.eat(&Tok::Colon) {
            let Some(Tok::Ident(cell2)) = self.next() else {
                return Err(ErrKind::Ref);
            };
            let end = parse_a1(&cell2).ok_or(ErrKind::Ref)?;
            if self.eats_intersection() {
                return Ok(Node::Err(ErrKind::Null));
            }
            return Ok(Node::Range(start, end));
        }
        if self.eats_intersection() {
            return Ok(Node::Err(ErrKind::Null));
        }
        Ok(Node::Ref(start))
    }
}

/// "A1" / "$B$12" → RefExpr
pub fn parse_a1(s: &str) -> Option<RefExpr> {
    let chars: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut col_abs = false;
    if i < chars.len() && chars[i] == '$' {
        col_abs = true;
        i += 1;
    }
    let start = i;
    while i < chars.len() && chars[i].is_ascii_alphabetic() {
        i += 1;
    }
    if i == start {
        return None;
    }
    let col_str: String = chars[start..i].iter().collect();
    let mut col: i64 = 0;
    for ch in col_str.to_ascii_uppercase().chars() {
        col = col * 26 + (ch as i64 - 'A' as i64 + 1);
    }
    col -= 1;

    let mut row_abs = false;
    if i < chars.len() && chars[i] == '$' {
        row_abs = true;
        i += 1;
    }
    let rstart = i;
    while i < chars.len() && chars[i].is_ascii_digit() {
        i += 1;
    }
    if i == rstart || i != chars.len() {
        return None;
    }
    let row: i64 = chars[rstart..i].iter().collect::<String>().parse::<i64>().ok()? - 1;
    if row < 0 {
        return None;
    }
    Some(RefExpr { sheet: None, row, col, row_abs, col_abs })
}

// ---------------------------------------------------------------- 求值

thread_local! {
    /// 本线程这一轮重算里遇到过循环引用的格子
    static CYCLE_CELLS: std::cell::RefCell<HashSet<(usize, u32, u32)>> =
        std::cell::RefCell::new(HashSet::new());
}

/// 取走并清空本轮记录的循环引用格子
pub fn take_cycle_cells() -> HashSet<(usize, u32, u32)> {
    CYCLE_CELLS.with(|c| std::mem::take(&mut *c.borrow_mut()))
}

pub struct Calc<'a> {
    wb: &'a Workbook,
    memo: HashMap<(usize, u32, u32), FValue>,
    visiting: HashSet<(usize, u32, u32)>,
    /// LET 绑定的名字（大写）
    env_stack: Vec<HashMap<String, FValue>>,
    /// 正在求值的单元格（供 ROW()/COLUMN() 用）
    cur: (u32, u32),
    /// 区域内容缓存：同一轮重算里同一块区域只物化一次
    range_cache: HashMap<(usize, i64, i64, i64, i64), std::rc::Rc<Vec<FValue>>>,
    /// 每张表的实际使用范围（最大行、最大列）
    bounds: HashMap<usize, (u32, u32)>,
    /// 递归深度：整列引用会把循环引用放大成深递归，超过就返回错误而不是爆栈
    depth: usize,
    /// 精确匹配用的哈希索引：key → 首次出现的行下标
    lookup_index: HashMap<(usize, i64, i64, i64, i64), std::rc::Rc<HashMap<String, usize>>>,
    /// 增量重算的范围：不在其中的公式单元格一律用缓存值
    scope: Option<std::collections::HashSet<self::deps::CellKey>>,
    /// 区域聚合结果缓存：(sheet, r1, c1, r2, c2, 函数, 条件)
    agg_cache: HashMap<(usize, i64, i64, i64, i64, u8, String), FValue>,
    /// 区域缓存已占用的值数量，防止把逐行累积的区间全缓存下来
    cache_vals: usize,
    cache_order: std::collections::VecDeque<((usize, i64, i64, i64, i64), usize)>,
    pub agg_hits: u64,
    pub agg_miss: u64,
}

#[derive(Clone, Debug)]
pub enum Arg {
    Val(FValue),
    Range { values: std::rc::Rc<Vec<FValue>>, rows: usize, cols: usize },
}

impl Arg {
    /// 借用形式：聚合函数用这个，避免每次调用都复制整块区域
    pub fn slice(&self) -> &[FValue] {
        match self {
            Arg::Range { values, .. } => values.as_slice(),
            Arg::Val(FValue::Array(a)) => a.data.as_slice(),
            Arg::Val(v) => std::slice::from_ref(v),
        }
    }

    pub fn values(&self) -> Vec<FValue> {
        match self {
            Arg::Val(FValue::Array(a)) => a.data.clone(),
            Arg::Val(v) => vec![v.clone()],
            Arg::Range { values, .. } => (**values).clone(),
        }
    }
    pub fn first(&self) -> FValue {
        match self {
            Arg::Val(v) => v.scalar(),
            Arg::Range { values, .. } => values.first().cloned().unwrap_or(FValue::Empty),
        }
    }
    /// 统一成 (行, 列, 数据) 网格，供数组函数使用
    pub fn grid(&self) -> (usize, usize, Vec<FValue>) {
        match self {
            Arg::Val(FValue::Array(a)) => (a.rows, a.cols, a.data.clone()),
            Arg::Val(v) => (1, 1, vec![v.scalar()]),
            Arg::Range { values, rows, cols } => (*rows, *cols, (**values).clone()),
        }
    }
    /// 数字序列（区域内忽略空/文本/布尔，与 SUM 一致）
    pub fn numbers(&self) -> Result<Vec<f64>, ErrKind> {
        let mut out = Vec::new();
        match self {
            Arg::Val(FValue::Array(a)) => {
                for v in &a.data {
                    match v {
                        FValue::Number(n) => out.push(*n),
                        FValue::Error(e) => return Err(*e),
                        _ => {}
                    }
                }
            }
            Arg::Val(v) => {
                if v.is_error() {
                    return Err(match v {
                        FValue::Error(e) => *e,
                        _ => ErrKind::Value,
                    });
                }
                match v {
                    FValue::Number(n) => out.push(*n),
                    FValue::Bool(b) => out.push(if *b { 1.0 } else { 0.0 }),
                    FValue::Text(_) => out.push(v.to_number()?),
                    _ => {}
                }
            }
            Arg::Range { values, .. } => {
                for v in values.iter() {
                    match v {
                        FValue::Number(n) => out.push(*n),
                        FValue::Error(e) => return Err(*e),
                        _ => {}
                    }
                }
            }
        }
        Ok(out)
    }
}

impl<'a> Calc<'a> {
    pub fn new(wb: &'a Workbook) -> Self {
        Self { wb, memo: HashMap::new(), visiting: HashSet::new(), env_stack: vec![HashMap::new()], cur: (0, 0), range_cache: HashMap::new(), bounds: HashMap::new(), lookup_index: HashMap::new(), scope: None,
            agg_cache: HashMap::new(), cache_vals: 0, depth: 0,
            cache_order: std::collections::VecDeque::new(), agg_hits: 0, agg_miss: 0 }
    }

    fn sheet_index(&self, name: Option<&str>, cur: usize) -> Result<usize, ErrKind> {
        match name {
            None => Ok(cur),
            Some(n) => self
                .wb
                .sheets
                .iter()
                .position(|s| s.name.eq_ignore_ascii_case(n))
                .ok_or(ErrKind::Ref),
        }
    }

    pub fn cell_value(&mut self, si: usize, r: u32, c: u32) -> FValue {
        if self.depth >= 200 {
            CYCLE_CELLS.with(|cells| {
                cells.borrow_mut().insert((si, r, c));
            });
            return FValue::Error(ErrKind::Ref);
        }
        self.depth += 1;
        let out = self.cell_value_inner(si, r, c);
        self.depth -= 1;
        out
    }

    fn cell_value_inner(&mut self, si: usize, r: u32, c: u32) -> FValue {
        let key = (si, r, c);
        if let Some(v) = self.memo.get(&key) {
            return v.clone();
        }
        let cell = match self.wb.sheets.get(si).and_then(|s| s.get(r, c)) {
            Some(c) if !c.is_blank() => c.clone(),
            _ => return FValue::Empty,
        };
        if let Some(f) = cell.formula.as_deref() {
            // 增量重算：不在本次范围内的公式，直接用文件里缓存的结果
            if let Some(scope) = &self.scope {
                if !scope.contains(&key) {
                    return to_fvalue(&cell.value);
                }
            }
            self.cur = (r, c);
            let raw = f.strip_prefix('=').unwrap_or(f).to_string();
            // 结构化引用：Table1[列] → A1 区域
            let expanded = crate::table::expand_structured(self.wb, si, &raw).unwrap_or(raw);
            let normalized = crate::normalize_formula(&expanded);
            let body = normalized.as_str();
            if !self.visiting.insert(key) {
                CYCLE_CELLS.with(|cells| {
                    cells.borrow_mut().insert(key);
                });
                return FValue::Error(ErrKind::Circular);
            }
            let v = match parse(body) {
                Ok(node) => self.eval_node(&node, si),
                Err(e) => FValue::Error(e),
            };
            self.visiting.remove(&key);
            self.memo.insert(key, v.clone());
            v
        } else {
            to_fvalue(&cell.value)
        }
    }

    fn range_values(&mut self, a: &RefExpr, b: &RefExpr, cur: usize) -> Result<Arg, ErrKind> {
        let si = self.sheet_index(a.sheet.as_deref(), cur)?;
        let (mut r1, mut c1) = (a.row.min(b.row).max(0) as u32, a.col.min(b.col).max(0) as u32);
        let (mut r2, mut c2) = (a.row.max(b.row).max(0) as u32, a.col.max(b.col).max(0) as u32);
        // 整列/整行引用按表的实际使用范围裁剪，避免展开 104 万空单元格
        let (max_r, max_c) = self.sheet_bounds(si);
        let huge = (r2 as u64 - r1 as u64 + 1) as u64 * (c2 as u64 - c1 as u64 + 1) as u64 > 200_000;
        if huge {
            r2 = r2.min(max_r);
            c2 = c2.min(max_c);
            if r1 > r2 || c1 > c2 {
                return Ok(Arg::Range {
                    values: std::rc::Rc::new(Vec::new()),
                    rows: 0,
                    cols: 0,
                });
            }
        }
        let rows = (r2 - r1 + 1) as usize;
        let cols = (c2 - c1 + 1) as usize;
        // 同一轮重算里同一块区域只展开一次
        let key = (si, r1 as i64, c1 as i64, r2 as i64, c2 as i64);
        let values = match self.range_cache.get(&key) {
            Some(v) => v.clone(),
            None => {
                let mut vals = Vec::with_capacity(rows * cols);
                for r in r1..=r2 {
                    for c in c1..=c2 {
                        vals.push(self.cell_value(si, r, c));
                    }
                }
                self.cache_put(key, std::rc::Rc::new(vals))
            }
        };
        Ok(Arg::Range { values, rows, cols })
    }

    pub fn eval_node(&mut self, node: &Node, si: usize) -> FValue {
        match node {
            Node::Empty => FValue::Empty,
            Node::Name(n) => {
                if let Some(v) = self.env_stack.iter().rev().find_map(|m| m.get(n).cloned()) {
                    return v;
                }
                // 工作簿定义名称：底层存的是定义式（区域、常量或公式）
                let hit = {
                    let mut found = None;
                    for (nm, sh, body) in self.wb.names.iter() {
                        if nm.eq_ignore_ascii_case(n) {
                            found = Some((*sh, body.clone()));
                            break;
                        }
                    }
                    found
                };
                if let Some((sh, body)) = hit {
                    let si2 = sh.unwrap_or(si);
                    let text = body.trim_start_matches('=').to_string();
                    if !text.is_empty() && !text.eq_ignore_ascii_case(n) {
                        let expanded =
                            crate::table::expand_structured(self.wb, si2, &text).unwrap_or(text);
                        if let Ok(node) = parse(&expanded) {
                            return self.eval_node(&node, si2);
                        }
                    }
                }
                FValue::Error(ErrKind::Name)
            }
            Node::Number(n) => FValue::Number(*n),
            Node::Text(s) => FValue::Text(s.as_str().into()),
            Node::Bool(b) => FValue::Bool(*b),
            Node::Err(e) => FValue::Error(*e),
            Node::Ref(r) => match self.sheet_index(r.sheet.as_deref(), si) {
                Ok(idx) if r.row >= 0 && r.col >= 0 => {
                    self.cell_value(idx, r.row as u32, r.col as u32)
                }
                _ => FValue::Error(ErrKind::Ref),
            },
            Node::Range(a, b) => match self.range_values(a, b, si) {
                Ok(Arg::Range { values, .. }) => values.first().cloned().unwrap_or(FValue::Empty),
                Ok(_) => FValue::Error(ErrKind::Value),
                Err(e) => FValue::Error(e),
            },
            Node::Unary(neg, e) => {
                let v = self.eval_node(e, si);
                match v.to_number() {
                    Ok(n) => FValue::number(if *neg { -n } else { n }),
                    Err(err) => FValue::Error(err),
                }
            }
            Node::Percent(e) => {
                let v = self.eval_node(e, si);
                match v.to_number() {
                    Ok(n) => FValue::number(n / 100.0),
                    Err(err) => FValue::Error(err),
                }
            }
            Node::Binary(op, l, r) => self.eval_binary(*op, l, r, si),
            Node::Array(rows, cols, data) => {
                FValue::Array(std::rc::Rc::new(value::Array::new(*rows, *cols, data.clone())))
            }
            // IF 系列必须短路求值：未被选中的分支不能计算（否则 IF(A1=0,1,1/A1) 会误报 #DIV/0!）
            Node::Func(name, nodes) if name == "IF" && nodes.len() >= 2 => {
                let cond = self.eval_node(&nodes[0], si);
                match cond.to_bool() {
                    Ok(true) => self.eval_node(&nodes[1], si),
                    Ok(false) => {
                        if nodes.len() >= 3 {
                            self.eval_node(&nodes[2], si)
                        } else {
                            FValue::Bool(false)
                        }
                    }
                    Err(e) => FValue::Error(e),
                }
            }
            Node::Func(name, nodes) if name == "IFERROR" && nodes.len() >= 2 => {
                let v = self.eval_node(&nodes[0], si);
                if v.is_error() {
                    self.eval_node(&nodes[1], si)
                } else {
                    v
                }
            }
            // LET(x, 1, y, x+1, y*2)：在局部环境里绑定名字
            Node::Func(name, nodes) if name == "LET" && nodes.len() >= 3 => {
                self.env_stack.push(HashMap::new());
                let mut i = 0;
                while i + 2 < nodes.len() {
                    if let Node::Name(nm) = &nodes[i] {
                        // LET(x, A6:A11, SUM(x*x)) 里的 x 是整块区域，
                        // 只取左上角会让 SUM(x*x) 得到一个数
                        let v = match &nodes[i + 1] {
                            Node::Range(a, b) => match self.range_values(a, b, si) {
                                Ok(Arg::Range { values, rows, cols }) => FValue::Array(
                                    std::rc::Rc::new(value::Array::new(rows, cols, (*values).clone())),
                                ),
                                Ok(Arg::Val(v)) => v,
                                Err(e) => FValue::Error(e),
                            },
                            other => self.eval_node(other, si),
                        };
                        self.env_stack.last_mut().unwrap().insert(nm.clone(), v);
                    }
                    i += 2;
                }
                let out = self.eval_node(nodes.last().unwrap(), si);
                self.env_stack.pop();
                out
            }
            // ISFORMULA / FORMULATEXT / SHEET / SHEETS
            Node::Func(name, nodes) if name == "ISFORMULA" || name == "FORMULATEXT" => {
                let re = match nodes.first() {
                    Some(Node::Ref(re)) => re.clone(),
                    _ => return FValue::Error(ErrKind::Value),
                };
                match self.sheet_index(re.sheet.as_deref(), si) {
                    Ok(idx) => {
                        let formula = self.wb.sheets[idx]
                            .get(re.row.max(0) as u32, re.col.max(0) as u32)
                            .and_then(|c| c.formula.clone());
                        match formula.as_deref() {
                            Some(f) if name == "FORMULATEXT" => FValue::Text(format!("={}", f.trim_start_matches("="))),
                            Some(_) => FValue::Bool(true),
                            None if name == "FORMULATEXT" => FValue::Error(ErrKind::Na),
                            None => FValue::Bool(false),
                        }
                    }
                    Err(e) => FValue::Error(e),
                }
            }
            Node::Func(name, nodes) if name == "SHEET" => {
                let idx = match nodes.first() {
                    Some(Node::Ref(re)) => self.sheet_index(re.sheet.as_deref(), si).unwrap_or(si),
                    _ => si,
                };
                FValue::number((idx + 1) as f64)
            }
            Node::Func(name, nodes) if name == "SHEETS" && nodes.is_empty() => {
                FValue::number(self.wb.sheets.len() as f64)
            }
            // ============ LAMBDA 家族（需要把函数当值传递，走特殊形式）
            Node::Func(name, nodes)
                if matches!(name.as_str(), "MAP" | "REDUCE" | "SCAN" | "BYROW" | "BYCOL" | "MAKEARRAY")
                    && nodes.len() >= 2 =>
            {
                let lam = nodes.last().unwrap();
                match name.as_str() {
                    "MAP" => {
                        let vals = self.operand_values(&nodes[0], si);
                        let lam_val = match self.eval_node(lam, si) {
                            FValue::Lambda(l) => Some(l),
                            _ => None,
                        };
                        let out: Vec<FValue> = vals
                            .iter()
                            .map(|v| match &lam_val {
                                Some(l) => self.apply_lambda_val(l, vec![v.clone()], si),
                                None => self.apply_lambda(lam, vec![v.clone()], si),
                            })
                            .collect();
                        FValue::Array(std::rc::Rc::new(value::Array::new(1, out.len(), out)))
                    }
                    "REDUCE" => {
                        let mut acc = self.eval_node(&nodes[0], si);
                        let vals = match nodes.get(1) {
                            Some(n) => self.operand_values(n, si),
                            None => vec![],
                        };
                        for v in vals {
                            acc = self.apply_lambda(lam, vec![acc, v], si);
                        }
                        acc
                    }
                    "SCAN" => {
                        let init = self.eval_node(&nodes[0], si);
                        let vals = match nodes.get(1) {
                            Some(n) => self.operand_values(n, si),
                            None => vec![],
                        };
                        let mut acc = init;
                        let mut out = Vec::with_capacity(vals.len());
                        for v in vals {
                            acc = self.apply_lambda(lam, vec![acc, v], si);
                            out.push(acc.clone());
                        }
                        FValue::Array(std::rc::Rc::new(value::Array::new(1, out.len(), out)))
                    }
                    "MAKEARRAY" => {
                        let rows = self.eval_node(&nodes[0], si).to_number().unwrap_or(0.0) as usize;
                        let cols = self.eval_node(&nodes[1], si).to_number().unwrap_or(0.0) as usize;
                        if rows == 0 || cols == 0 || rows * cols > 1_000_000 {
                            return FValue::Error(ErrKind::Value);
                        }
                        let mut data = Vec::with_capacity(rows * cols);
                        for r in 0..rows {
                            for c in 0..cols {
                                data.push(self.apply_lambda(
                                    lam,
                                    vec![FValue::number((r + 1) as f64), FValue::number((c + 1) as f64)],
                                    si,
                                ));
                            }
                        }
                        FValue::Array(std::rc::Rc::new(value::Array::new(rows, cols, data)))
                    }
                    "BYROW" => {
                        let (rows, cols, data) = match nodes.first() {
                            Some(n) => self.grid_of(n, si),
                            None => (0, 0, vec![]),
                        };
                        let mut out = Vec::with_capacity(rows);
                        for r in 0..rows {
                            let row: Vec<FValue> = (0..cols).map(|c| data[r * cols + c].clone()).collect();
                            let arg = FValue::Array(std::rc::Rc::new(value::Array::new(1, cols, row)));
                            out.push(self.apply_lambda(lam, vec![arg], si));
                        }
                        FValue::Array(std::rc::Rc::new(value::Array::new(rows.max(1), 1, out)))
                    }
                    _ => {
                        // BYCOL
                        let (rows, cols, data) = match nodes.first() {
                            Some(n) => self.grid_of(n, si),
                            None => (0, 0, vec![]),
                        };
                        let mut out = Vec::with_capacity(cols);
                        for c in 0..cols {
                            let col: Vec<FValue> = (0..rows).map(|r| data[r * cols + c].clone()).collect();
                            let arg = FValue::Array(std::rc::Rc::new(value::Array::new(rows, 1, col)));
                            out.push(self.apply_lambda(lam, vec![arg], si));
                        }
                        FValue::Array(std::rc::Rc::new(value::Array::new(1, cols.max(1), out)))
                    }
                }
            }
            // LAMBDA 作为值出现（LET 绑定、或者传参）：包成一等公民
            Node::Func(name, parts) if name == "LAMBDA" && !parts.is_empty() => {
                let params = parts[..parts.len() - 1]
                    .iter()
                    .filter_map(|p| match p {
                        Node::Name(n) => Some(n.clone()),
                        _ => None,
                    })
                    .collect();
                let body = parts[parts.len() - 1].clone();
                FValue::Lambda(std::rc::Rc::new(crate::formula::value::LambdaVal {
                    params,
                    body,
                    env: self.env_stack.clone(),
                }))
            }
            // 立即调用：LAMBDA(x, x*x)(7)
            Node::Func(name, nodes) if name == "__CALL__" && !nodes.is_empty() => {
                let target = self.eval_node(&nodes[0], si);
                match target {
                    FValue::Lambda(l) => {
                        let params = l.params.clone();
                        let body = l.body.clone();
                        let captured = l.env.clone();
                        let mut bound: HashMap<String, FValue> = HashMap::new();
                        for (i, p) in params.iter().enumerate() {
                            let v = match nodes.get(i + 1) {
                                Some(n) => self.eval_node(n, si),
                                None => FValue::Empty,
                            };
                            bound.insert(p.clone(), v);
                        }
                        let saved = std::mem::replace(&mut self.env_stack, captured);
                        self.env_stack.push(bound);
                        let out = self.eval_node(&body, si);
                        self.env_stack = saved;
                        out
                    }
                    _ => FValue::Error(ErrKind::Value),
                }
            }
            Node::Func(name, nodes) if name == "ISOMITTED" => match nodes.first() {
                Some(Node::Name(n)) => FValue::Bool(
                    self.env_stack
                        .iter()
                        .rev()
                        .find_map(|m| m.get(n))
                        .map(|v| matches!(v, FValue::Empty))
                        .unwrap_or(false),
                ),
                _ => FValue::Bool(false),
            },
            // CELL / AREAS / INFO
            Node::Func(name, nodes) if name == "CELL" => {
                let info = match nodes.first() {
                    Some(n) => self.eval_node(n, si).to_text().unwrap_or_default().to_lowercase(),
                    None => return FValue::Error(ErrKind::Value),
                };
                let (r, c, sheet) = match nodes.get(1) {
                    Some(Node::Ref(re)) => (re.row.max(0) as u32, re.col.max(0) as u32, re.sheet.clone()),
                    Some(Node::Range(a, _)) => (a.row.max(0) as u32, a.col.max(0) as u32, a.sheet.clone()),
                    _ => (self.cur.0, self.cur.1, None),
                };
                let idx = match self.sheet_index(sheet.as_deref(), si) {
                    Ok(i) => i,
                    Err(e) => return FValue::Error(e),
                };
                match info.as_str() {
                    "address" => FValue::Text(format!("${}${}", crate::model::col_label(c), r + 1).into()),
                    "row" => FValue::number((r + 1) as f64),
                    "col" => FValue::number((c + 1) as f64),
                    "contents" => self.cell_value(idx, r, c),
                    "type" => match self.cell_value(idx, r, c) {
                        FValue::Empty => FValue::Text("b".into()),
                        FValue::Text(_) => FValue::Text("l".into()),
                        _ => FValue::Text("v".into()),
                    },
                    "width" => FValue::number(8.0),
                    "prefix" | "format" => FValue::Text(String::new()),
                    "protect" => FValue::number(0.0),
                    "parentheses" => FValue::number(0.0),
                    "filename" | "color" => FValue::Text(String::new()),
                    _ => FValue::Error(ErrKind::Value),
                }
            }
            Node::Func(name, nodes) if name == "AREAS" => match nodes.first() {
                Some(Node::Range(..)) | Some(Node::Ref(_)) => FValue::number(1.0),
                _ => FValue::Error(ErrKind::Value),
            },
            Node::Func(name, nodes) if name == "INFO" => {
                let kind = match nodes.first() {
                    Some(n) => self.eval_node(n, si).to_text().unwrap_or_default().to_lowercase(),
                    None => return FValue::Error(ErrKind::Value),
                };
                match kind.as_str() {
                    "numfile" => FValue::number(self.wb.sheets.len() as f64),
                    "release" => FValue::Text("16.0".into()),
                    "system" => FValue::Text("pcdos".into()),
                    "osversion" => FValue::Text("Windows (32-bit) NT 10.00".into()),
                    "recalc" => FValue::Text("Automatic".into()),
                    "directory" | "memavail" | "memused" | "totmem" => FValue::Text(String::new()),
                    _ => FValue::Error(ErrKind::Value),
                }
            }
            // ROW()/COLUMN()：无参返回当前单元格，带引用返回引用起点
            Node::Func(name, nodes) if name == "ROW" || name == "COLUMN" => {
                let (r, c) = match nodes.first() {
                    Some(Node::Ref(re)) => (re.row.max(0) as u32, re.col.max(0) as u32),
                    Some(Node::Range(a, _)) => (a.row.max(0) as u32, a.col.max(0) as u32),
                    _ => self.cur,
                };
                if name == "ROW" {
                    FValue::number((r + 1) as f64)
                } else {
                    FValue::number((c + 1) as f64)
                }
            }
            // INDIRECT("A1") / INDIRECT("Sheet1!A1:B2")
            Node::Func(name, nodes) if name == "INDIRECT" => {
                let txt = match nodes.first() {
                    Some(n) => self.eval_node(n, si).to_text().unwrap_or_default(),
                    None => String::new(),
                };
                let (sheet, rest) = match txt.split_once('!') {
                    Some((sh, r)) => (Some(sh.trim_start_matches("'").trim_end_matches("'").to_string()), r.to_string()),
                    None => (None, txt.clone()),
                };
                if let Some((a, b)) = rest.split_once(':') {
                    match (parse_a1(a.trim()), parse_a1(b.trim())) {
                        (Some(mut ra), Some(mut rb)) => {
                            ra.sheet = sheet.clone();
                            rb.sheet = sheet.clone();
                            match self.range_values(&ra, &rb, si) {
                                Ok(Arg::Range { values, rows, cols }) => FValue::Array(
                                    std::rc::Rc::new(value::Array::new(rows, cols, (*values).clone())),
                                ),
                                Ok(_) => FValue::Error(ErrKind::Value),
                                Err(e) => FValue::Error(e),
                            }
                        }
                        _ => FValue::Error(ErrKind::Ref),
                    }
                } else {
                    match parse_a1(rest.trim()) {
                        Some(mut re) => {
                            re.sheet = sheet;
                            match self.sheet_index(re.sheet.as_deref(), si) {
                                Ok(idx) if re.row >= 0 && re.col >= 0 => {
                                    self.cell_value(idx, re.row as u32, re.col as u32)
                                }
                                _ => FValue::Error(ErrKind::Ref),
                            }
                        }
                        None => FValue::Error(ErrKind::Ref),
                    }
                }
            }
            // OFFSET(ref, rows, cols, [height], [width])
            Node::Func(name, nodes) if name == "OFFSET" && nodes.len() >= 3 => {
                let base = match nodes.first() {
                    Some(Node::Ref(re)) => (re.sheet.clone(), re.row, re.col, 1i64, 1i64),
                    Some(Node::Range(a, b)) => (
                        a.sheet.clone(),
                        a.row,
                        a.col,
                        (b.row - a.row).abs() + 1,
                        (b.col - a.col).abs() + 1,
                    ),
                    _ => return FValue::Error(ErrKind::Ref),
                };
                let dr = self.eval_node(&nodes[1], si).to_number().unwrap_or(0.0) as i64;
                let dc = self.eval_node(&nodes[2], si).to_number().unwrap_or(0.0) as i64;
                let h = if nodes.len() > 3 {
                    self.eval_node(&nodes[3], si).to_number().unwrap_or(1.0) as i64
                } else {
                    base.3
                };
                let w = if nodes.len() > 4 {
                    self.eval_node(&nodes[4], si).to_number().unwrap_or(1.0) as i64
                } else {
                    base.4
                };
                let r0 = base.1 + dr;
                let c0 = base.2 + dc;
                if r0 < 0 || c0 < 0 || h <= 0 || w <= 0 {
                    return FValue::Error(ErrKind::Ref);
                }
                let a = RefExpr { sheet: base.0.clone(), row: r0, col: c0, row_abs: false, col_abs: false };
                let b = RefExpr {
                    sheet: base.0,
                    row: r0 + h - 1,
                    col: c0 + w - 1,
                    row_abs: false,
                    col_abs: false,
                };
                match self.range_values(&a, &b, si) {
                    Ok(Arg::Range { values, rows, cols }) => FValue::Array(
                        std::rc::Rc::new(value::Array::new(rows, cols, (*values).clone())),
                    ),
                    _ => FValue::Error(ErrKind::Ref),
                }
            }
            // ADDRESS(row, col, [abs], [a1], [sheet])
            Node::Func(name, nodes) if name == "ADDRESS" && nodes.len() >= 2 => {
                let row = self.eval_node(&nodes[0], si).to_number().unwrap_or(0.0) as u32;
                let col = self.eval_node(&nodes[1], si).to_number().unwrap_or(0.0) as u32;
                let abs = if nodes.len() > 2 {
                    self.eval_node(&nodes[2], si).to_number().unwrap_or(1.0) as i32
                } else {
                    1
                };
                let sheet = if nodes.len() > 4 {
                    self.eval_node(&nodes[4], si).to_text().unwrap_or_default()
                } else {
                    String::new()
                };
                if row == 0 || col == 0 {
                    return FValue::Error(ErrKind::Value);
                }
                let col_part = crate::model::col_label(col - 1);
                let (c_abs, r_abs) = match abs {
                    1 => (true, true),
                    2 => (false, true),
                    3 => (true, false),
                    _ => (false, false),
                };
                let body = format!(
                    "{}{}{}{}",
                    if c_abs { "$" } else { "" },
                    col_part,
                    if r_abs { "$" } else { "" },
                    row
                );
                FValue::Text(if sheet.is_empty() {
                    body
                } else {
                    format!("{}!{}", sheet, body)
                })
            }
            Node::Func(name, nodes) if name == "IFS" => {
                let mut i = 0;
                while i + 1 < nodes.len() {
                    match self.eval_node(&nodes[i], si).to_bool() {
                        Ok(true) => return self.eval_node(&nodes[i + 1], si),
                        Ok(false) => i += 2,
                        Err(e) => return FValue::Error(e),
                    }
                }
                FValue::Error(ErrKind::Na)
            }
            // ============ 查找函数快速路径：避免每次查找都复制整块区域
            Node::Func(name, nodes)
                if matches!(name.as_str(), "VLOOKUP" | "HLOOKUP" | "MATCH" | "XMATCH" | "XLOOKUP")
                    && nodes.len() >= 2 =>
            {
                let key = self.eval_node(&nodes[0], si);
                let literal = |n: &Node| -> Option<(RefExpr, RefExpr)> {
                    match n {
                        Node::Range(a, b) => Some((a.clone(), b.clone())),
                        Node::Ref(a) => Some((a.clone(), a.clone())),
                        _ => None,
                    }
                };
                let Some((la, lb)) = literal(&nodes[1]) else {
                    return self.call_generic(name, nodes, si);
                };
                let idx = match self.sheet_index(la.sheet.as_deref(), si) {
                    Ok(i) => i,
                    Err(e) => return FValue::Error(e),
                };
                let (rows, cols, data) = self.cached_grid(idx, &la, &lb);
                if rows == 0 || cols == 0 {
                    return FValue::Error(ErrKind::Ref);
                }
                let vertical = name != "HLOOKUP";
                let n = if vertical { rows } else { cols };
                let at = |i: usize| -> FValue {
                    let (r, c) = if vertical {
                        (la.row.max(lb.row) as usize + i, la.col.min(lb.col) as usize)
                    } else {
                        (la.row.min(lb.row) as usize, la.col.max(lb.col) as usize + i)
                    };
                    let rr = (r as i64 - la.row.min(lb.row)) as usize;
                    let cc = (c as i64 - la.col.min(lb.col)) as usize;
                    data.get(rr * cols + cc).cloned().unwrap_or(FValue::Empty)
                };
                let _ = at;

                if name == "VLOOKUP" || name == "HLOOKUP" {
                    let col_idx = match nodes.get(2) {
                        Some(n) => self.eval_node(n, si).to_number().unwrap_or(0.0) as usize,
                        None => return FValue::Error(ErrKind::Value),
                    };
                    let approx = match nodes.get(3) {
                        Some(n) => self.eval_node(n, si).to_bool().unwrap_or(true),
                        None => true,
                    };
                    let limit = if vertical { cols } else { rows };
                    if col_idx == 0 || col_idx > limit {
                        return FValue::Error(ErrKind::Ref);
                    }
                    let mut best: Option<usize> = None;
                    if !approx {
                        let ix = self.lookup_hash(idx, &la, &lb, cols, rows, vertical);
                        best = ix.get(&key_norm(&key)).copied();
                    } else {
                        for i in 0..n {
                            let cell = if vertical {
                                &data[i * cols]
                            } else {
                                &data[i]
                            };
                            match cell.compare(&key) {
                                Ok(std::cmp::Ordering::Equal) => {
                                    best = Some(i);
                                    break;
                                }
                                Ok(std::cmp::Ordering::Less) => best = Some(i),
                                _ => {}
                            }
                        }
                    }
                    return match best {
                        Some(i) => {
                            let (r, c) = if vertical {
                                (la.row.min(lb.row) + i as i64, la.col.min(lb.col) + col_idx as i64 - 1)
                            } else {
                                (la.row.min(lb.row) + col_idx as i64 - 1, la.col.min(lb.col) + i as i64)
                            };
                            self.cell_value(idx, r as u32, c as u32)
                        }
                        None => FValue::Error(ErrKind::Na),
                    };
                }

                // MATCH / XMATCH / XLOOKUP：在查找区里找位置
                let mode = match nodes.get(2) {
                    Some(n) => self.eval_node(n, si).to_number().unwrap_or(0.0),
                    None => 0.0,
                };
                let mut hit: Option<usize> = None;
                if mode == 0.0 {
                    let ix = self.lookup_hash(idx, &la, &lb, cols, rows, vertical);
                    hit = ix.get(&key_norm(&key)).copied();
                } else {
                    for i in 0..n {
                        let cell = if vertical { &data[i * cols] } else { &data[i] };
                        if let Ok(std::cmp::Ordering::Equal) = cell.compare(&key) {
                            hit = Some(i);
                            break;
                        }
                    }
                }
                if hit.is_none() && mode == 1.0 {
                    for i in 0..n {
                        let cell = if vertical { &data[i * cols] } else { &data[i] };
                        if let Ok(std::cmp::Ordering::Less) = cell.compare(&key) {
                            hit = Some(i);
                        }
                    }
                }
                if hit.is_none() && mode == -1.0 {
                    for i in (0..n).rev() {
                        let cell = if vertical { &data[i * cols] } else { &data[i] };
                        if let Ok(std::cmp::Ordering::Greater) = cell.compare(&key) {
                            hit = Some(i);
                        }
                    }
                }

                if name == "MATCH" || name == "XMATCH" {
                    return match hit {
                        Some(i) => FValue::number((i + 1) as f64),
                        None => FValue::Error(ErrKind::Na),
                    };
                }

                // XLOOKUP：把位置映射到返回区
                let Some((ra, rb)) = nodes.get(2).and_then(literal) else {
                    return self.call_generic(name, nodes, si);
                };
                let ridx = match self.sheet_index(ra.sheet.as_deref(), si) {
                    Ok(i) => i,
                    Err(e) => return FValue::Error(e),
                };
                match hit {
                    Some(i) => {
                        let (r, c) = if vertical {
                            (ra.row.min(rb.row) + i as i64, ra.col.min(rb.col))
                        } else {
                            (ra.row.min(rb.row), ra.col.min(rb.col) + i as i64)
                        };
                        self.cell_value(ridx, r as u32, c as u32)
                    }
                    None => match nodes.get(3) {
                        Some(n) => self.eval_node(n, si),
                        None => FValue::Error(ErrKind::Na),
                    },
                }
            }
            // ============ 区域聚合缓存：同一块区域被成百上千条公式重复扫时只算一次
            Node::Func(name, nodes)
                if matches!(
                    name.as_str(),
                    "SUM" | "SUMIF" | "COUNTIF" | "COUNTA" | "AVERAGE" | "MAX" | "MIN"
                ) && !nodes.is_empty() =>
            {
                let literal = |n: &Node| -> Option<(RefExpr, RefExpr)> {
                    match n {
                        Node::Range(a, b) => Some((a.clone(), b.clone())),
                        Node::Ref(a) => Some((a.clone(), a.clone())),
                        _ => None,
                    }
                };
                let simple = matches!(name.as_str(), "SUM" | "COUNTA" | "AVERAGE" | "MAX" | "MIN");
                let ok_arity = if simple { nodes.len() == 1 } else { nodes.len() >= 2 };
                if ok_arity {
                    if let Some((ra, rb)) = literal(&nodes[0]) {
                        let si2 = match self.sheet_index(ra.sheet.as_deref(), si) {
                            Ok(i) => i,
                            Err(e) => return FValue::Error(e),
                        };
                        let crit = if simple {
                            String::new()
                        } else {
                            self.eval_node(&nodes[1], si2).to_text().unwrap_or_default()
                        };
                        let (rows, cols) = (
                            (ra.row.max(rb.row) - ra.row.min(rb.row) + 1).max(0),
                            (ra.col.max(rb.col) - ra.col.min(rb.col) + 1).max(0),
                        );
                        let key = (
                            si2,
                            ra.row.min(rb.row),
                            ra.col.min(rb.col),
                            ra.row.max(rb.row),
                            ra.col.max(rb.col),
                            match name.as_str() {
                                "SUM" => 1u8,
                                "SUMIF" => 2,
                                "COUNTIF" => 3,
                                "COUNTA" => 4,
                                "AVERAGE" => 5,
                                "MAX" => 6,
                                _ => 7,
                            },
                            crit.clone(),
                        );
                        if let Some(v) = self.agg_cache.get(&key) {
                            self.agg_hits += 1;
                            return v.clone();
                        }
                        self.agg_miss += 1;
                        let t0 = std::time::Instant::now();
                        let (r_, c_, data) = self.cached_grid(si2, &ra, &rb);
                        let mut out = match name.as_str() {
                            "SUM" => {
                                let mut t = 0.0;
                                let mut err = None;
                                for v in data.iter() {
                                    match v {
                                        FValue::Number(n) => t += n,
                                        FValue::Error(e) => {
                                            err = Some(*e);
                                            break;
                                        }
                                        _ => {}
                                    }
                                }
                                match err {
                                    Some(e) => FValue::Error(e),
                                    None => FValue::number(t),
                                }
                            }
                            "COUNTA" => {
                                let n = data.iter().filter(|v| **v != FValue::Empty).count();
                                FValue::number(n as f64)
                            }
                            "AVERAGE" => {
                                let mut t = 0.0;
                                let mut n = 0usize;
                                for v in data.iter() {
                                    if let FValue::Number(x) = v {
                                        t += x;
                                        n += 1;
                                    }
                                }
                                if n == 0 {
                                    FValue::Error(ErrKind::Div0)
                                } else {
                                    FValue::number(t / n as f64)
                                }
                            }
                            "MAX" | "MIN" => {
                                let mut best: Option<f64> = None;
                                for v in data.iter() {
                                    if let FValue::Number(x) = v {
                                        best = Some(match best {
                                            None => *x,
                                            Some(b) => {
                                                if name == "MAX" {
                                                    b.max(*x)
                                                } else {
                                                    b.min(*x)
                                                }
                                            }
                                        });
                                    }
                                }
                                FValue::number(best.unwrap_or(0.0))
                            }
                            "COUNTIF" => {
                                let c = functions::Crit::parse(&crit);
                                let n = data.iter().filter(|v| c.matches(v)).count();
                                FValue::number(n as f64)
                            }
                            _ => {
                                // SUMIF：第三个参数是和区域，可能跨表
                                let c = functions::Crit::parse(&crit);
                                let sum_vals: Option<std::rc::Rc<Vec<FValue>>> =
                                    match nodes.get(2).and_then(|n| literal(n)) {
                                        Some((sa, sb)) => {
                                            let sidx =
                                                self.sheet_index(sa.sheet.as_deref(), si2).unwrap_or(si2);
                                            let (sr, sc, sd) = self.cached_grid(sidx, &sa, &sb);
                                            let _ = (sr, sc);
                                            Some(sd)
                                        }
                                        None => None,
                                    };
                                let mut t = 0.0;
                                for (i, v) in data.iter().enumerate() {
                                    if c.matches(v) {
                                        let sv = match &sum_vals {
                                            Some(sd) => sd.get(i).cloned(),
                                            None => Some(v.clone()),
                                        };
                                        if let Some(FValue::Number(n)) = sv {
                                            t += n;
                                        }
                                    }
                                }
                                FValue::number(t)
                            }
                        };
                        let _ = (rows, cols, r_, c_);
                        prof_record(&format!("{name}#agg"), t0.elapsed().as_secs_f64());
                        self.agg_cache.insert(key, out.clone());
                        return out;
                    }
                }
                self.call_generic(name, nodes, si)
            }
            Node::Func(name, nodes) => {
                // 名字被 LET 绑定成函数时优先当函数调用
                let bound = self
                    .env_stack
                    .iter()
                    .rev()
                    .find_map(|m| {
                        m.get(name).cloned().or_else(|| {
                            m.iter()
                                .find(|(k, _)| k.eq_ignore_ascii_case(name))
                                .map(|(_, v)| v.clone())
                        })
                    });
                if let Some(FValue::Lambda(lam)) = bound {
                    let mut vals = Vec::with_capacity(nodes.len());
                    for n in nodes.iter() {
                        vals.push(self.eval_node(n, si));
                    }
                    return self.apply_lambda_val(&lam, vals, si);
                }
                let mut args = Vec::with_capacity(nodes.len());
                for n in nodes {
                    // 区域参数保留结构（SUM/COUNTIF 等需要）
                    if let Node::Range(a, b) = n {
                        match self.range_values(a, b, si) {
                            Ok(arg) => args.push(arg),
                            Err(e) => args.push(Arg::Val(FValue::Error(e))),
                        }
                    } else {
                        args.push(Arg::Val(self.eval_node(n, si)));
                    }
                }
                let t0 = std::time::Instant::now();
                let out = functions::call(name, &args);
                prof_record(name, t0.elapsed().as_secs_f64());
                out
            }
        }
    }

    /// 取一个节点可能产生的所有值（区域 → 多值），用于数组广播
    /// 把任意节点求值成 (行, 列, 数据) 网格
    /// 写入区域缓存，超出预算时按先进先出淘汰
    fn cache_put(
        &mut self,
        key: (usize, i64, i64, i64, i64),
        vals: std::rc::Rc<Vec<FValue>>,
    ) -> std::rc::Rc<Vec<FValue>> {
        const LIMIT: usize = 16_000_000; // 约 400 MB
        if self.range_cache.contains_key(&key) {
            return self.range_cache.get(&key).unwrap().clone();
        }
        self.cache_vals += vals.len();
        self.cache_order.push_back((key, vals.len()));
        self.range_cache.insert(key, vals.clone());
        while self.cache_vals > LIMIT {
            match self.cache_order.pop_front() {
                Some((k, n)) => {
                    self.range_cache.remove(&k);
                    self.cache_vals -= n;
                }
                None => break,
            }
        }
        vals
    }

    /// 表的实际使用范围，避免 A:A 这种整列引用被展开成 104 万格
    fn sheet_bounds(&mut self, si: usize) -> (u32, u32) {
        if let Some(b) = self.bounds.get(&si) {
            return *b;
        }
        let mut mr = 0u32;
        let mut mc = 0u32;
        if let Some(sheet) = self.wb.sheets.get(si) {
            for cref in sheet.iter().map(|(k, _)| k) {
                if cref.row > mr {
                    mr = cref.row;
                }
                if cref.col > mc {
                    mc = cref.col;
                }
            }
        }
        self.bounds.insert(si, (mr, mc));
        (mr, mc)
    }

    /// 精确匹配索引：key 归一化后 → 第一次出现的行下标
    fn lookup_hash(
        &mut self,
        si: usize,
        a: &RefExpr,
        b: &RefExpr,
        cols: usize,
        rows: usize,
        vertical: bool,
    ) -> std::rc::Rc<HashMap<String, usize>> {
        let key = (
            si,
            a.row.min(b.row),
            a.col.min(b.col),
            a.row.max(b.row),
            a.col.max(b.col),
        );
        if let Some(ix) = self.lookup_index.get(&key) {
            return ix.clone();
        }
        let (_rows, _c, data) = self.cached_grid(si, a, b);
        let count = if vertical { rows } else { cols };
        let mut map: HashMap<String, usize> = HashMap::with_capacity(count);
        for i in 0..count {
            let v = if vertical {
                data.get(i * cols.max(1))
            } else {
                data.get(i)
            };
            if let Some(v) = v {
                map.entry(key_norm(v)).or_insert(i);
            }
        }
        let rc = std::rc::Rc::new(map);
        self.lookup_index.insert(key, rc.clone());
        rc
    }

    /// 同一轮重算里，同一块区域只物化一次
    fn cached_grid(
        &mut self,
        si: usize,
        a: &RefExpr,
        b: &RefExpr,
    ) -> (usize, usize, std::rc::Rc<Vec<FValue>>) {
        // 同样按实际使用范围裁剪，否则 A:A 会展开成 104 万格
        let (max_r, max_c) = self.sheet_bounds(si);
        let mut br = a.row.max(b.row);
        let mut bc = a.col.max(b.col);
        let area = (br - a.row.min(b.row) + 1).saturating_mul(bc - a.col.min(b.col) + 1);
        if area > 200_000 {
            br = br.min(max_r as i64);
            bc = bc.min(max_c as i64);
        }
        let a = &RefExpr { sheet: a.sheet.clone(), row: a.row.min(b.row), col: a.col.min(b.col), row_abs: false, col_abs: false };
        let b = &RefExpr { sheet: b.sheet.clone(), row: br, col: bc, row_abs: false, col_abs: false };
        let key = (
            si,
            a.row.min(b.row),
            a.col.min(b.col),
            a.row.max(b.row),
            a.col.max(b.col),
        );
        if let Some(v) = self.range_cache.get(&key) {
            let rows = (key.3 - key.1 + 1) as usize;
            let cols = (key.4 - key.2 + 1) as usize;
            return (rows, cols, v.clone());
        }
        let rows = (key.3 - key.1 + 1).max(0) as usize;
        let cols = (key.4 - key.2 + 1).max(0) as usize;
        let mut vals = Vec::with_capacity(rows * cols);
        for r in key.1..=key.3 {
            for c in key.2..=key.4 {
                vals.push(self.cell_value(si, r as u32, c as u32));
            }
        }
        let rc = self.cache_put(key, std::rc::Rc::new(vals));
        (rows, cols, rc)
    }

    /// 查找参数不是字面区域时，退回通用求值路径
    fn call_generic(&mut self, name: &str, nodes: &[Node], si: usize) -> FValue {
        let mut args = Vec::with_capacity(nodes.len());
        for n in nodes {
            if let Node::Range(a, b) = n {
                match self.range_values(a, b, si) {
                    Ok(arg) => args.push(arg),
                    Err(e) => args.push(Arg::Val(FValue::Error(e))),
                }
            } else {
                args.push(Arg::Val(self.eval_node(n, si)));
            }
        }
        functions::call(name, &args)
    }

    fn grid_of(&mut self, n: &Node, si: usize) -> (usize, usize, Vec<FValue>) {
        match n {
            Node::Ref(re) => match self.sheet_index(re.sheet.as_deref(), si) {
                Ok(idx) => {
                    let v = self.cell_value(idx, re.row.max(0) as u32, re.col.max(0) as u32);
                    (1, 1, vec![v])
                }
                Err(_) => (0, 0, vec![]),
            },
            Node::Range(a, b) => match self.range_values(a, b, si) {
                Ok(Arg::Range { values, rows, cols }) => (rows, cols, (*values).clone()),
                _ => (0, 0, vec![]),
            },
            _ => match self.eval_node(n, si) {
                FValue::Array(a) => (a.rows, a.cols, a.data.clone()),
                FValue::Error(e) => (0, 0, vec![FValue::Error(e)]),
                v => (1, 1, vec![v]),
            },
        }
    }

    /// 应用一等公民 lambda
    fn apply_lambda_val(
        &mut self,
        lam: &crate::formula::value::LambdaVal,
        vals: Vec<FValue>,
        si: usize,
    ) -> FValue {
        let mut env: HashMap<String, FValue> = HashMap::new();
        for (p, v) in lam.params.iter().zip(vals.into_iter()) {
            env.insert(p.clone(), v);
        }
        for p in lam.params.iter() {
            env.entry(p.clone()).or_insert(FValue::Empty);
        }
        // 先把定义时捕获的环境铺上，再压参数帧
        let base = self.env_stack.len();
        for frame in lam.env.iter() {
            self.env_stack.push(frame.clone());
        }
        self.env_stack.push(env);
        let out = self.eval_node(&lam.body, si);
        self.env_stack.truncate(base);
        out
    }

    /// 应用 LAMBDA：把实参绑定到形参后求值函数体
    fn apply_lambda(&mut self, lam: &Node, vals: Vec<FValue>, si: usize) -> FValue {
        if let Node::Func(n, parts) = lam {
            if n == "LAMBDA" && !parts.is_empty() {
                let params = &parts[..parts.len() - 1];
                let body = &parts[parts.len() - 1];
                let mut env: HashMap<String, FValue> = HashMap::new();
                for (p, v) in params.iter().zip(vals.into_iter()) {
                    if let Node::Name(pn) = p {
                        env.insert(pn.clone(), v);
                    }
                }
                // 缺省参数按 Empty 处理，供 ISOMITTED 判断
                for p in params {
                    if let Node::Name(pn) = p {
                        env.entry(pn.clone()).or_insert(FValue::Empty);
                    }
                }
                self.env_stack.push(env);
                let out = self.eval_node(body, si);
                self.env_stack.pop();
                return out;
            }
        }
        FValue::Error(ErrKind::Value)
    }

    fn operand_values(&mut self, n: &Node, si: usize) -> Vec<FValue> {
        if let Node::Range(a, b) = n {
            if let Ok(Arg::Range { values, .. }) = self.range_values(a, b, si) {
                return (*values).clone();
            }
        }
        let v = self.eval_node(n, si);
        // MAP/BYROW 这类返回数组的函数，聚合时需要摊平
        if let FValue::Array(a) = &v {
            return a.data.clone();
        }
        vec![v]
    }

    fn eval_binary(&mut self, op: BinOp, l: &Node, r: &Node, si: usize) -> FValue {
        let lv = self.operand_values(l, si);
        let rv = self.operand_values(r, si);
        if lv.len() == 1 && rv.len() == 1 {
            return bin_scalar(op, &lv[0], &rv[0]);
        }
        // 广播：一方是单值就复制，否则要求等长
        let n = lv.len().max(rv.len());
        if lv.len() != 1 && rv.len() != 1 && lv.len() != rv.len() {
            return FValue::Error(ErrKind::Value);
        }
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let a = if lv.len() == 1 { lv[0].clone() } else { lv[i].clone() };
            let b = if rv.len() == 1 { rv[0].clone() } else { rv[i].clone() };
            out.push(bin_scalar(op, &a, &b));
        }
        FValue::Array(std::rc::Rc::new(value::Array::new(1, n, out)))
    }
}

fn bin_scalar(op: BinOp, lv: &FValue, rv: &FValue) -> FValue {
    if let FValue::Error(e) = lv {
        return FValue::Error(*e);
    }
    if let FValue::Error(e) = rv {
        return FValue::Error(*e);
    }

    match op {
        BinOp::Concat => {
            let a = match lv.to_text() {
                Ok(s) => s,
                Err(e) => return FValue::Error(e),
            };
            let b = match rv.to_text() {
                Ok(s) => s,
                Err(e) => return FValue::Error(e),
            };
            FValue::Text(a + &b)
        }
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
            use std::cmp::Ordering::*;
            let ord = match lv.compare(rv) {
                Ok(o) => o,
                Err(e) => return FValue::Error(e),
            };
            let res = match op {
                BinOp::Eq => ord == Equal,
                BinOp::Ne => ord != Equal,
                BinOp::Lt => ord == Less,
                BinOp::Le => ord != Greater,
                BinOp::Gt => ord == Greater,
                _ => ord != Less,
            };
            FValue::Bool(res)
        }
        _ => {
            let a = match lv.to_number() {
                Ok(n) => n,
                Err(e) => return FValue::Error(e),
            };
            let b = match rv.to_number() {
                Ok(n) => n,
                Err(e) => return FValue::Error(e),
            };
            match op {
                BinOp::Add => FValue::number(a + b),
                BinOp::Sub => FValue::number(a - b),
                BinOp::Mul => FValue::number(a * b),
                BinOp::Div => {
                    if b == 0.0 {
                        FValue::Error(ErrKind::Div0)
                    } else {
                        FValue::number(a / b)
                    }
                }
                BinOp::Pow => FValue::number(a.powf(b)),
                _ => FValue::Error(ErrKind::Value),
            }
        }
    }
}

pub fn to_fvalue(v: &crate::model::Value) -> FValue {
    use crate::model::Value as V;
    match v {
        V::Empty => FValue::Empty,
        V::Number(n) => FValue::Number(*n),
        V::Text(s) => FValue::Text(s.to_string()),
        V::Bool(b) => FValue::Bool(*b),
    }
}

pub fn from_fvalue(v: &FValue) -> crate::model::Value {
    use crate::model::Value as V;
    match v {
        FValue::Empty => V::Empty,
        FValue::Number(n) => V::Number(*n),
        FValue::Text(s) => V::Text(s.as_str().into()),
        FValue::Bool(b) => V::Bool(*b),
        FValue::Error(e) => V::Text(e.text().into()),
        FValue::Array(a) => from_fvalue(&a.get(0, 0)),
        // 函数值落盘没有对应类型，Excel 里显示 #CALC!
        FValue::Lambda(_) => V::Text("#CALC!".into()),
    }
}

fn key_norm(v: &FValue) -> String {
    match v {
        FValue::Number(n) => {
            if n.fract() == 0.0 && n.abs() < 1e15 {
                format!("n{}", *n as i64)
            } else {
                format!("n{n}")
            }
        }
        FValue::Text(t) => format!("s{}", t.to_lowercase()),
        FValue::Bool(b) => format!("b{b}"),
        FValue::Empty => "e".into(),
        _ => format!("x{}", v.display()),
    }
}

thread_local! {
    static FN_PROF: std::cell::RefCell<HashMap<String, (u64, f64)>> =
        std::cell::RefCell::new(HashMap::new());
}

pub fn prof_reset() {
    FN_PROF.with(|p| p.borrow_mut().clear());
}

pub fn prof_record(name: &str, secs: f64) {
    FN_PROF.with(|p| {
        let mut m = p.borrow_mut();
        let e = m.entry(name.to_string()).or_insert((0, 0.0));
        e.0 += 1;
        e.1 += secs;
    });
}

pub fn prof_dump() -> Vec<(String, u64, f64)> {
    FN_PROF.with(|p| {
        let m = p.borrow();
        let mut v: Vec<(String, u64, f64)> = m.iter().map(|(k, (c, t))| (k.clone(), *c, *t)).collect();
        v.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap());
        v
    })
}

/// 供条件格式解析 DATE(y,m,d) 用
pub fn date_to_serial_pub(y: i32, m: u32, d: u32) -> Option<f64> {
    functions::date_to_serial(y, m, d)
}

/// 供样式模块格式化日期
pub fn serial_to_date_pub(serial: f64) -> Option<(i32, u32, u32)> {
    functions::serial_to_date(serial)
}

pub fn parse(src: &str) -> Result<Node, ErrKind> {
    let toks = lex(src)?;
    let mut p = Parser { toks, pos: 0 };
    let node = p.parse_expr()?;
    if p.pos != p.toks.len() {
        return Err(ErrKind::Value);
    }
    Ok(node)
}

/// 重新计算整本工作簿的所有公式（含跨表引用、循环检测、动态数组溢出）。
/// 跑两遍：第一遍算出数组并写入溢出区域，第二遍让依赖溢出区域的公式拿到值。

/// 把求值结果写回工作表（处理动态数组溢出与 #SPILL!）
pub fn write_back(wb: &mut Workbook, results: Vec<(usize, u32, u32, FValue)>) {
    for (si, r, c, v) in results {
            match v {
                FValue::Array(a) => {
                    // 检查溢出区域是否被别的数据占着
                    let mut blocked = false;
                    for i in 0..a.rows {
                        for j in 0..a.cols {
                            if i == 0 && j == 0 {
                                continue;
                            }
                            let key = CellRef::new(r + i as u32, c + j as u32);
                            if let Some(cell) = wb.sheets[si].get(key.row, key.col) {
                                if !cell.is_blank() && !wb.sheets[si].spilled.contains(&key) {
                                    blocked = true;
                                }
                            }
                        }
                    }
                    if blocked {
                        if let Some(cell) = wb.sheets[si].get_mut(r, c) {
                            cell.value = from_fvalue(&FValue::Error(ErrKind::Spill));
                        }
                        continue;
                    }
                    for i in 0..a.rows {
                        for j in 0..a.cols {
                            let key = CellRef::new(r + i as u32, c + j as u32);
                            let val = from_fvalue(&a.get(i, j));
                            if i == 0 && j == 0 {
                                if let Some(cell) = wb.sheets[si].get_mut(key.row, key.col) {
                                    cell.value = val;
                                }
                            } else {
                                wb.sheets[si].set(key.row, key.col, Cell { value: val, formula: None });
                                wb.sheets[si].spilled.insert(key);
                            }
                        }
                    }
                }
                other => {
                    if let Some(cell) = wb.sheets[si].get_mut(r, c) {
                        cell.value = from_fvalue(&other);
                    }
                }
            }
        }
}

/// 增量重算：只算受影响的公式单元格，其余用缓存值。
/// 返回实际重算的格子数。
pub fn recalc_dirty(wb: &mut Workbook, deps: &deps::Deps, seeds: &[deps::CellKey]) -> usize {
    if std::env::var("SC_PROFILE").is_ok() {
        prof_reset();
    }
    let dirty = deps.closure(seeds);
    if dirty.is_empty() {
        return 0;
    }
    // 不需要拓扑排序：Calc 求值时递归拉取依赖，memo 保证每个格子只算一次
    let order: Vec<deps::CellKey> = dirty.iter().cloned().collect();
    let mut results = Vec::with_capacity(order.len());
    {
        let mut calc = Calc::new(wb);
        calc.scope = Some(dirty.clone());
        for (si, r, c) in &order {
            let v = calc.cell_value(*si, *r, *c);
            results.push((*si, *r, *c, v));
        }
    }
    if std::env::var("SC_PROFILE").is_ok() {
        for (n, c, t) in prof_dump().into_iter().take(6) {
            eprintln!("  [edit] {n:<12} {c:>7} 次  {t:>7.3}s");
        }
    }
    write_back(wb, results);
    order.len()
}

pub fn recalc_all(wb: &mut Workbook) {
    let phase = std::env::var("SC_PHASE").is_ok();
    let t_all = std::time::Instant::now();
    let mut t_phase = std::time::Instant::now();
    macro_rules! mark {
        ($name:expr) => {
            if phase {
                eprintln!("  [phase] {:<12} {:.2}s", $name, t_phase.elapsed().as_secs_f64());
                t_phase = std::time::Instant::now();
            }
        };
    }
    // 文件里带 t="array" ref 的公式，覆盖区域里存的是上次的结果值；
    // 先把这些格子登记成溢出区，否则重算时会被自己上次的结果挡住（#SPILL!）
    for sheet in wb.sheets.iter_mut() {
        for (anchor, (r1, c1, r2, c2)) in sheet.arrays.clone() {
            for r in r1..=r2 {
                for c in c1..=c2 {
                    let k = CellRef::new(r, c);
                    if k != anchor {
                        sheet.spilled.insert(k);
                    }
                }
            }
        }
    }
    // 先清掉上一轮的溢出单元格
    for si in 0..wb.sheets.len() {
        let old: Vec<CellRef> = wb.sheets[si].spilled.drain().collect();
        for k in old {
            wb.sheets[si].remove(k.row, k.col);
        }
    }

    mark!("清溢出");
    // 遍数：默认一遍。cell_value 会按依赖递归求值，顺序问题由 memo 解决；
    // 只有含易变函数（NOW/TODAY/RAND/OFFSET/INDIRECT…）或第一遍出错的表才跑第二遍。
    let volatile = wb.sheets.iter().any(|sh| {
        sh.iter().any(|(_, c)| {
            c.formula
                .as_ref()
                .map(|f| {
                    const VOL: [&str; 8] = [
                        "NOW(", "TODAY(", "RAND(", "RANDBETWEEN(", "OFFSET(", "INDIRECT(", "CELL(",
                        "INFO(",
                    ];
                    let up = f.to_ascii_uppercase();
                    VOL.iter().any(|v| up.contains(v))
                })
                .unwrap_or(false)
        })
    });
    let mut passes = if volatile || std::env::var("SC_TWOPASS").is_ok() { 2 } else { 1 };
    let mut pass_done = 0usize;
    for _pass in 0..passes {
        pass_done += 1;
        let formulas: Vec<(usize, u32, u32)> = wb
            .sheets
            .iter()
            .enumerate()
            .flat_map(|(si, s)| {
                s.iter()
                    .filter(|(_, c)| c.formula.is_some())
                    .map(move |(r, _)| (si, r.row, r.col))
            })
            .collect();

        mark!("收集公式");
        let mut results: Vec<(usize, u32, u32, FValue)> = Vec::with_capacity(formulas.len());
        {
            let mut calc = Calc::new(wb);
            let profile = std::env::var("SC_PROFILE").is_ok();
            let mut slow: Vec<(f64, usize, u32, u32, String)> = Vec::new();
            for (si, r, c) in formulas {
                let t0 = std::time::Instant::now();
                let v = calc.cell_value(si, r, c);
                if profile {
                    slow.push((
                        t0.elapsed().as_secs_f64(),
                        si,
                        r,
                        c,
                        wb.sheets[si]
                            .get(r, c)
                            .and_then(|x| x.formula.as_deref().map(|f| f.to_string()))
                            .unwrap_or_default(),
                    ));
                }
                results.push((si, r, c, v));
            }
            if profile {
                for (n, c, t) in prof_dump().into_iter().take(15) {
                    eprintln!("  [fn] {n:<12} {c:>8} 次  {t:>8.2}s");
                }
                slow.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
                let total: f64 = slow.iter().map(|x| x.0).sum();
                eprintln!(
                    "[profile] 公式数 {} 总耗时 {:.2}s  聚合缓存命中 {} 未命中 {}",
                    slow.len(),
                    total,
                    calc.agg_hits,
                    calc.agg_miss
                );
                for (d, si, r, c, f) in slow.iter().take(12) {
                    eprintln!("  {:>7.3}s  {}!R{}C{}  {}", d, si, r + 1, c + 1, f.chars().take(90).collect::<String>().as_str());
                }
            }
        }

        mark!("求值");
        // 第一遍就出现错误值（可能是求值顺序导致的），补一遍
        if _pass == 0 && passes == 1 && results.iter().any(|r| matches!(r.3, FValue::Error(_))) {
            passes = 2;
        }
        write_back(wb, results);
        mark!("写回");
        if std::env::var("SC_CHECKSUM").is_ok() {
            let mut sum = 0.0f64;
            let mut cnt = 0usize;
            for sh in wb.sheets.iter() {
                for (_, cell) in sh.iter() {
                    if let crate::model::Value::Number(n) = cell.value {
                        sum += n;
                        cnt += 1;
                    }
                }
            }
            eprintln!("[checksum] pass {} 数值格 {} 合计 {:.6}", _pass + 1, cnt, sum);
        }
    }
    mark!("结束");
    if phase {
        eprintln!("[recalc] 总耗时 {:.2}s（{} 遍）", t_all.elapsed().as_secs_f64(), pass_done);
    }
}

/// 求单个单元格的值（测试与 UI 用）
pub fn eval_cell(wb: &Workbook, si: usize, r: u32, c: u32) -> FValue {
    let mut calc = Calc::new(wb);
    calc.cell_value(si, r, c)
}

/// 直接求一段公式文本（测试用）
pub fn eval_formula(wb: &Workbook, si: usize, formula: &str) -> FValue {
    let raw = formula.strip_prefix('=').unwrap_or(formula).to_string();
    let expanded = crate::table::expand_structured(wb, si, &raw).unwrap_or(raw);
    let normalized = crate::normalize_formula(&expanded);
    let body = normalized.as_str();
    match parse(body) {
        Ok(node) => {
            let mut calc = Calc::new(wb);
            calc.eval_node(&node, si)
        }
        Err(e) => FValue::Error(e),
    }
}

pub fn set_formula(sheet: &mut Sheet, r: u32, c: u32, formula: &str, cached: FValue) {
    let cell = Cell {
        value: from_fvalue(&cached),
        formula: Some(if formula.starts_with('=') {
            formula.into()
        } else {
            format!("={formula}").into()
        }),
    };
    sheet.set(r, c, cell);
}
