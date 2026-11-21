//! Excel 公式：词法、语法、求值
//!
//! 语法优先级（低 → 高），与 Excel 一致：
//!   比较(= <> < > <= >=) < 连接(&) < 加减 < 乘除 < 乘方 < 一元负号 < 百分号 < 区间(:)

pub mod array;
pub mod deps;
pub mod dist;
pub mod extra;
pub mod fncat;
pub mod functions;
pub mod value;

pub use value::{ErrKind, FValue};

use crate::model::{Cell, CellRef, Sheet, Workbook};
use std::collections::{HashMap, HashSet};

// ---------------------------------------------------------------- AST

#[derive(Clone, Debug, PartialEq)]
pub struct RefExpr {
    /// 外部工作簿：`[1]` 或 `[预算.xlsx]` 里的那段；None 表示本簿
    pub book: Option<String>,
    pub sheet: Option<String>,
    /// 三维引用的结束表名：`Sheet1:Sheet3!A1` 的 Sheet3
    pub sheet2: Option<String>,
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
    /// 限定作用域的名称：工作簿级 `[1]!TaxRate` 或工作表级 `Sheet1!Rate` / `[1]Sheet1!Rate`
    ScopedName {
        book: Option<String>,
        sheet: Option<String>,
        name: String,
    },
    Number(f64),
    Text(String),
    Bool(bool),
    Err(ErrKind),
    Ref(RefExpr),
    ///  spills  #：A1# 返回锚点动态数组的完整区域
    Spill(RefExpr),
    /// 隐式交集运算符 `@`：按公式所在行/列与引用区域求交
    Implicit(Box<Node>),
    Range(RefExpr, RefExpr),
    Func(String, Vec<Node>),
    Unary(bool, Box<Node>), // true = 负号
    Percent(Box<Node>),
    Binary(BinOp, Box<Node>, Box<Node>),
    /// 数组常量 {1,2;3,4}
    Array(usize, usize, Vec<FValue>),
}

/// A SUMIF amount reference supplies the top-left cell. Dependency indexing and
/// evaluation share this rectangle, while export keeps the user's original formula.
pub(crate) fn sumif_amount_range(args: &[Node]) -> Option<Node> {
    let [criteria, _, amounts] = args else { return None };
    let (height, width) = match criteria {
        Node::Range(a, b) => ((a.row - b.row).abs(), (a.col - b.col).abs()),
        Node::Ref(_) => (0, 0),
        _ => return None,
    };
    let start = match amounts {
        Node::Ref(a) => a.clone(),
        Node::Range(a, b) => RefExpr { row: a.row.min(b.row), col: a.col.min(b.col), ..a.clone() },
        _ => return None,
    };
    let end = RefExpr {
        row: (start.row + height).min(1_048_575),
        col: (start.col + width).min(16_383),
        ..start.clone()
    };
    Some(Node::Range(start, end))
}

// ---------------------------------------------------------------- 词法

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    Num(f64),
    Str(String),
    Ident(String),
    /// `[1]` / `[预算.xlsx]` 里的书名
    Book(String),
    Op(&'static str),
    LParen,
    RParen,
    LBrace,
    RBrace,
    Comma,
    Semi,
    Colon,
    Bang,
    Hash,
    At,
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
            // 外部工作簿：[1]预算!A1、[预算.xlsx]Sheet1!A1
            '[' => {
                let mut name = String::new();
                i += 1;
                while i < b.len() && b[i] != ']' {
                    name.push(b[i]);
                    i += 1;
                }
                if i >= b.len() {
                    return Err(ErrKind::Value);
                }
                i += 1;
                out.push(Tok::Book(name));
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
            '@' => {
                out.push(Tok::At);
                i += 1;
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
                // 错误字面量 #N/A #REF! 等；单独的 # 是动态数组溢出运算符 A1#。
                let start = i;
                i += 1;
                while i < b.len() && (b[i].is_alphanumeric() || b[i] == '/' || b[i] == '!' || b[i] == '_') {
                    i += 1;
                }
                let lit: String = b[start..i].iter().collect();
                if lit == "#" {
                    out.push(Tok::Hash);
                    continue;
                }
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
        if self.eat(&Tok::At) {
            let e = self.parse_unary()?;
            return Ok(Node::Implicit(Box::new(e)));
        }
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
                        // 数组常量里的错误字面量：`{1;#N/A}`。字面量在词法阶段被编成
                        // "\0ERRn="，这里必须认，否则整个常量直接 #VALUE!。
                        Some(Tok::Ident(id)) if id.starts_with('\u{0}') => {
                            let code = id
                                .trim_start_matches('\u{0}')
                                .strip_prefix("ERR")
                                .and_then(|c| c.trim_end_matches('=').parse::<u8>().ok())
                                .ok_or(ErrKind::Name)?;
                            FValue::Error(match code {
                                0 => ErrKind::Null,
                                1 => ErrKind::Div0,
                                2 => ErrKind::Value,
                                3 => ErrKind::Ref,
                                4 => ErrKind::Name,
                                5 => ErrKind::Num,
                                _ => ErrKind::Na,
                            })
                        }
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
            Some(Tok::Book(book)) => {
                // [1]!TaxRate（外部工作簿级命名范围）
                if self.eat(&Tok::Bang) {
                    return self.parse_bang_target(Some(book), String::new(), None);
                }
                // [1]预算!A1 / [1]First:Third!A1 / [1]'First 1':'Third 3'!A1:B2 / [1]Sheet1!LocalName
                let Some(Tok::Ident(sheet)) = self.next() else {
                    return Err(ErrKind::Ref);
                };
                if self.eat(&Tok::Colon) {
                    let Some(Tok::Ident(end_sheet)) = self.next() else {
                        return Err(ErrKind::Ref);
                    };
                    if !self.eat(&Tok::Bang) {
                        return Err(ErrKind::Ref);
                    }
                    return self.parse_bang_target(Some(book), sheet, Some(end_sheet));
                }
                if !self.eat(&Tok::Bang) {
                    return Err(ErrKind::Ref);
                }
                if let Some((first, last)) = sheet.split_once(':') {
                    if first.is_empty() || last.is_empty() {
                        return Err(ErrKind::Ref);
                    }
                    return self.parse_bang_target(
                        Some(book),
                        first.to_string(),
                        Some(last.to_string()),
                    );
                }
                return self.parse_bang_target(Some(book), sheet, None);
            }
            Some(Tok::Ident(name)) => {
                // 错误字面量
                if let Some(e) = decode_err_ident(&name) {
                    return Ok(Node::Err(e));
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

    fn parse_bang_target(
        &mut self,
        book: Option<String>,
        sheet: String,
        sheet2: Option<String>,
    ) -> Result<Node, ErrKind> {
        let Some(Tok::Ident(target)) = self.next() else {
            return Err(ErrKind::Ref);
        };
        if let Some(e) = decode_err_ident(&target) {
            if self.eat(&Tok::Colon) {
                let _ = self.next();
            }
            return Ok(Node::Err(e));
        }
        if sheet.is_empty() {
            if book.is_some() {
                return Ok(Node::ScopedName {
                    book,
                    sheet: None,
                    name: target,
                });
            }
            return Err(ErrKind::Ref);
        }
        let mut start = match parse_a1(&target) {
            Some(r) => r,
            None if sheet2.is_none() => {
                return Ok(Node::ScopedName {
                    book,
                    sheet: Some(sheet),
                    name: target,
                });
            }
            None => return Err(ErrKind::Ref),
        };
        start.book = book;
        start.sheet = Some(sheet);
        start.sheet2 = sheet2;
        if self.eat(&Tok::Colon) {
            let Some(Tok::Ident(cell2)) = self.next() else {
                return Err(ErrKind::Ref);
            };
            if let Some(e) = decode_err_ident(&cell2) {
                return Ok(Node::Err(e));
            }
            let mut end = parse_a1(&cell2).ok_or(ErrKind::Ref)?;
            end.book = start.book.clone();
            end.sheet = start.sheet.clone();
            end.sheet2 = start.sheet2.clone();
            return Ok(Node::Range(start, end));
        }
        if self.eat(&Tok::Hash) {
            return Ok(Node::Spill(start));
        }
        if start.sheet2.is_none() && self.eats_intersection() {
            return Ok(Node::Err(ErrKind::Null));
        }
        Ok(Node::Ref(start))
    }

    /// name 可能是 "A1"、"$A$1"、"Sheet1"（后面跟 !）或 `'[1]First:Third'`
    fn parse_ref_from(&mut self, name: String) -> Result<Node, ErrKind> {
        // 三维引用：Sheet1:Sheet3!A1 / Sheet1:Sheet3!A1:B2 / '[1]First 1':'Third 3'!A1。
        // 必须在普通 A1 区域的冒号分支之前识别，否则 Sheet3! 会被当成
        // 普通单元格区域的一部分。
        if self.peek() == Some(&Tok::Colon) {
            if let (Some(Tok::Ident(end_sheet)), Some(Tok::Bang)) =
                (self.toks.get(self.pos + 1).cloned(), self.toks.get(self.pos + 2))
            {
                self.pos += 3; // ':' end_sheet '!'
                let (book, first_sheet) = split_book_sheet(&name);
                return self.parse_bang_target(book, first_sheet, Some(end_sheet));
            }
        }
        // 带 ! 限定符的引用：本簿表/三维、外部工作簿单表/三维、作用域限定名称。
        if matches!(self.peek(), Some(Tok::Bang)) {
            self.pos += 1;
            let (book, sheet) = split_book_sheet(&name);
            if let Some((first, last)) = sheet.split_once(':') {
                if first.is_empty() || last.is_empty() {
                    return Err(ErrKind::Ref);
                }
                return self.parse_bang_target(book, first.to_owned(), Some(last.to_owned()));
            }
            return self.parse_bang_target(book, sheet, None);
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
            if let Some(e) = decode_err_ident(&cell2) {
                return Ok(Node::Err(e));
            }
            let end = parse_a1(&cell2).ok_or(ErrKind::Ref)?;
            if self.eats_intersection() {
                return Ok(Node::Err(ErrKind::Null));
            }
            return Ok(Node::Range(start, end));
        }
        if self.eat(&Tok::Hash) {
            return Ok(Node::Spill(start));
        }
        if self.eats_intersection() {
            return Ok(Node::Err(ErrKind::Null));
        }
        Ok(Node::Ref(start))
    }
}

fn decode_err_ident(name: &str) -> Option<ErrKind> {
    let code = name.strip_prefix("\u{0}ERR")?.trim_end_matches('=').parse::<u8>().ok()?;
    Some(match code {
        0 => ErrKind::Null,
        1 => ErrKind::Div0,
        2 => ErrKind::Value,
        3 => ErrKind::Ref,
        4 => ErrKind::Name,
        5 => ErrKind::Num,
        _ => ErrKind::Na,
    })
}

/// `'[预算.xlsx]Sheet1'` → `(Some("预算.xlsx"), "Sheet1")`；`'[1]'` → `(Some("1"), "")`；没有书名就原样返回
fn split_book_sheet(name: &str) -> (Option<String>, String) {
    if let Some(close) = name.rfind(']') {
        if let Some(open) = name[..close].rfind('[') {
            let book = name[open + 1..close].to_string();
            let sheet = name[close + 1..].to_string();
            if !book.is_empty() {
                return (Some(book), sheet);
            }
        }
    }
    (None, name.to_string())
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
    Some(RefExpr { book: None, sheet: None, sheet2: None, row, col, row_abs, col_abs })
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
    cur_sheet: usize,
    /// 同一次重算的溢出稳定化阶段重放随机/时间采样，不反复生成新值。
    volatile_values: HashMap<(deps::CellKey, usize, String), FValue>,
    volatile_counts: HashMap<deps::CellKey, usize>,
    /// 进入时外层的日期体系，Drop 时恢复
    prev_system: crate::model::DateSystem,
    /// 进入时外层的 NOW 快照，Drop 时恢复
    prev_now: Option<f64>,
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
    /// 区域物化实际读取的单元格数（测试/诊断用）
    pub range_reads: usize,
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

impl Drop for Calc<'_> {
    fn drop(&mut self) {
        functions::set_current_date_system(self.prev_system);
        functions::set_now_snapshot(self.prev_now);
    }
}

impl<'a> Calc<'a> {
    pub fn new(wb: &'a Workbook) -> Self {
        let prev_system = functions::current_date_system();
        functions::set_current_date_system(wb.date_system);
        let prev_now = functions::now_snapshot();
        if prev_now.is_none() {
            let now = functions::now_serial();
            functions::set_now_snapshot(Some(now));
        }
        Self { wb, memo: HashMap::new(), visiting: HashSet::new(), env_stack: vec![HashMap::new()], cur: (0, 0), range_cache: HashMap::new(), bounds: HashMap::new(), lookup_index: HashMap::new(), scope: None,
            agg_cache: HashMap::new(), cache_vals: 0, depth: 0,
            range_reads: 0,
            cache_order: std::collections::VecDeque::new(), agg_hits: 0, agg_miss: 0,
            cur_sheet: 0, volatile_values: HashMap::new(), volatile_counts: HashMap::new(),
            prev_system, prev_now }
    }

    /// 本次求值实际读取的区域单元格数；用于验证整行/整列没有逐格扫全表。
    pub fn range_reads(&self) -> usize {
        self.range_reads
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

    /// 三维引用包含的 sheet 下标（按当前表标签顺序，含首尾）。
    fn three_d_indices(&self, r: &RefExpr) -> Result<Vec<usize>, ErrKind> {
        let start = r.sheet.as_deref().ok_or(ErrKind::Ref)?;
        let end = r.sheet2.as_deref().ok_or(ErrKind::Ref)?;
        let a = self.wb.sheets.iter().position(|s| s.name.eq_ignore_ascii_case(start)).ok_or(ErrKind::Ref)?;
        let b = self.wb.sheets.iter().position(|s| s.name.eq_ignore_ascii_case(end)).ok_or(ErrKind::Ref)?;
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        Ok((lo..=hi).collect())
    }

    /// 外部引用单格：从 links 的缓存里取；没有缓存（链接没解析过）就是 #REF!
    fn link_cell(&self, r: &RefExpr) -> FValue {
        if r.row < 0 || r.col < 0 {
            return FValue::Error(ErrKind::Ref);
        }
        let Some(li) = crate::xlinks::resolve_book(self.wb, r.book.as_deref().unwrap_or("")) else {
            return FValue::Error(ErrKind::Ref);
        };
        let link = &self.wb.links[li];
        if link.cache.is_empty() {
            return FValue::Error(ErrKind::Ref);
        }
        let Some(slot) = crate::xlinks::sheet_slot(link, r.sheet.as_deref()) else {
            return FValue::Error(ErrKind::Ref);
        };
        match link.cache.get(&(slot, r.row as u32, r.col as u32)) {
            Some(v) => to_fvalue(v),
            None => FValue::Empty,
        }
    }

    /// 外部引用区域：按缓存矩形取，缺的格当空
    fn link_range(&mut self, a: &RefExpr, b: &RefExpr) -> Result<Arg, ErrKind> {
        let book = a.book.as_ref().or(b.book.as_ref()).cloned().unwrap_or_default();
        let Some(li) = crate::xlinks::resolve_book(self.wb, &book) else {
            return Err(ErrKind::Ref);
        };
        let link = &self.wb.links[li];
        if link.cache.is_empty() {
            return Err(ErrKind::Ref);
        }
        let sheet = a.sheet.clone().or_else(|| b.sheet.clone());
        let Some(slot) = crate::xlinks::sheet_slot(link, sheet.as_deref()) else {
            return Err(ErrKind::Ref);
        };
        let mut r1 = a.row.min(b.row).max(0) as u32;
        let mut c1 = a.col.min(b.col).max(0) as u32;
        let mut r2 = a.row.max(b.row).max(0) as u32;
        let mut c2 = a.col.max(b.col).max(0) as u32;
        // 整列引用（A:A）按缓存边界裁剪，别展开 104 万格
        if (r2 as u64 - r1 as u64 + 1) * (c2 as u64 - c1 as u64 + 1) > 200_000 {
            let max_r = link
                .cache
                .keys()
                .filter(|(s, _, _)| *s == slot)
                .map(|(_, r, _)| *r)
                .max()
                .unwrap_or(0);
            let max_c = link
                .cache
                .keys()
                .filter(|(s, _, _)| *s == slot)
                .map(|(_, _, c)| *c)
                .max()
                .unwrap_or(0);
            r2 = r2.min(max_r);
            c2 = c2.min(max_c);
            if r1 > r2 || c1 > c2 {
                r1 = 0;
                c1 = 0;
                r2 = 0;
                c2 = 0;
            }
        }
        let rows = (r2 - r1 + 1) as usize;
        let cols = (c2 - c1 + 1) as usize;
        let mut vals = Vec::with_capacity(rows * cols);
        for r in r1..=r2 {
            for c in c1..=c2 {
                let v = link
                    .cache
                    .get(&(slot, r, c))
                    .cloned()
                    .unwrap_or(crate::model::Value::Empty);
                vals.push(to_fvalue(&v));
            }
        }
        Ok(Arg::Range {
            values: std::rc::Rc::new(vals),
            rows,
            cols,
        })
    }

    /// 外部工作簿三维区间 `[1]First:Third!A1` 的 `sheetId` 范围（按 `link.sheets` 顺序）。
    fn link_three_d_slots(
        &self,
        link: &crate::xlinks::ExternalBook,
        r: &RefExpr,
    ) -> Result<Vec<usize>, ErrKind> {
        let start = r.sheet.as_deref().ok_or(ErrKind::Ref)?.trim().trim_matches('\'');
        let end = r.sheet2.as_deref().ok_or(ErrKind::Ref)?.trim().trim_matches('\'');
        let a = link
            .sheets
            .iter()
            .position(|s| s.eq_ignore_ascii_case(start))
            .ok_or(ErrKind::Ref)?;
        let b = link
            .sheets
            .iter()
            .position(|s| s.eq_ignore_ascii_case(end))
            .ok_or(ErrKind::Ref)?;
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        Ok((lo..=hi).collect())
    }

    /// 外部工作簿三维单格 `[1]First:Third!A1` → 纵向数组，供 SUM / AVERAGE / MAX / MIN 等聚合函数统一消费。
    fn link_three_d_cell(&self, r: &RefExpr) -> FValue {
        if r.row < 0 || r.col < 0 {
            return FValue::Error(ErrKind::Ref);
        }
        let Some(li) = crate::xlinks::resolve_book(self.wb, r.book.as_deref().unwrap_or("")) else {
            return FValue::Error(ErrKind::Ref);
        };
        let link = &self.wb.links[li];
        if link.cache.is_empty() && !link.refreshed {
            return FValue::Error(ErrKind::Ref);
        }
        let slots = match self.link_three_d_slots(link, r) {
            Ok(v) => v,
            Err(e) => return FValue::Error(e),
        };
        let mut vals = Vec::with_capacity(slots.len());
        for slot in slots {
            let v = match link.cache.get(&(slot, r.row as u32, r.col as u32)) {
                Some(v) => to_fvalue(v),
                None => FValue::Empty,
            };
            vals.push(v);
        }
        FValue::Array(std::rc::Rc::new(value::Array::new(vals.len().max(1), 1, vals)))
    }

    /// 外部工作簿三维区域 `[1]First:Third!A1:B2` → 纵向拼接的多表二维网格。
    fn link_three_d_range(&self, a: &RefExpr, b: &RefExpr) -> Result<FValue, ErrKind> {
        let book = a.book.as_deref().or(b.book.as_deref()).unwrap_or("");
        let Some(li) = crate::xlinks::resolve_book(self.wb, book) else {
            return Err(ErrKind::Ref);
        };
        let link = &self.wb.links[li];
        if link.cache.is_empty() && !link.refreshed {
            return Err(ErrKind::Ref);
        }
        let rep = if a.sheet2.is_some() { a } else { b };
        let slots = self.link_three_d_slots(link, rep)?;
        let mut r1 = a.row.min(b.row).max(0) as u32;
        let mut c1 = a.col.min(b.col).max(0) as u32;
        let mut r2 = a.row.max(b.row).max(0) as u32;
        let mut c2 = a.col.max(b.col).max(0) as u32;
        let full_cols = c1 == 0 && c2 == 16_383;
        let full_rows = r1 == 0 && r2 == 1_048_575;
        let huge = (r2 as u64 - r1 as u64 + 1) * (c2 as u64 - c1 as u64 + 1) > 200_000;
        if full_cols || full_rows || huge {
            let max_r = link
                .cache
                .keys()
                .filter(|(s, _, _)| slots.contains(s))
                .map(|(_, r, _)| *r)
                .max()
                .unwrap_or(0);
            let max_c = link
                .cache
                .keys()
                .filter(|(s, _, _)| slots.contains(s))
                .map(|(_, _, c)| *c)
                .max()
                .unwrap_or(0);
            if full_rows || huge {
                r2 = r2.min(max_r);
            }
            if full_cols || huge {
                c2 = c2.min(max_c);
            }
            if r1 > r2 || c1 > c2 {
                return Ok(FValue::Array(std::rc::Rc::new(value::Array::new(1, 1, vec![FValue::Empty]))));
            }
        }
        let rows_per = (r2 - r1 + 1) as usize;
        let cols = (c2 - c1 + 1) as usize;
        let mut vals = Vec::with_capacity(slots.len() * rows_per * cols);
        for slot in &slots {
            for r in r1..=r2 {
                for c in c1..=c2 {
                    let v = match link.cache.get(&(*slot, r, c)) {
                        Some(v) => to_fvalue(v),
                        None => FValue::Empty,
                    };
                    vals.push(v);
                }
            }
        }
        Ok(FValue::Array(std::rc::Rc::new(value::Array::new(
            (slots.len() * rows_per).max(1),
            cols.max(1),
            vals,
        ))))
    }

    fn eval_ext_defined_name(
        &mut self,
        li: usize,
        sheet: Option<&str>,
        name: &str,
        si: usize,
    ) -> FValue {
        if self.depth >= 200 {
            return FValue::Error(ErrKind::Ref);
        }
        let Some(link) = self.wb.links.get(li) else {
            return FValue::Error(ErrKind::Ref);
        };
        let want_slot = sheet.and_then(|sh| crate::xlinks::sheet_slot(link, Some(sh)));
        if sheet.is_some() && want_slot.is_none() {
            return FValue::Error(ErrKind::Ref);
        }
        let found = if let Some(slot) = want_slot {
            link.defined_names
                .iter()
                .find(|(nm, s, _)| nm.eq_ignore_ascii_case(name) && *s == Some(slot))
                .or_else(|| {
                    link.defined_names
                        .iter()
                        .find(|(nm, s, _)| nm.eq_ignore_ascii_case(name) && s.is_none())
                })
        } else {
            link.defined_names
                .iter()
                .find(|(nm, s, _)| nm.eq_ignore_ascii_case(name) && s.is_none())
        };
        let Some((_, _, refers)) = found.cloned() else {
            return FValue::Error(ErrKind::Name);
        };
        let body = refers.trim().trim_start_matches('=');
        if body.is_empty() {
            return FValue::Error(ErrKind::Name);
        }
        let normalized = crate::normalize_formula(body);
        let Ok(mut node) = parse(&normalized) else {
            return FValue::Error(ErrKind::Value);
        };
        let book_str = (li + 1).to_string();
        let default_sh = sheet.or_else(|| {
            self.wb
                .links
                .get(li)
                .and_then(|l| l.sheets.first().map(|s| s.as_str()))
        });
        qualify_ext_node(&mut node, &book_str, default_sh);
        self.depth += 1;
        let out = self.eval_node(&node, si);
        self.depth -= 1;
        out
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
            let raw = f.strip_prefix('=').unwrap_or(f).to_string();
            // 结构化引用：Table1[列] → A1 区域
            let expanded = crate::table::expand_structured_at(self.wb, si, Some(r), Some(c), &raw).unwrap_or(raw);
            let normalized = crate::normalize_formula(&expanded);
            let body = normalized.as_str();
            // 合法但尚未实现的引用组合：保留文件里的缓存，不让 parse 失败把
            // 已知结果写成 #REF!/#VALUE!。只对明确的组合生效，真正的运算错误
            //（包括本来就缓存为错误的格）仍走正常求值。
            if let Some(_reason) = unsupported_reference_combo(body) {
                let cached = to_fvalue(&cell.value);
                if !matches!(cached, FValue::Error(_)) {
                    return cached;
                }
            }
            if !self.visiting.insert(key) {
                CYCLE_CELLS.with(|cells| {
                    cells.borrow_mut().insert(key);
                });
                if self.wb.calc_iterate {
                    // 迭代计算：环上的格子取「上一轮的值」（首轮取 0，与 Excel 一致），
                    // 外层多遍扫描会把结果逐步推近不动点。
                    let prev = to_fvalue(&cell.value);
                    return match prev {
                        FValue::Error(_) | FValue::Empty => FValue::number(0.0),
                        v => v,
                    };
                }
                return FValue::Error(ErrKind::Circular);
            }
            let previous = (self.cur_sheet, self.cur);
            self.cur_sheet = si;
            self.cur = (r, c);
            self.volatile_counts.insert(key, 0);
            let mut v = match parse(body) {
                Ok(node) => self.eval_node(&node, si),
                Err(e) => FValue::Error(e),
            };
            if let FValue::Array(a) = &v {
                if array_blocked(self.wb, si, r, c, a, true) {
                    v = FValue::Error(ErrKind::Spill);
                }
            }
            (self.cur_sheet, self.cur) = previous;
            self.visiting.remove(&key);
            self.memo.insert(key, v.clone());
            v
        } else {
            to_fvalue(&cell.value)
        }
    }

    /// 读取已经物化的溢出区域（用于只重算其他表时的跨表 `A1#`）。
    /// 只承认重算写回时记录的锚点形状；1×1 数组同样有记录。`spilled`
    /// 只保存非锚点尾格，不能拿来反推形状，也不能把尾格当锚点。
    fn materialized_spill(&self, si: usize, r: u32, c: u32) -> Option<value::Array> {
        let sh = self.wb.sheets.get(si)?;
        let anchor = CellRef::new(r, c);
        if let Some(&(rows, cols)) = sh.spill_shapes.get(&anchor) {
            if rows == 0 || cols == 0 { return None; }
            let data = (0..rows)
                .flat_map(|rr| (0..cols).map(move |cc| to_fvalue(&sh.value(r + rr, c + cc))))
                .collect();
            return Some(value::Array::new(rows as usize, cols as usize, data));
        }
        if let Some(&(r1, c1, r2, c2)) =
            sh.arrays.iter().find(|(a, _)| *a == anchor).map(|(_, r)| r)
        {
            let rows = (r2 - r1 + 1) as usize;
            let cols = (c2 - c1 + 1) as usize;
            let data = (r1..=r2)
                .flat_map(|rr| (c1..=c2).map(move |cc| to_fvalue(&sh.value(rr, cc))))
                .collect();
            return Some(value::Array::new(rows, cols, data));
        }
        None
    }

    /// `A1#` 的统一取值：只有真正的动态数组/已物化数组才是数组；
    /// 普通标量、外部书名和错误都按 Excel 语义传播/报 #REF!，不能把标量
    /// 当成 1×1 数组（否则 BYROW(A1#,...) 会错误返回标量）。
    fn spill_value(&mut self, si: usize, r: u32, c: u32) -> FValue {
        if self.wb.sheets.get(si).is_none() {
            return FValue::Error(ErrKind::Ref);
        }
        let key = (si, r, c);
        let outside_scope = self.scope.as_ref().is_some_and(|scope| !scope.contains(&key));
        if outside_scope {
            if let Some(a) = self.materialized_spill(si, r, c) {
                return FValue::Array(std::rc::Rc::new(a));
            }
            return FValue::Error(ErrKind::Ref);
        }
        match self.cell_value(si, r, c) {
            FValue::Array(a) => FValue::Array(a),
            FValue::Error(e) => FValue::Error(e),
            _ => FValue::Error(ErrKind::Ref),
        }
    }

    /// 隐式交集：单格引用直接取值；单列区域按公式行取同列，单行区域
    /// 按公式列取同行；二维区域必须同时命中公式行和公式列。区域与公式
    /// 没有唯一交点时按 Excel 返回 #VALUE!。`A1#` 的 `@` 目前取数组左上角，
    /// 与 Excel 365 对动态数组的处理一致。
    fn implicit_value(&mut self, n: &Node, si: usize) -> FValue {
        match n {
            Node::Range(a, b) => self.implicit_range(a, b, si),
            // 单格引用 / Spill / 普通表达式：数组按左上角标量处理；这
            // 与 `@A1` 在任意位置都返回 A1、`@A1#` 返回锚点值一致。
            _ => self.eval_node(n, si).scalar(),
        }
    }

    fn implicit_range(&mut self, a: &RefExpr, b: &RefExpr, si: usize) -> FValue {
        // 外部工作簿没有本簿的几何边界；退回普通区域首值。
        if a.book.is_some() || b.book.is_some() {
            return match self.range_values(a, b, si) {
                Ok(Arg::Range { values, .. }) => values.first().cloned().unwrap_or(FValue::Empty),
                Ok(Arg::Val(v)) => v.scalar(),
                Err(e) => FValue::Error(e),
            };
        }
        let idx = match self.sheet_index(a.sheet.as_deref().or(b.sheet.as_deref()), si) {
            Ok(i) => i,
            Err(e) => return FValue::Error(e),
        };
        let (mut r1, mut c1) = (a.row.min(b.row).max(0) as u32, a.col.min(b.col).max(0) as u32);
        let (mut r2, mut c2) = (a.row.max(b.row).max(0) as u32, a.col.max(b.col).max(0) as u32);
        let (max_r, max_c) = self.sheet_bounds(idx);
        if r1 == 0 && r2 == 1_048_575 {
            r2 = r2.min(max_r);
        }
        if c1 == 0 && c2 == 16_383 {
            c2 = c2.min(max_c);
        }
        if r1 > r2 || c1 > c2 {
            return FValue::Error(ErrKind::Value);
        }
        let (fr, fc) = (self.cur.0 as i64, self.cur.1 as i64);
        let target = if r1 == r2 && c1 == c2 {
            Some((r1, c1))
        } else if r1 == r2 {
            // 单行区域：Excel 的隐式交集按公式列取该行对应列，
            // 不要求公式与区域同一行（`@A1:C1` 在 B2/B4 都取 B1）。
            if fc >= c1 as i64 && fc <= c2 as i64 {
                Some((r1, fc as u32))
            } else {
                None
            }
        } else if c1 == c2 {
            // 单列区域：公式必须与区域同行（同一行），列不限。
            if fr >= r1 as i64 && fr <= r2 as i64 {
                Some((fr as u32, c1))
            } else {
                None
            }
        } else if fr >= r1 as i64 && fr <= r2 as i64 && fc >= c1 as i64 && fc <= c2 as i64 {
            Some((fr as u32, fc as u32))
        } else {
            None
        };
        match target {
            Some((r, c)) => self.cell_value(idx, r, c).scalar(),
            None => FValue::Error(ErrKind::Value),
        }
    }

    fn range_values(&mut self, a: &RefExpr, b: &RefExpr, cur: usize) -> Result<Arg, ErrKind> {
        if a.book.is_some() || b.book.is_some() {
            return self.link_range(a, b);
        }
        let si = self.sheet_index(a.sheet.as_deref(), cur)?;
        let (mut r1, mut c1) = (a.row.min(b.row).max(0) as u32, a.col.min(b.col).max(0) as u32);
        let (mut r2, mut c2) = (a.row.max(b.row).max(0) as u32, a.col.max(b.col).max(0) as u32);
        // 整列/整行引用按表的实际使用范围裁剪，避免展开 104 万空单元格。
        // `XFD` / `1048576` 是整行/整列的展开哨兵；即使面积还没到 20 万，
        // 1:1 这类整行也不能逐格访问 16384 列。
        let (max_r, max_c) = self.sheet_bounds(si);
        let full_cols = c1 == 0 && c2 == 16_383;
        let full_rows = r1 == 0 && r2 == 1_048_575;
        if full_cols {
            c2 = c2.min(max_c);
        }
        if full_rows {
            r2 = r2.min(max_r);
        }
        if r1 > r2 || c1 > c2 {
            return Ok(Arg::Range {
                values: std::rc::Rc::new(Vec::new()),
                rows: 0,
                cols: 0,
            });
        }
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
                        self.range_reads += 1;
                        vals.push(self.cell_value(si, r, c).scalar());
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
                // 作用域：当前表的局部名称优先于工作簿名称（Excel 同名规则）
                let hit = {
                    let mut local = None;
                    let mut global = None;
                    for (nm, sh, body) in self.wb.names.iter() {
                        if !nm.eq_ignore_ascii_case(n) {
                            continue;
                        }
                        match *sh {
                            Some(s) if s == si => {
                                local = Some((*sh, body.clone()));
                                break;
                            }
                            Some(_) => {}
                            None => global = Some((*sh, body.clone())),
                        }
                    }
                    local.or(global)
                };
                if let Some((sh, body)) = hit {
                    let si2 = sh.unwrap_or(si);
                    let text = body.trim_start_matches('=').to_string();
                    if !text.is_empty() && !text.eq_ignore_ascii_case(n) {
                        let expanded =
                            crate::table::expand_structured_at(self.wb, si2, Some(self.cur.0), Some(self.cur.1), &text).unwrap_or(text);
                        if let Ok(node) = parse(&expanded) {
                            return self.eval_node(&node, si2);
                        }
                    }
                }
                FValue::Error(ErrKind::Name)
            }
            Node::ScopedName { book, sheet, name } => {
                if let Some(bk) = book.as_deref() {
                    let Some(li) = crate::xlinks::resolve_book(self.wb, bk) else {
                        return FValue::Error(ErrKind::Ref);
                    };
                    return self.eval_ext_defined_name(li, sheet.as_deref(), name, si);
                }
                if let Some(sh) = sheet.as_deref() {
                    if let Ok(si2) = self.sheet_index(Some(sh), si) {
                        let hit = self
                            .wb
                            .names
                            .iter()
                            .find(|(nm, s, _)| nm.eq_ignore_ascii_case(name) && *s == Some(si2))
                            .or_else(|| {
                                self.wb
                                    .names
                                    .iter()
                                    .find(|(nm, s, _)| nm.eq_ignore_ascii_case(name) && s.is_none())
                            })
                            .map(|(_, _, body)| body.clone());
                        if let Some(body) = hit {
                            let text = body.trim_start_matches('=').to_string();
                            if !text.is_empty() && self.depth < 200 {
                                let expanded = crate::table::expand_structured_at(
                                    self.wb,
                                    si2,
                                    Some(self.cur.0),
                                    Some(self.cur.1),
                                    &text,
                                )
                                .unwrap_or(text);
                                if let Ok(node) = parse(&expanded) {
                                    self.depth += 1;
                                    let v = self.eval_node(&node, si2);
                                    self.depth -= 1;
                                    return v;
                                }
                            }
                        }
                        return FValue::Error(ErrKind::Name);
                    }
                    if let Some(li) = crate::xlinks::resolve_book(self.wb, sh) {
                        return self.eval_ext_defined_name(li, None, name, si);
                    }
                }
                FValue::Error(ErrKind::Name)
            }
            Node::Number(n) => FValue::Number(*n),
            Node::Text(s) => FValue::Text(s.as_str().into()),
            Node::Bool(b) => FValue::Bool(*b),
            Node::Err(e) => FValue::Error(*e),
            Node::Implicit(e) => self.implicit_value(e, si),
            Node::Ref(r) => {
                if r.sheet2.is_some() {
                    if r.book.is_some() {
                        return self.link_three_d_cell(r);
                    }
                    let idxs = match self.three_d_indices(r) {
                        Ok(v) => v,
                        Err(e) => return FValue::Error(e),
                    };
                    let mut vals = Vec::with_capacity(idxs.len());
                    for idx in idxs {
                        vals.push(self.cell_value(idx, r.row.max(0) as u32, r.col.max(0) as u32).scalar());
                    }
                    return FValue::Array(std::rc::Rc::new(value::Array::new(vals.len(), 1, vals)));
                }
                if r.book.is_some() {
                    return self.link_cell(r);
                }
                match self.sheet_index(r.sheet.as_deref(), si) {
                    Ok(idx) if r.row >= 0 && r.col >= 0 => {
                        self.cell_value(idx, r.row as u32, r.col as u32).scalar()
                    }
                    _ => FValue::Error(ErrKind::Ref),
                }
            }
            Node::Spill(r) => {
                if r.book.is_some() {
                    return FValue::Error(ErrKind::Ref);
                }
                match self.sheet_index(r.sheet.as_deref(), si) {
                    Ok(idx) if r.row >= 0 && r.col >= 0 => {
                        self.spill_value(idx, r.row as u32, r.col as u32)
                    }
                    _ => FValue::Error(ErrKind::Ref),
                }
            }
            Node::Range(a, b) => {
                if a.sheet2.is_some() || b.sheet2.is_some() {
                    if a.book.is_some() || b.book.is_some() {
                        return match self.link_three_d_range(a, b) {
                            Ok(v) => v,
                            Err(e) => FValue::Error(e),
                        };
                    }
                    let rep = if a.sheet2.is_some() { a } else { b };
                    let idxs = match self.three_d_indices(rep) {
                        Ok(v) => v,
                        Err(e) => return FValue::Error(e),
                    };
                    let mut aa = a.clone();
                    let mut bb = b.clone();
                    for r in [&mut aa, &mut bb] {
                        r.sheet = None;
                        r.sheet2 = None;
                        r.book = None;
                    }
                    let mut vals = Vec::new();
                    let mut rows = 0usize;
                    let mut cols = 0usize;
                    for idx in idxs {
                        match self.range_values(&aa, &bb, idx) {
                            Ok(Arg::Range { values, rows: rr, cols: cc }) => {
                                rows += rr;
                                cols = cols.max(cc);
                                vals.extend((*values).iter().cloned());
                            }
                            Ok(Arg::Val(v)) => {
                                rows += 1;
                                cols = cols.max(1);
                                vals.push(v.scalar());
                            }
                            Err(e) => return FValue::Error(e),
                        }
                    }
                    if vals.is_empty() {
                        return FValue::Error(ErrKind::Ref);
                    }
                    FValue::Array(std::rc::Rc::new(value::Array::new(rows.max(1), cols.max(1), vals)))
                } else {
                    match self.range_values(a, b, si) {
                        Ok(Arg::Range { values, rows, cols }) => {
                            if rows == 1 && cols == 1 {
                                values.first().cloned().unwrap_or(FValue::Empty)
                            } else {
                                FValue::Array(std::rc::Rc::new(value::Array::new(
                                    rows,
                                    cols,
                                    (*values).clone(),
                                )))
                            }
                        }
                        Ok(_) => FValue::Error(ErrKind::Value),
                        Err(e) => FValue::Error(e),
                    }
                }
            }
            Node::Unary(neg, e) => {
                // 一元也走二维块：`=-SEQUENCE(2,2)` 必须得到 2×2 而不是 #VALUE!
                let (rows, cols, vals) = self.operand_block(e, si);
                let mut out = Vec::with_capacity(vals.len());
                for v in &vals {
                    // 逐元素：遇到错误值保留该格的错误，不能把整块坍成一个错误
                    // （`=-{1;#N/A}` 在 Excel 里是 2×1 的 [-1; #N/A]）
                    match v.to_number() {
                        Ok(n) => out.push(FValue::number(if *neg { -n } else { n })),
                        Err(err) => out.push(FValue::Error(err)),
                    }
                }
                if rows == 1 && cols == 1 {
                    return out.pop().unwrap_or(FValue::Error(ErrKind::Value));
                }
                FValue::Array(std::rc::Rc::new(value::Array::new(rows, cols, out)))
            }
            Node::Percent(e) => {
                let (rows, cols, vals) = self.operand_block(e, si);
                let mut out = Vec::with_capacity(vals.len());
                for v in &vals {
                    match v.to_number() {
                        Ok(n) => out.push(FValue::number(n / 100.0)),
                        Err(err) => out.push(FValue::Error(err)),
                    }
                }
                if rows == 1 && cols == 1 {
                    return out.pop().unwrap_or(FValue::Error(ErrKind::Value));
                }
                FValue::Array(std::rc::Rc::new(value::Array::new(rows, cols, out)))
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
                match nodes.first() {
                    Some(Node::Ref(re)) | Some(Node::Range(re, _)) => {
                        if let Some(book) = re.book.as_deref() {
                            let Some(li) = crate::xlinks::resolve_book(self.wb, book) else {
                                return FValue::Error(ErrKind::Ref);
                            };
                            let link = &self.wb.links[li];
                            let Some(slot) = crate::xlinks::sheet_slot(link, re.sheet.as_deref()) else {
                                return FValue::Error(ErrKind::Ref);
                            };
                            return FValue::number((slot + 1) as f64);
                        }
                        match self.sheet_index(re.sheet.as_deref(), si) {
                            Ok(idx) => FValue::number((idx + 1) as f64),
                            Err(e) => FValue::Error(e),
                        }
                    }
                    None => FValue::number((si + 1) as f64),
                    Some(_) => FValue::number((si + 1) as f64),
                }
            }
            Node::Func(name, nodes) if name == "SHEETS" => {
                if nodes.is_empty() {
                    return FValue::number(self.wb.sheets.len() as f64);
                }
                match nodes.first() {
                    Some(Node::Ref(re)) => {
                        if re.sheet2.is_some() {
                            if let Some(book) = re.book.as_deref() {
                                let Some(li) = crate::xlinks::resolve_book(self.wb, book) else {
                                    return FValue::Error(ErrKind::Ref);
                                };
                                return match self.link_three_d_slots(&self.wb.links[li], re) {
                                    Ok(slots) => FValue::number(slots.len() as f64),
                                    Err(e) => FValue::Error(e),
                                };
                            }
                            return match self.three_d_indices(re) {
                                Ok(idxs) => FValue::number(idxs.len() as f64),
                                Err(e) => FValue::Error(e),
                            };
                        }
                        if let Some(book) = re.book.as_deref() {
                            let Some(li) = crate::xlinks::resolve_book(self.wb, book) else {
                                return FValue::Error(ErrKind::Ref);
                            };
                            return match crate::xlinks::sheet_slot(&self.wb.links[li], re.sheet.as_deref()) {
                                Some(_) => FValue::number(1.0),
                                None => FValue::Error(ErrKind::Ref),
                            };
                        }
                        match self.sheet_index(re.sheet.as_deref(), si) {
                            Ok(_) => FValue::number(1.0),
                            Err(e) => FValue::Error(e),
                        }
                    }
                    Some(Node::Range(a, b)) => {
                        if a.sheet2.is_some() || b.sheet2.is_some() {
                            let rep = if a.sheet2.is_some() { a } else { b };
                            if let Some(book) = rep.book.as_deref() {
                                let Some(li) = crate::xlinks::resolve_book(self.wb, book) else {
                                    return FValue::Error(ErrKind::Ref);
                                };
                                return match self.link_three_d_slots(&self.wb.links[li], rep) {
                                    Ok(slots) => FValue::number(slots.len() as f64),
                                    Err(e) => FValue::Error(e),
                                };
                            }
                            return match self.three_d_indices(rep) {
                                Ok(idxs) => FValue::number(idxs.len() as f64),
                                Err(e) => FValue::Error(e),
                            };
                        }
                        if let Some(book) = a.book.as_deref().or(b.book.as_deref()) {
                            let Some(li) = crate::xlinks::resolve_book(self.wb, book) else {
                                return FValue::Error(ErrKind::Ref);
                            };
                            let sh = a.sheet.as_deref().or(b.sheet.as_deref());
                            return match crate::xlinks::sheet_slot(&self.wb.links[li], sh) {
                                Some(_) => FValue::number(1.0),
                                None => FValue::Error(ErrKind::Ref),
                            };
                        }
                        match self.sheet_index(a.sheet.as_deref(), si) {
                            Ok(_) => FValue::number(1.0),
                            Err(e) => FValue::Error(e),
                        }
                    }
                    _ => FValue::Error(ErrKind::Value),
                }
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
                        if data.len() == 1 {
                            if let FValue::Error(e) = data[0] {
                                return FValue::Error(e);
                            }
                        }
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
                        if data.len() == 1 {
                            if let FValue::Error(e) = data[0] {
                                return FValue::Error(e);
                            }
                        }
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
                    "contents" => self.cell_value(idx, r, c).scalar(),
                    "type" => match self.cell_value(idx, r, c).scalar() {
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
                                    self.cell_value(idx, re.row as u32, re.col as u32).scalar()
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
                let a = RefExpr { book: None, sheet: base.0.clone(), sheet2: None, row: r0, col: c0, row_abs: false, col_abs: false };
                let b = RefExpr {
                    book: None,
                    sheet: base.0,
                    sheet2: None,
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
                // 查找键本身是错误值：Excel 直接把该错误抛出去（VLOOKUP(#DIV/0!,…) = #DIV/0!）
                if let FValue::Error(e) = key {
                    return FValue::Error(e);
                }
                // 文本查找键带通配符（* ? ~）时交给通用实现，哈希索引只支持等值
                if let FValue::Text(t) = &key {
                    if crate::formula::functions::is_wildcard(t) {
                        return self.call_generic(name, nodes, si);
                    }
                }
                let literal = |n: &Node| -> Option<(RefExpr, RefExpr)> {
                    match n {
                        Node::Range(a, b)
                            if a.book.is_none()
                                && b.book.is_none()
                                && a.sheet2.is_none()
                                && b.sheet2.is_none() =>
                        {
                            Some((a.clone(), b.clone()))
                        }
                        Node::Ref(a) if a.book.is_none() && a.sheet2.is_none() => {
                            Some((a.clone(), a.clone()))
                        }
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
                if name == "VLOOKUP" || name == "HLOOKUP" {
                    let vertical = name == "VLOOKUP";
                    let col_idx = match nodes.get(2) {
                        Some(n) => self.eval_node(n, si).to_number().unwrap_or(0.0) as usize,
                        None => return FValue::Error(ErrKind::Value),
                    };
                    let approx = match nodes.get(3) {
                        Some(n) => self.eval_node(n, si).to_bool().unwrap_or(true),
                        None => true,
                    };
                    let (max_r, max_c) = self.sheet_bounds(idx);
                    let (r1, c1) = (la.row.min(lb.row), la.col.min(lb.col));
                    let mut br = la.row.max(lb.row);
                    let mut bc = la.col.max(lb.col);
                    if c1 == 0 && bc == 16_383 {
                        bc = bc.min(max_c as i64);
                    }
                    if r1 == 0 && br == 1_048_575 {
                        br = br.min(max_r as i64);
                    }
                    if br < r1 || bc < c1 {
                        return FValue::Error(ErrKind::Ref);
                    }
                    let limit = if vertical {
                        (bc - c1 + 1) as usize
                    } else {
                        (br - r1 + 1) as usize
                    };
                    if col_idx == 0 || col_idx > limit {
                        return FValue::Error(ErrKind::Ref);
                    }
                    // 关键内存优化：VLOOKUP 只在首列查找、HLOOKUP 只在首行查找，目标返回值由 `cell_value`
                    // 单格按需读取；只物化并索引首列/首行（而非整张 26 列大表），内存峰值降低 N_cols 倍，
                    // 且 `VLOOKUP(..., A:B, 2, 0)`..`VLOOKUP(..., A:Z, 26, 0)` 共享同一份哈希索引与列缓存！
                    let mut sa = la.clone();
                    let mut sb = lb.clone();
                    if vertical {
                        sa.col = c1;
                        sb.col = c1;
                    } else {
                        sa.row = r1;
                        sb.row = r1;
                    }
                    let (s_rows, s_cols, data) = self.cached_grid(idx, &sa, &sb);
                    let n = if vertical { s_rows } else { s_cols };
                    if n == 0 {
                        return FValue::Error(ErrKind::Na);
                    }
                    let mut best: Option<usize> = None;
                    if !approx {
                        let ix = self.lookup_hash(idx, &sa, &sb, s_cols, s_rows, vertical);
                        best = ix.get(&key_norm(&key)).copied();
                    } else {
                        for i in 0..n {
                            let cell = &data[i];
                            match cell.compare(&key) {
                                // 重复值：Excel 取最后一个相等的行，不能遇到就停
                                Ok(std::cmp::Ordering::Equal) => best = Some(i),
                                Ok(std::cmp::Ordering::Less) => best = Some(i),
                                Ok(std::cmp::Ordering::Greater) => break,
                                _ => {}
                            }
                        }
                    }
                    return match best {
                        Some(i) => {
                            let (r, c) = if vertical {
                                (r1 + i as i64, c1 + col_idx as i64 - 1)
                            } else {
                                (r1 + col_idx as i64 - 1, c1 + i as i64)
                            };
                            self.cell_value(idx, r as u32, c as u32).scalar()
                        }
                        None => FValue::Error(ErrKind::Na),
                    };
                }

                let (rows, cols, data) = self.cached_grid(idx, &la, &lb);
                if rows == 0 || cols == 0 {
                    return FValue::Error(ErrKind::Ref);
                }
                let vertical = !(rows == 1 && cols > 1);
                let n = if vertical { rows } else { cols };

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
                        self.cell_value(ridx, r as u32, c as u32).scalar()
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
                        Node::Range(a, b)
                            if a.book.is_none()
                                && b.book.is_none()
                                && a.sheet2.is_none()
                                && b.sheet2.is_none() =>
                        {
                            Some((a.clone(), b.clone()))
                        }
                        Node::Ref(a) if a.book.is_none() && a.sheet2.is_none() => {
                            Some((a.clone(), a.clone()))
                        }
                        _ => None,
                    }
                };
                let simple = matches!(name.as_str(), "SUM" | "COUNTA" | "AVERAGE" | "MAX" | "MIN");
                let ok_arity = if simple { nodes.len() == 1 } else { nodes.len() >= 2 };
                if ok_arity {
                    if let Some((ra, rb)) = literal(&nodes[0]) {
                        // 三维引用不是单张表的矩形，不能走单表区域聚合缓存。
                        if ra.sheet2.is_some() || rb.sheet2.is_some() {
                            return self.call_generic(name, nodes, si);
                        }
                        // 外部引用不走这块区域缓存：交给通用路径按 links 求值
                        if ra.book.is_some() || rb.book.is_some() {
                            return self.call_generic(name, nodes, si);
                        }
                        let si2 = match self.sheet_index(ra.sheet.as_deref(), si) {
                            Ok(i) => i,
                            Err(e) => return FValue::Error(e),
                        };
                        let crit = if simple {
                            String::new()
                        } else {
                            // 条件表达式（如 A5）的默认工作表必须是公式所在的当前表 si，绝不能误落到被聚合表 si2！
                            match self.eval_node(&nodes[1], si).to_text() {
                                Ok(value) => value,
                                Err(error) => return FValue::Error(error),
                            }
                        };
                        let sum_spec = if name == "SUMIF" {
                            let effective = sumif_amount_range(nodes);
                            match effective.as_ref().or(nodes.get(2)) {
                                None => None,
                                Some(n2) => match literal(n2) {
                                    Some((sa, sb))
                                        if sa.sheet2.is_none()
                                            && sb.sheet2.is_none()
                                            && sa.book.is_none()
                                            && sb.book.is_none() =>
                                    {
                                        match self.sheet_index(sa.sheet.as_deref(), si) {
                                            Ok(sidx) => Some((sidx, sa, sb)),
                                            Err(e) => return FValue::Error(e),
                                        }
                                    }
                                    _ => return self.call_generic(name, nodes, si),
                                },
                            }
                        } else {
                            None
                        };
                        let cache_crit = if let Some((sidx, ref sa, ref sb)) = sum_spec {
                            format!(
                                "{crit}\0{sidx}:{}:{}:{}:{}",
                                sa.row.min(sb.row),
                                sa.col.min(sb.col),
                                sa.row.max(sb.row),
                                sa.col.max(sb.col)
                            )
                        } else {
                            crit.clone()
                        };
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
                            cache_crit,
                        );
                        if let Some(v) = self.agg_cache.get(&key) {
                            self.agg_hits += 1;
                            return v.clone();
                        }
                        self.agg_miss += 1;
                        let t0 = std::time::Instant::now();
                        let (rows, cols, data) = self.cached_grid(si2, &ra, &rb);
                        let out = match name.as_str() {
                            "SUM" | "COUNTA" | "AVERAGE" | "MAX" | "MIN" => {
                                let range = Arg::Range { values: data.clone(), rows, cols };
                                functions::call(name, &[range])
                            }
                            "COUNTIF" => {
                                let c = functions::Crit::parse(&crit);
                                let n = data.iter().filter(|v| c.matches(v)).count();
                                FValue::number(n as f64)
                            }
                            _ => {
                                // SUMIF：第三个参数是和区域，可能跨表
                                let c = functions::Crit::parse(&crit);
                                let amounts = sum_spec.map(|(sidx, sa, sb)| self.cached_grid(sidx, &sa, &sb));
                                let mut t = 0.0;
                                for (i, v) in data.iter().enumerate() {
                                    if c.matches(v) {
                                        let amount = match &amounts {
                                            Some((amount_rows, amount_cols, values)) => {
                                                let (row, col) = (i / cols, i % cols);
                                                if row >= *amount_rows || col >= *amount_cols { continue; }
                                                &values[row * amount_cols + col]
                                            }
                                            None => v,
                                        };
                                        match amount {
                                            FValue::Number(n) => t += n,
                                            FValue::Error(e) => return FValue::Error(*e),
                                            _ => {}
                                        }
                                    }
                                }
                                FValue::number(t)
                            }
                        };
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
                let args = self.function_args(name, nodes, si);
                let t0 = std::time::Instant::now();
                let out = if matches!(name.as_str(), "RAND" | "RANDBETWEEN" | "RANDARRAY" | "NOW" | "TODAY") {
                    let cell = (self.cur_sheet, self.cur.0, self.cur.1);
                    let count = self.volatile_counts.entry(cell).or_default();
                    let key = (cell, *count, format!("{name}:{args:?}"));
                    *count += 1;
                    if let Some(value) = self.volatile_values.get(&key) {
                        value.clone()
                    } else {
                        let value = functions::call(name, &args);
                        self.volatile_values.insert(key, value.clone());
                        value
                    }
                } else {
                    functions::call(name, &args)
                };
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
        const LIMIT: usize = 4_000_000; // 约 128 MB 上限，抑制大表重算内存峰值
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

    /// 表的实际使用范围（通过 Store 页索引 O(pages) 获取，避免全表逐格遍历或 A:A 展开 104 万格）
    fn sheet_bounds(&mut self, si: usize) -> (u32, u32) {
        if let Some(b) = self.bounds.get(&si) {
            return *b;
        }
        let b = self
            .wb
            .sheets
            .get(si)
            .and_then(|sheet| sheet.used_bounds())
            .map(|(_, _, mr, mc)| (mr, mc))
            .unwrap_or((0, 0));
        self.bounds.insert(si, b);
        b
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
        let full_cols = a.col.min(b.col) == 0 && bc == 16_383;
        let full_rows = a.row.min(b.row) == 0 && br == 1_048_575;
        if full_cols {
            bc = bc.min(max_c as i64);
        }
        if full_rows {
            br = br.min(max_r as i64);
        }
        let area = (br - a.row.min(b.row) + 1).saturating_mul(bc - a.col.min(b.col) + 1);
        if area > 200_000 {
            br = br.min(max_r as i64);
            bc = bc.min(max_c as i64);
        }
        let (r1, c1) = (a.row.min(b.row), a.col.min(b.col));
        if br < r1 || bc < c1 {
            return (0, 0, std::rc::Rc::new(Vec::new()));
        }
        let key = (si, r1, c1, br, bc);
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
                self.range_reads += 1;
                vals.push(self.cell_value(si, r as u32, c as u32).scalar());
            }
        }
        let rc = self.cache_put(key, std::rc::Rc::new(vals));
        (rows, cols, rc)
    }

    fn drop_hidden_rows(&self, arg: Arg, r1: u32, si: usize, skip_manual: bool) -> Arg {
        let Arg::Range { values, rows, cols } = arg else { return arg };
        let sheet = &self.wb.sheets[si];
        if sheet.hidden_rows.is_empty() && (!skip_manual || sheet.hidden_rows_user.is_empty()) {
            return Arg::Range { values, rows, cols };
        }
        let mut out = Vec::with_capacity(values.len());
        let mut kept = 0;
        for i in 0..rows {
            let row = r1 + i as u32;
            if sheet.hidden_rows.contains(&row) || (skip_manual && sheet.hidden_rows_user.contains(&row)) {
                continue;
            }
            out.extend_from_slice(&values[i * cols..(i + 1) * cols]);
            kept += 1;
        }
        Arg::Range { values: std::rc::Rc::new(out), rows: kept, cols }
    }

    fn function_args(&mut self, name: &str, nodes: &[Node], si: usize) -> Vec<Arg> {
        let mut args: Vec<Arg> = Vec::with_capacity(nodes.len());
        let amounts = if name == "SUMIF" { sumif_amount_range(nodes) } else { None };
        for (i, n) in nodes.iter().enumerate() {
            let n = if i == 2 { amounts.as_ref().unwrap_or(n) } else { n };
            // Every SUBTOTAL excludes filtered rows. Codes 101..111 also exclude manual hiding.
            let visibility = match name {
                "SUBTOTAL" if i > 0 => Some(args[0].first().to_number().unwrap_or(0.0) >= 100.0),
                "AGGREGATE" if i > 1 && matches!(args[1].first().to_number(), Ok(5.0 | 6.0)) => Some(true),
                _ => None,
            };
            let refs = match n {
                Node::Range(a, b) => Some((a, b)),
                Node::Ref(a) if visibility.is_some() => Some((a, a)),
                _ => None,
            };
            if let Some((a, b)) = refs.filter(|(a, b)| a.sheet2.is_none() && b.sheet2.is_none()) {
                match self.range_values(a, b, si) {
                    Ok(mut arg) => {
                        if let Some(skip_manual) = visibility
                            && a.book.is_none() && b.book.is_none()
                            && let Ok(source) = self.sheet_index(a.sheet.as_deref(), si)
                        {
                            arg = self.drop_hidden_rows(arg, a.row.min(b.row).max(0) as u32, source, skip_manual);
                        }
                        args.push(arg);
                    }
                    Err(error) => args.push(Arg::Val(FValue::Error(error))),
                }
            } else {
                args.push(Arg::Val(self.eval_node(n, si)));
            }
        }
        args
    }

    fn call_generic(&mut self, name: &str, nodes: &[Node], si: usize) -> FValue {
        functions::call(name, &self.function_args(name, nodes, si))
    }

    fn grid_of(&mut self, n: &Node, si: usize) -> (usize, usize, Vec<FValue>) {
        match n {
            Node::Ref(re) => {
                if re.sheet2.is_some() || re.book.is_some() {
                    return match self.eval_node(n, si) {
                        FValue::Array(a) => (a.rows, a.cols, a.data.clone()),
                        FValue::Error(e) => (1, 1, vec![FValue::Error(e)]),
                        v => (1, 1, vec![v]),
                    };
                }
                match self.sheet_index(re.sheet.as_deref(), si) {
                    Ok(idx) => {
                        let v = self.cell_value(idx, re.row.max(0) as u32, re.col.max(0) as u32).scalar();
                        (1, 1, vec![v])
                    }
                    Err(_) => (0, 0, vec![]),
                }
            }
            Node::Spill(re) => {
                if re.book.is_some() {
                    return (1, 1, vec![FValue::Error(ErrKind::Ref)]);
                }
                match self.sheet_index(re.sheet.as_deref(), si) {
                    Ok(idx) => match self.spill_value(idx, re.row.max(0) as u32, re.col.max(0) as u32) {
                        FValue::Array(a) => (a.rows, a.cols, a.data.clone()),
                        FValue::Error(e) => (1, 1, vec![FValue::Error(e)]),
                        _ => (1, 1, vec![FValue::Error(ErrKind::Ref)]),
                    },
                    Err(_) => (1, 1, vec![FValue::Error(ErrKind::Ref)]),
                }
            },
            Node::Range(a, b) => {
                if a.sheet2.is_some() || b.sheet2.is_some() || a.book.is_some() || b.book.is_some() {
                    return match self.eval_node(n, si) {
                        FValue::Array(a) => (a.rows, a.cols, a.data.clone()),
                        FValue::Error(e) => (0, 0, vec![FValue::Error(e)]),
                        _ => (0, 0, vec![]),
                    };
                }
                match self.range_values(a, b, si) {
                    Ok(Arg::Range { values, rows, cols }) => (rows, cols, (*values).clone()),
                    _ => (0, 0, vec![]),
                }
            }
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
            if a.sheet2.is_none() && b.sheet2.is_none() {
                if let Ok(Arg::Range { values, .. }) = self.range_values(a, b, si) {
                    return (*values).clone();
                }
            }
        }
        let v = self.eval_node(n, si);
        // MAP/BYROW 这类返回数组的函数，聚合时需要摊平
        if let FValue::Array(a) = &v {
            return a.data.clone();
        }
        vec![v]
    }

    /// 二元运算的操作数一律当成二维块：区域/数组保留 rows×cols，标量是 1×1。
    /// 旧实现直接摊平成 Vec，长度相等就按 1×n 输出，纵向数组会被写成横的。
    fn operand_block(&mut self, n: &Node, si: usize) -> (usize, usize, Vec<FValue>) {
        if let Node::Range(a, b) = n {
            if a.sheet2.is_none() && b.sheet2.is_none() {
                if let Ok(Arg::Range { values, rows, cols }) = self.range_values(a, b, si) {
                    return (rows.max(1), cols.max(1), (*values).clone());
                }
            }
        }
        match self.eval_node(n, si) {
            FValue::Array(a) => (a.rows.max(1), a.cols.max(1), a.data.clone()),
            v => (1, 1, vec![v]),
        }
    }

    fn eval_binary(&mut self, op: BinOp, l: &Node, r: &Node, si: usize) -> FValue {
        let (lr, lc, lv) = self.operand_block(l, si);
        let (rr, rc, rv) = self.operand_block(r, si);
        if lr == 1 && lc == 1 && rr == 1 && rc == 1 {
            return bin_scalar(op, &lv[0], &rv[0]);
        }
        // Excel 的广播按两个维度分别判：相等就逐格算，一边是 1 就复制，其余为 #VALUE!。
        // 不能只比元素总数——2×2 与 1×4 元素个数一样，但 Excel 报 #VALUE!。
        let rows = if lr == rr {
            lr
        } else if lr == 1 {
            rr
        } else if rr == 1 {
            lr
        } else {
            return FValue::Error(ErrKind::Value);
        };
        let cols = if lc == rc {
            lc
        } else if lc == 1 {
            rc
        } else if rc == 1 {
            lc
        } else {
            return FValue::Error(ErrKind::Value);
        };
        let mut out = Vec::with_capacity(rows * cols);
        for i in 0..rows {
            for j in 0..cols {
                let li = (if lr == 1 { 0 } else { i }) * lc + (if lc == 1 { 0 } else { j });
                let ri = (if rr == 1 { 0 } else { i }) * rc + (if rc == 1 { 0 } else { j });
                out.push(bin_scalar(op, &lv[li], &rv[ri]));
            }
        }
        FValue::Array(std::rc::Rc::new(value::Array::new(rows, cols, out)))
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
        V::Error(e) => FValue::Error(*e),
    }
}

pub fn from_fvalue(v: &FValue) -> crate::model::Value {
    use crate::model::Value as V;
    match v {
        FValue::Empty => V::Empty,
        FValue::Number(n) => V::Number(*n),
        FValue::Text(s) => V::Text(s.as_str().into()),
        FValue::Bool(b) => V::Bool(*b),
        FValue::Error(e) => V::Error(*e),
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
// 日期体系相关的公开入口（model/style/pivot 用）
pub use functions::{
    current_date_system, date_to_serial_in, serial_to_date_in, set_current_date_system,
    with_date_system,
};

pub fn serial_to_date_pub(serial: f64) -> Option<(i32, u32, u32)> {
    functions::serial_to_date(serial)
}

pub fn parse(src: &str) -> Result<Node, ErrKind> {
    // 整行/整列引用在这里统一展开（兼容未经过 normalize_formula 的直接调用方如条件格式/数据验证），
    // 模型/文件里的原公式保持不变，求值阶段按实际使用区裁剪。
    let src = crate::expand_whole_cols(&crate::expand_whole_rows(src));
    let toks = lex(&src)?;
    let mut p = Parser { toks, pos: 0 };
    let node = p.parse_expr()?;
    if p.pos != p.toks.len() {
        return Err(ErrKind::Value);
    }
    Ok(node)
}

/// 把外部工作簿定义名称展开后的 AST 节点全部绑定到 `[book]`，使其求值走外链缓存路径。
pub(crate) fn qualify_ext_node(node: &mut Node, book: &str, default_sheet: Option<&str>) {
    let bind_ref = |r: &mut RefExpr| {
        if r.book.is_none() {
            r.book = Some(book.to_string());
            if r.sheet.is_none() {
                if let Some(ds) = default_sheet {
                    r.sheet = Some(ds.to_string());
                }
            }
        }
    };
    match node {
        Node::Ref(r) | Node::Spill(r) => bind_ref(r),
        Node::Range(a, b) => {
            bind_ref(a);
            bind_ref(b);
        }
        Node::Name(n) => {
            let nm = std::mem::take(n);
            *node = Node::ScopedName {
                book: Some(book.to_string()),
                sheet: default_sheet.map(str::to_string),
                name: nm,
            };
        }
        Node::ScopedName { book: bk, .. } => {
            if bk.is_none() {
                *bk = Some(book.to_string());
            }
        }
        Node::Implicit(inner) | Node::Unary(_, inner) | Node::Percent(inner) => {
            qualify_ext_node(inner, book, default_sheet);
        }
        Node::Binary(_, l, r) => {
            qualify_ext_node(l, book, default_sheet);
            qualify_ext_node(r, book, default_sheet);
        }
        Node::Func(_, args) => {
            for a in args {
                qualify_ext_node(a, book, default_sheet);
            }
        }
        _ => {}
    }
}

/// 识别 C11 引用范围内、当前求值器尚未实现的合法组合。
///
/// 外部工作簿三维引用现已在原生公式解析与求值中完整实现，此函数保留用于兼容外部调用接口，始终返回 `None`。
pub fn unsupported_reference_combo(src: &str) -> Option<&'static str> {
    let _ = src;
    None
}

fn array_blocked(wb: &Workbook, si: usize, r: u32, c: u32, a: &value::Array, ignore_old_spills: bool) -> bool {
    if r as u64 + a.rows as u64 > 1_048_576 || c as u64 + a.cols as u64 > 16_384 {
        return true;
    }
    for i in 0..a.rows {
        for j in 0..a.cols {
            if i == 0 && j == 0 { continue; }
            let key = CellRef::new(r + i as u32, c + j as u32);
            if wb.sheets[si].get(key.row, key.col).is_some_and(|cell| !cell.is_blank())
                && !(ignore_old_spills && wb.sheets[si].spilled.contains(&key)) {
                return true;
            }
        }
    }
    false
}

/// 把求值结果写回工作表（处理动态数组溢出与 #SPILL!）
/// 调用方在写入数组前清空旧溢出区域，已被本批次写入的格子也属于冲突。
pub fn write_back(wb: &mut Workbook, results: Vec<(usize, u32, u32, FValue)>) {
    for (si, r, c, v) in results {
            match v {
                FValue::Array(a) => {
                    // 检查溢出区域是否被别的数据占着
                    if array_blocked(wb, si, r, c, &a, false) {
                        wb.sheets[si].spill_shapes.remove(&CellRef::new(r, c));
                        if let Some(cell) = wb.sheets[si].get_mut(r, c) {
                            cell.value = from_fvalue(&FValue::Error(ErrKind::Spill));
                        }
                        continue;
                    }
                    wb.sheets[si]
                        .spill_shapes
                        .insert(CellRef::new(r, c), (a.rows as u32, a.cols as u32));
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

/// 手动模式：把受影响的表记入待计算集合，供“计算工作表”逐表清理。
pub fn mark_pending(wb: &mut Workbook, deps: &deps::Deps, seeds: &[deps::CellKey]) {
    let mut sheets: std::collections::BTreeSet<usize> =
        seeds.iter().map(|(si, _, _)| *si).collect();
    if !deps.is_empty() {
        for (si, _, _) in deps.closure(seeds) {
            sheets.insert(si);
        }
    }
    wb.calc_pending_sheets.extend(sheets);
    wb.calc_pending = !wb.calc_pending_sheets.is_empty();
}

/// 增量重算：只算受影响的公式单元格，其余用缓存值。
/// 返回实际重算的格子数。
pub fn recalc_dirty(wb: &mut Workbook, deps: &deps::Deps, seeds: &[deps::CellKey]) -> usize {
    if wb.calc_manual {
        mark_pending(wb, deps, seeds);
        return 0;
    }
    if std::env::var("SC_PROFILE").is_ok() {
        prof_reset();
    }
    if seeds.is_empty() {
        return 0;
    }
    let has_spills = wb.sheets.iter().any(|s| !s.spilled.is_empty() || !s.arrays.is_empty()
        || s.iter().any(|(_, c)| {
            // 错误值现在是类型；老的 Text("#SPILL!") 仍然认，兼容旧文件与旧缓存
            c.formula.is_some()
                && match &c.value {
                    crate::Value::Error(ErrKind::Spill) => true,
                    crate::Value::Text(t) => t.as_ref() == "#SPILL!",
                    _ => false,
                }
        }));
    if deps.requires_full_recalc() || has_spills {
        // 用户覆盖溢出格时保留输入，让数组按正常冲突规则处理。
        for &(si, r, c) in seeds {
            if let Some(sheet) = wb.sheets.get_mut(si) {
                sheet.spilled.remove(&CellRef::new(r, c));
            }
        }
        let count = wb.sheets.iter().map(|s| s.iter().filter(|(_, c)| c.formula.is_some()).count()).sum();
        recalc_all_force(wb);
        return count;
    }
    let mut dirty = deps.closure(seeds);
    // 修改的公式也必须计算，不能只算它的下游。
    for &key in seeds {
        if wb.sheets.get(key.0).and_then(|s| s.get(key.1, key.2))
            .is_some_and(|c| c.formula.is_some()) {
            dirty.insert(key);
        }
    }
    if dirty.is_empty() {
        return 0;
    }
    // 不需要拓扑排序：Calc 求值时递归拉取依赖，memo 保证每个格子只算一次
    let mut order: Vec<deps::CellKey> = dirty.iter().cloned().collect();
    order.sort_unstable();
    // 迭代模式：同一批脏格要反复算到不动点，否则环上只会停在中途（实测 1.5 而非 4/3）
    let passes = if wb.calc_iterate {
        wb.calc_max_iterations.max(1) as usize
    } else {
        1
    };
    let mut prev_nums: HashMap<deps::CellKey, f64> = HashMap::new();
    let mut last_count = 0usize;
    for pass in 0..passes {
        let mut results = Vec::with_capacity(order.len());
        {
            let mut calc = Calc::new(wb);
            calc.scope = Some(dirty.clone());
            for (si, r, c) in &order {
                let v = calc.cell_value(*si, *r, *c);
                results.push((*si, *r, *c, v));
            }
        }
        // 新公式可能第一次产生数组，此时尚无 spilled 标记。
        if results.iter().any(|(_, _, _, value)| matches!(value, FValue::Array(_))) {
            let count = wb.sheets.iter().map(|s| s.iter().filter(|(_, c)| c.formula.is_some()).count()).sum();
            recalc_all_force(wb);
            return count;
        }
        write_back(wb, results);
        last_count = order.len();
        if !wb.calc_iterate {
            break;
        }
        let mut max_delta = 0.0f64;
        for key in &order {
            let cur = wb.sheets[key.0]
                .get(key.1, key.2)
                .map(|c| match c.value {
                    crate::model::Value::Number(n) => n,
                    _ => 0.0,
                })
                .unwrap_or(0.0);
            let prev = prev_nums.get(key).copied().unwrap_or(0.0);
            max_delta = max_delta.max((cur - prev).abs());
            prev_nums.insert(*key, cur);
        }
        if pass > 0 && max_delta < wb.calc_max_change {
            break;
        }
    }
    if std::env::var("SC_PROFILE").is_ok() {
        for (n, c, t) in prof_dump().into_iter().take(6) {
            eprintln!("  [edit] {n:<12} {c:>7} 次  {t:>7.3}s");
        }
    }
    last_count
}

/// 仅即时计算 `seeds` 中本身带有公式的单元格（供提交编辑时在广播前毫秒级算出当前格结果）。
/// 若遇到需整表物化的动态数组溢出返回 `false`，普通标量公式写回后返回 `true`。
pub fn eval_formula_seeds_immediate(wb: &mut Workbook, seeds: &[deps::CellKey]) -> bool {
    if wb.calc_manual || seeds.is_empty() {
        return true;
    }
    let mut formula_seeds: Vec<deps::CellKey> = seeds
        .iter()
        .copied()
        .filter(|&(si, r, c)| {
            wb.sheets
                .get(si)
                .and_then(|s| s.get(r, c))
                .is_some_and(|cell| cell.formula.is_some())
        })
        .collect();
    if formula_seeds.is_empty() {
        return true;
    }
    formula_seeds.sort_unstable();
    formula_seeds.dedup();
    let scope: HashSet<deps::CellKey> = formula_seeds.iter().copied().collect();
    let mut results = Vec::with_capacity(formula_seeds.len());
    {
        let mut calc = Calc::new(wb);
        calc.scope = Some(scope);
        for &(si, r, c) in &formula_seeds {
            let v = calc.cell_value(si, r, c);
            if matches!(v, FValue::Array(_)) {
                return false;
            }
            results.push((si, r, c, v));
        }
    }
    write_back(wb, results);
    true
}

/// 交互式增量重算（专供 UI 编辑提交与实时协作帧调用）：
/// - 小型工作簿（≤ 1,200 格）直接走完整同步 `recalc_dirty`；
/// - 中大型工作簿先即时算出当前编辑公式格，再对静态依赖闭包施加严格数量与 12ms 时间预算，
///   超出预算或含全局动态引用/数组溢出时返回 `false`，由静默后台线程异步补齐，确保 UI 永不掉帧。
pub fn recalc_interactive(
    wb: &mut Workbook,
    deps: &deps::Deps,
    seeds: &[deps::CellKey],
    deps_ready: bool,
) -> bool {
    if wb.calc_manual {
        mark_pending(wb, deps, seeds);
        return true;
    }
    if seeds.is_empty() {
        return true;
    }
    let total_cells: usize = wb.sheets.iter().map(|s| s.len()).sum();
    if total_cells <= 1_200 {
        recalc_dirty(wb, deps, seeds);
        return true;
    }
    for &(si, r, c) in seeds {
        if let Some(sheet) = wb.sheets.get_mut(si) {
            sheet.spilled.remove(&CellRef::new(r, c));
        }
    }
    if !eval_formula_seeds_immediate(wb, seeds) {
        return false;
    }
    let has_spills_or_arrays = wb
        .sheets
        .iter()
        .any(|s| !s.spilled.is_empty() || !s.arrays.is_empty());
    if !deps_ready || deps.requires_full_recalc() || has_spills_or_arrays || wb.calc_iterate {
        return false;
    }
    let dirty = deps.closure(seeds);
    if dirty.is_empty() {
        return true;
    }
    if dirty.len() > 96 {
        return false;
    }
    let mut order: Vec<deps::CellKey> = dirty.iter().copied().collect();
    order.sort_unstable();
    let t0 = std::time::Instant::now();
    let budget = std::time::Duration::from_millis(12);
    let mut results = Vec::with_capacity(order.len());
    let mut timed_out = false;
    {
        let mut calc = Calc::new(wb);
        calc.scope = Some(dirty);
        for (idx, &(si, r, c)) in order.iter().enumerate() {
            if idx >= 4 && t0.elapsed() > budget {
                timed_out = true;
                break;
            }
            let v = calc.cell_value(si, r, c);
            if matches!(v, FValue::Array(_)) {
                return false;
            }
            results.push((si, r, c, v));
        }
    }
    if !results.is_empty() {
        write_back(wb, results);
    }
    !timed_out
}

fn clear_spills(wb: &mut Workbook) {
    for sheet in &mut wb.sheets {
        // `Sheet::set` 会把被用户覆盖的格子从 spilled 里摘掉，
        // 所以这里剩下的键一定还是上次溢出写的值，可以安全清掉。
        sheet.spill_shapes.clear();
        let old: Vec<CellRef> = sheet.spilled.drain().collect();
        for key in old {
            // set(empty) 同时记录 cleared，保存时才能删除缩小后的旧尾部。
            sheet.set(key.row, key.col, Cell::default());
        }
    }
}

fn spill_values(wb: &Workbook) -> HashMap<deps::CellKey, crate::Value> {
    wb.sheets.iter().enumerate().flat_map(|(si, sheet)| {
        sheet.spilled.iter().map(move |key| ((si, key.row, key.col), sheet.value(key.row, key.col)))
    }).collect()
}

/// 全量计算后物化数组，再计算读取溢出格的公式，直到溢出内容稳定。
/// 非收敛的溢出循环显式记为循环引用，避免把尚未稳定的数字当成结果。
pub fn recalc_all(wb: &mut Workbook) {
    if wb.calc_manual {
        wb.calc_pending_sheets = wb.sheets.iter().enumerate()
            .filter(|(_, s)| s.iter().any(|(_, c)| c.formula.is_some()))
            .map(|(i, _)| i)
            .collect();
        wb.calc_pending = !wb.calc_pending_sheets.is_empty();
        return;
    }
    recalc_all_force(wb);
}

/// 强制执行全量计算（F9 / 计算全部 / 切回自动模式）。
pub fn recalc_all_force(wb: &mut Workbook) {
    let _ = recalc_all_force_impl(wb, None, None);
}

/// 全量计算的实现：可在公式循环里检查取消标志并把进度写进两个原子计数。
/// 返回 `false` 表示被取消（此时写回还没发生，模型保持原值，并标 `calc_pending`）。
pub fn recalc_all_force_impl(
    wb: &mut Workbook,
    cancel: Option<&std::sync::atomic::AtomicBool>,
    progress: Option<(&std::sync::atomic::AtomicUsize, &std::sync::atomic::AtomicUsize)>,
) -> bool {
    use std::sync::atomic::Ordering;
    let prev_now = functions::ensure_now_snapshot_in(wb.date_system);
    // 重算不是用户编辑：记下 edited，写回后再恢复。否则未改过的表
    // （尤其是动态数组表）会被当成 dirty，保存时整表重写，_xlfn 前缀
    // 和溢出缓存都会丢，Excel 打不开/结果不对。
    let edited_before: Vec<bool> = wb.sheets.iter().map(|s| s.edited).collect();
    wb.calc_pending = false;
    wb.calc_pending_sheets.clear();
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
                    if k != anchor && sheet.get(k.row, k.col).is_none_or(|c| c.formula.is_none()) {
                        sheet.spilled.insert(k);
                    }
                }
            }
        }
    }
    // 先清掉上一轮的溢出单元格
    clear_spills(wb);

    mark!("清溢出");
    let formula_count: usize = wb.sheets.iter()
        .map(|s| s.iter().filter(|(_, c)| c.formula.is_some()).count()).sum();
    // 无环的溢出依赖链每遍至少推进一个公式；再多一遍确认稳定。
    // 迭代计算：环上按上一轮值反复推进，最多 calc_max_iterations 遍（Excel 默认 100）
    let passes = if wb.calc_iterate {
        (wb.calc_max_iterations.max(2)) as usize
    } else {
        formula_count.saturating_add(1).max(2)
    };
    let force_two = std::env::var("SC_TWOPASS").is_ok();
    let mut volatile_values = HashMap::new();
    let mut previous_arrays: HashMap<deps::CellKey, FValue> = HashMap::new();
    let mut pass_done = 0usize;
    // 迭代模式：记录环上格子每遍的数值，用来判收敛
    let mut prev_nums: HashMap<deps::CellKey, f64> = HashMap::new();
    for _pass in 0..passes {
        pass_done += 1;
        let before_spills = spill_values(wb);
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
        if let Some((done, total)) = progress {
            total.store(formulas.len(), Ordering::Relaxed);
            done.store(0, Ordering::Relaxed);
        }
        let mut results: Vec<(usize, u32, u32, FValue)> = Vec::with_capacity(formulas.len());
        {
            let mut calc = Calc::new(wb);
            calc.volatile_values = std::mem::take(&mut volatile_values);
            let profile = std::env::var("SC_PROFILE").is_ok();
            let mut slow: Vec<(f64, usize, u32, u32, String)> = Vec::new();
            for (i, (si, r, c)) in formulas.into_iter().enumerate() {
                if let Some(flag) = cancel {
                    // 每 256 个公式查一次，取消时立刻放弃本轮（尚未写回）
                    if i % 256 == 0 && flag.load(Ordering::Relaxed) {
                        drop(calc);
                        wb.calc_pending = true;
                        for (sheet, was) in wb.sheets.iter_mut().zip(edited_before.iter()) {
                            sheet.edited = *was;
                        }
                        functions::set_now_snapshot(prev_now);
                        return false;
                    }
                }
                if let Some((done, _)) = progress {
                    done.store(i, Ordering::Relaxed);
                }
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
            volatile_values = std::mem::take(&mut calc.volatile_values);
        }

        mark!("求值");
        let arrays: HashMap<deps::CellKey, FValue> = results.iter()
            .filter(|(_, _, _, v)| matches!(v, FValue::Array(_)))
            .map(|(si, r, c, v)| ((*si, *r, *c), v.clone())).collect();
        let final_results = (_pass + 1 == passes).then(|| results.clone());
        clear_spills(wb);
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
        let after_spills = spill_values(wb);
        // 普通模式：溢出块稳定即停。迭代模式不能在这里停——环上格子没有溢出，
        // 一停就只跑一遍，永远到不了不动点。
        if !wb.calc_iterate && before_spills == after_spills && (!force_two || _pass > 0) {
            break;
        }
        if let Some(mut final_results) = final_results {
            let mut unstable: HashSet<deps::CellKey> = before_spills.keys().chain(after_spills.keys())
                .filter(|key| before_spills.get(key) != after_spills.get(key)).copied().collect();
            let changing_arrays: HashSet<deps::CellKey> = previous_arrays.keys().chain(arrays.keys())
                .filter(|key| previous_arrays.get(key) != arrays.get(key)).copied().collect();
            unstable.extend(changing_arrays.iter().copied());
            let graph = deps::Deps::build(wb);
            // 无法静态解析的动态引用保守视为受影响，普通独立公式保留结果。
            unstable.extend(graph.dynamic_cells());
            let mut affected = graph.closure(&unstable.iter().copied().collect::<Vec<_>>());
            affected.extend(changing_arrays);
            affected.extend(graph.dynamic_cells());
            clear_spills(wb);
            for (si, r, c, value) in &mut final_results {
                let key = (*si, *r, *c);
                // 迭代计算时环上格子每遍都在变，它们不属于「不稳定」，而是收敛过程本身；
                // 这里只清理溢出/动态引用牵连的格子，否则收敛值会被改回 #REF!
                if affected.contains(&key) && !wb.calc_iterate {
                    *value = FValue::Error(ErrKind::Circular);
                    CYCLE_CELLS.with(|cells| { cells.borrow_mut().insert(key); });
                }
            }
            write_back(wb, final_results);
        }
        previous_arrays = arrays;
        if wb.calc_iterate {
            let mut max_delta = 0.0f64;
            let cycle_now: Vec<deps::CellKey> =
                CYCLE_CELLS.with(|cells| cells.borrow().iter().copied().collect());
            for key in cycle_now {
                let cur = wb.sheets[key.0]
                    .get(key.1, key.2)
                    .map(|c| match c.value {
                        crate::model::Value::Number(n) => n,
                        _ => 0.0,
                    })
                    .unwrap_or(0.0);
                let prev = prev_nums.get(&key).copied().unwrap_or(0.0);
                max_delta = max_delta.max((cur - prev).abs());
                prev_nums.insert(key, cur);
            }
            if _pass > 0 && max_delta < wb.calc_max_change {
                break;
            }
        }
    }
    mark!("结束");
    if phase {
        eprintln!("[recalc] 总耗时 {:.2}s（{} 遍）", t_all.elapsed().as_secs_f64(), pass_done);
    }
    if let Some((done, total)) = progress {
        let n = total.load(Ordering::Relaxed);
        done.store(n, Ordering::Relaxed);
    }
    for (sheet, was) in wb.sheets.iter_mut().zip(edited_before.iter()) {
        sheet.edited = *was;
    }
    functions::set_now_snapshot(prev_now);
    true
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

/// 与 `eval_formula` 相同，但返回区域物化实际读取的格数，供性能回归使用。
pub fn eval_formula_reads(wb: &Workbook, si: usize, formula: &str) -> (FValue, usize) {
    let raw = formula.strip_prefix('=').unwrap_or(formula).to_string();
    let expanded = crate::table::expand_structured(wb, si, &raw).unwrap_or(raw);
    let normalized = crate::normalize_formula(&expanded);
    match parse(&normalized) {
        Ok(node) => {
            let mut calc = Calc::new(wb);
            let v = calc.eval_node(&node, si);
            let reads = calc.range_reads();
            (v, reads)
        }
        Err(e) => (FValue::Error(e), 0),
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

/// 只强制重算某张工作表：手动模式下“计算工作表”用。
/// 不把其它表的无关易失函数拉进来。
pub fn recalc_sheet(wb: &mut Workbook, sheet: usize) -> usize {
    if sheet >= wb.sheets.len() {
        return 0;
    }
    let prev_now = functions::ensure_now_snapshot_in(wb.date_system);
    let mut seeds: Vec<deps::CellKey> = wb.sheets[sheet]
        .iter()
        .filter(|(_, c)| c.formula.is_some())
        .map(|(k, _)| (sheet, k.row, k.col))
        .collect();
    // 文件里的 t="array" ref：先把旧结果区域登记为 spill，下面才能安全清掉，
    // 否则第二次计算会被自己上次的结果挡住。
    for (anchor, (r1, c1, r2, c2)) in wb.sheets[sheet].arrays.clone() {
        for r in r1..=r2 {
            for c in c1..=c2 {
                let k = CellRef::new(r, c);
                if k != anchor
                    && wb.sheets[sheet].get(k.row, k.col).is_none_or(|c| c.formula.is_none())
                {
                    wb.sheets[sheet].spilled.insert(k);
                }
            }
        }
    }
    let count = seeds.len();
    let scope: std::collections::HashSet<deps::CellKey> = seeds.iter().copied().collect();
    let mut volatile_values = std::collections::HashMap::new();
    // 只重算目标表；其它表的 spill 和缓存一格不碰。
    // 先清一次旧 spill（否则扩大数组会被旧尾格挡住），之后每遍都先读上一遍
    // 写下的 spill 值，再清空并写新结果；这样依赖尾格的公式能追上。
    clear_spills_sheet(wb, sheet);
    let passes = count.saturating_add(1).max(2);
    for pass in 0..passes {
        let before = sheet_spill_values(wb, sheet);
        let results = {
            let mut calc = Calc::new(wb);
            calc.scope = Some(scope.clone());
            calc.volatile_values = std::mem::take(&mut volatile_values);
            let out: Vec<_> = seeds
                .iter()
                .map(|(si, r, c)| (*si, *r, *c, calc.cell_value(*si, *r, *c)))
                .collect();
            volatile_values = std::mem::take(&mut calc.volatile_values);
            out
        };
        clear_spills_sheet(wb, sheet);
        write_back(wb, results);
        if before == sheet_spill_values(wb, sheet) && pass > 0 {
            break;
        }
    }
    wb.calc_pending_sheets.remove(&sheet);
    wb.calc_pending = !wb.calc_pending_sheets.is_empty();
    functions::set_now_snapshot(prev_now);
    count
}

fn sheet_spill_values(wb: &Workbook, sheet: usize) -> std::collections::BTreeMap<CellRef, crate::Value> {
    wb.sheets[sheet]
        .spilled
        .iter()
        .map(|k| (*k, wb.sheets[sheet].value(k.row, k.col)))
        .collect()
}

fn clear_spills_sheet(wb: &mut Workbook, sheet: usize) {
    wb.sheets[sheet].spill_shapes.clear();
    let old: Vec<CellRef> = wb.sheets[sheet].spilled.drain().collect();
    for key in old {
        wb.sheets[sheet].set(key.row, key.col, Cell::default());
    }
}

#[cfg(test)]
mod calc_mode_tests {
    use super::*;
    use crate::model::{Cell, Sheet, Workbook};

    fn book() -> Workbook {
        let mut wb = Workbook::new_blank();
        wb.sheets[0].set(0, 0, Cell { value: crate::Value::Number(1.0), formula: None });
        wb.sheets[0].set(0, 1, Cell {
            value: crate::Value::Empty,
            formula: Some("=A1*2".into()),
        });
        wb
    }

    #[test]
    fn manual_mode_keeps_cached_values_until_forced() {
        let mut wb = book();
        recalc_all_force(&mut wb);
        assert_eq!(wb.sheets[0].value(0, 1), crate::Value::Number(2.0));
        wb.calc_manual = true;
        wb.sheets[0].set(0, 0, Cell { value: crate::Value::Number(5.0), formula: None });
        let deps = deps::Deps::build(&wb);
        assert_eq!(recalc_dirty(&mut wb, &deps, &[(0, 0, 0)]), 0);
        assert_eq!(wb.sheets[0].value(0, 1), crate::Value::Number(2.0), "手动模式保留旧值");
        assert!(wb.calc_pending, "必须标记待计算");
        recalc_all(&mut wb);
        assert_eq!(wb.sheets[0].value(0, 1), crate::Value::Number(2.0), "recalc_all 在手动模式也不能偷算");
        recalc_all_force(&mut wb);
        assert_eq!(wb.sheets[0].value(0, 1), crate::Value::Number(10.0));
        assert!(!wb.calc_pending);
    }

    #[test]
    fn calculate_sheet_does_not_touch_unrelated_volatile_formula() {
        let mut wb = book();
        let mut other = Sheet::new("另一表");
        other.set(0, 0, Cell {
            value: crate::Value::Number(999.0),
            formula: Some("=RAND()".into()),
        });
        wb.sheets.push(other);
        let n = recalc_sheet(&mut wb, 0);
        assert!(n >= 1, "至少算到第一张表的公式（依赖格可能一并计算）");
        assert_eq!(wb.sheets[0].value(0, 1), crate::Value::Number(2.0));
        assert_eq!(wb.sheets[1].value(0, 0), crate::Value::Number(999.0), "无关易失函数不能重算");
    }

    #[test]
    fn calc_mode_roundtrips_through_xlsx_calc_pr() {
        let dir = std::env::temp_dir().join(format!("sc_calc_mode_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let manual = dir.join("manual.xlsx");
        let auto = dir.join("auto.xlsx");
        let mut wb = book();
        wb.calc_manual = true;
        wb.calc_pending = true;
        crate::xlsx::save_atomic(&manual, &wb).unwrap();
        let (loaded, diag) = crate::xlsx::load_diag(&manual).unwrap();
        assert!(diag.is_complete(), "{:?}", diag);
        assert!(loaded.calc_manual, "保存后重开仍是手动计算");
        assert!(!loaded.calc_pending, "加载不伪造待计算状态");
        wb.calc_manual = false;
        crate::xlsx::save_atomic(&auto, &wb).unwrap();
        assert!(!crate::patch::read_calc_manual(&auto), "自动模式不写 manual");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod recalc_cancel_tests {
    use super::*;
    use crate::model::{Cell, Workbook};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    fn chain(n: u32) -> Workbook {
        let mut wb = Workbook::new_blank();
        for r in 0..n {
            wb.sheets[0].set(r, 0, Cell { value: crate::Value::Number(r as f64), formula: None });
            wb.sheets[0].set(r, 1, Cell {
                value: crate::Value::Empty,
                formula: Some(format!("=A{}+1", r + 1).into()),
            });
        }
        wb
    }

    #[test]
    fn cancel_stops_before_writeback_and_marks_pending() {
        let mut wb = chain(50_000);
        let cancel = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicUsize::new(0));
        let total = Arc::new(AtomicUsize::new(0));
        let c2 = cancel.clone();
        // 立刻取消：必须在写回之前退出，模型里的 B 列仍是空值
        c2.store(true, Ordering::Relaxed);
        let ok = recalc_all_force_impl(
            &mut wb,
            Some(&cancel),
            Some((&done, &total)),
        );
        assert!(!ok, "取消后仍报告完成");
        assert!(wb.calc_pending, "取消后没有标 calc_pending，界面不会提示需重算");
        assert!(total.load(Ordering::Relaxed) > 0, "没有写入总进度");
        let b1 = wb.sheets[0].get(0, 1).map(|c| c.value.clone());
        assert!(
            matches!(b1, Some(crate::Value::Empty) | None),
            "取消后仍写回了部分结果：{b1:?}"
        );
    }

    #[test]
    fn full_run_after_cancel_computes_everything() {
        let mut wb = chain(2_000);
        let cancel = Arc::new(AtomicBool::new(true));
        let ok = recalc_all_force_impl(&mut wb, Some(&cancel), None);
        assert!(!ok);
        // 恢复：不取消再跑一次，结果必须完全正确
        cancel.store(false, Ordering::Relaxed);
        let done = Arc::new(AtomicUsize::new(0));
        let total = Arc::new(AtomicUsize::new(0));
        let ok = recalc_all_force_impl(&mut wb, Some(&cancel), Some((&done, &total)));
        assert!(ok);
        assert_eq!(done.load(Ordering::Relaxed), total.load(Ordering::Relaxed));
        let last = wb.sheets[0].get(1_999, 1).unwrap().value.clone();
        assert_eq!(last, crate::Value::Number(2_000.0));
        assert!(!wb.calc_pending);
    }
}

#[cfg(test)]
mod calc_sheet_tests {
    use super::*;
    use crate::model::{Cell, Sheet, Workbook};

    fn f(text: &str) -> Cell {
        Cell { value: crate::Value::Empty, formula: Some(text.into()) }
    }

    #[test]
    fn recalc_does_not_mark_sheet_edited() {
        let mut wb = Workbook::new_blank();
        wb.sheets[0].set(0, 0, Cell {
            value: crate::Value::Empty,
            formula: Some("=1+1".into()),
        });
        wb.sheets[0].edited = false;
        recalc_all(&mut wb);
        assert!(!wb.sheets[0].edited, "重算不是用户编辑，不能标 dirty");
        assert_eq!(wb.sheets[0].value(0, 0), crate::Value::Number(2.0));
    }

    #[test]
    fn recalc_sheet_clears_shrinking_spill_tail_and_updates_dependents() {
        let mut wb = Workbook::new_blank();
        wb.sheets[0].set(0, 0, f("=SEQUENCE(3,1)"));
        wb.sheets[0].set(0, 1, f("=A3+10"));
        let mut other = Sheet::new("易失表");
        other.set(0, 0, Cell { value: crate::Value::Number(999.0), formula: Some("=RAND()".into()) });
        wb.sheets.push(other);

        assert!(recalc_sheet(&mut wb, 0) > 0);
        assert_eq!(wb.sheets[0].value(0, 0), crate::Value::Number(1.0));
        assert_eq!(wb.sheets[0].value(1, 0), crate::Value::Number(2.0));
        assert_eq!(wb.sheets[0].value(2, 0), crate::Value::Number(3.0));
        assert_eq!(wb.sheets[0].value(0, 1), crate::Value::Number(13.0));
        assert_eq!(wb.sheets[1].value(0, 0), crate::Value::Number(999.0));
        // 同样 3 行再算一次：旧 spill 不能把自己挡住。
        recalc_sheet(&mut wb, 0);
        assert_eq!(wb.sheets[0].value(2, 0), crate::Value::Number(3.0));
        assert_eq!(wb.sheets[0].value(0, 1), crate::Value::Number(13.0));
        assert!(!matches!(wb.sheets[0].value(0, 0), crate::Value::Error(ErrKind::Spill)));

        // 缩到 2 行：旧 A3 必须清掉，B1 依赖 A3 也要跟着变。
        wb.sheets[0].set(0, 0, f("=SEQUENCE(2,1)"));
        recalc_sheet(&mut wb, 0);
        assert_eq!(wb.sheets[0].value(1, 0), crate::Value::Number(2.0));
        assert_eq!(wb.sheets[0].value(2, 0), crate::Value::Empty, "缩小的旧尾格要清空");
        assert!(!wb.sheets[0].spilled.contains(&CellRef::new(2, 0)));
        assert_eq!(wb.sheets[0].value(0, 1), crate::Value::Number(10.0));

        // 再长回 3 行：旧尾格不能挡住新溢出，也不能留 #SPILL!。
        wb.sheets[0].set(0, 0, f("=SEQUENCE(3,1)"));
        recalc_sheet(&mut wb, 0);
        assert_eq!(wb.sheets[0].value(2, 0), crate::Value::Number(3.0));
        assert_eq!(wb.sheets[0].value(0, 1), crate::Value::Number(13.0));
        assert!(!matches!(wb.sheets[0].value(0, 0), crate::Value::Error(ErrKind::Spill)));
        assert_eq!(wb.sheets[1].value(0, 0), crate::Value::Number(999.0), "无关表 RAND 不能被碰");
    }

    #[test]
    fn calculate_one_sheet_keeps_other_pending_and_old_cache() {
        let mut wb = Workbook::new_blank();
        wb.sheets[0].set(0, 0, Cell { value: crate::Value::Number(1.0), formula: None });
        wb.sheets[0].set(0, 1, f("=A1*2"));
        let mut second = Sheet::new("第二表");
        second.set(0, 0, Cell { value: crate::Value::Number(10.0), formula: None });
        second.set(0, 1, f("=A1*2"));
        wb.sheets.push(second);
        recalc_all_force(&mut wb);

        wb.calc_manual = true;
        wb.sheets[0].set(0, 0, Cell { value: crate::Value::Number(5.0), formula: None });
        wb.sheets[1].set(0, 0, Cell { value: crate::Value::Number(20.0), formula: None });
        let deps = deps::Deps::build(&wb);
        assert_eq!(recalc_dirty(&mut wb, &deps, &[(0, 0, 0)]), 0);
        assert_eq!(recalc_dirty(&mut wb, &deps, &[(1, 0, 0)]), 0);
        assert!(wb.calc_pending_sheets.contains(&0) && wb.calc_pending_sheets.contains(&1));

        recalc_sheet(&mut wb, 0);
        assert_eq!(wb.sheets[0].value(0, 1), crate::Value::Number(10.0));
        assert_eq!(wb.sheets[1].value(0, 1), crate::Value::Number(20.0), "第二表保持旧缓存");
        assert!(wb.calc_pending, "只算一张表不能清掉全簿待计算");
        assert_eq!(wb.calc_pending_sheets.iter().copied().collect::<Vec<_>>(), vec![1]);

        recalc_sheet(&mut wb, 1);
        assert_eq!(wb.sheets[1].value(0, 1), crate::Value::Number(40.0));
        assert!(!wb.calc_pending);
        assert!(wb.calc_pending_sheets.is_empty());
    }
}
