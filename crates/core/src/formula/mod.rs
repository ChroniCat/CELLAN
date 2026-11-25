//! Excel 公式：词法、语法、求值
//!
//! 语法优先级（低 → 高），与 Excel 一致：
//!   比较(= <> < > <= >=) < 连接(&) < 加减 < 乘除 < 乘方 < 一元负号 < 百分号 < 区间(:)

pub mod array;
pub(crate) mod compiled;
mod scalar_batch;
mod dependency_batch;
pub(crate) mod lookup_cache;
mod conditional_index;
mod func_id;
mod fxhash;
mod pool;
#[cfg(test)]
mod performance_tests;
use compiled::compile_at;
pub(crate) use compiled::with_node as with_compiled_node;
pub use compiled::RuleFormulas;
pub use func_id::FuncName;
use func_id::FuncId;
use fxhash::{FxBuildHasher, FxHashMap, FxHashSet, PackedCellKey};
pub mod deps;
pub mod eval_steps;
pub mod dist;
pub mod extra;
pub mod fncat;
pub mod functions;
pub mod value;

pub use value::{ErrKind, FValue};

use crate::model::{Cell, CellRef, Sheet, Workbook};
use std::borrow::Cow;
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
    Text(std::sync::Arc<str>),
    Bool(bool),
    Err(ErrKind),
    Ref(RefExpr),
    ///  spills  #：A1# 返回锚点动态数组的完整区域
    Spill(RefExpr),
    /// 隐式交集运算符 `@`：按公式所在行/列与引用区域求交
    Implicit(Box<Node>),
    Range(RefExpr, RefExpr),
    Func(FuncName, Vec<Node>),
    Unary(bool, Box<Node>), // true = 负号
    Percent(Box<Node>),
    Binary(BinOp, Box<Node>, Box<Node>),
    /// 数组常量 {1,2;3,4}
    Array(usize, usize, Vec<crate::Value>),
}

/// Excel omits referenced cells containing a nested SUBTOTAL. Check the AST
/// only for possible calls, so text literals and similarly named identifiers count normally.
fn contains_subtotal(wb: &Workbook, sheet: usize, src: &str, row: u32, col: u32) -> bool {
    fn has_call(n: &Node) -> bool {
        match n {
            Node::Func(name, args) => name.id() == FuncId::Subtotal || args.iter().any(has_call),
            Node::Unary(_, n) | Node::Implicit(n) | Node::Percent(n) => has_call(n),
            Node::Binary(_, left, right) => has_call(left) || has_call(right),
            _ => false,
        }
    }
    if !src.as_bytes().windows(8).any(|w| w.eq_ignore_ascii_case(b"SUBTOTAL")) { return false; }
    let raw = src.strip_prefix('=').unwrap_or(src);
    let expanded = if wb.sheets.iter().any(|s| !s.tables.is_empty()) && raw.contains('[') {
        crate::table::expand_structured_at(wb, sheet, Some(row), Some(col), raw)
    } else {
        None
    };
    compiled::with_shifted(expanded.as_deref().unwrap_or(raw), row, col, |node, _, _| has_call(node))
        .unwrap_or(false)
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
    let bytes = src.as_bytes();
    let mut i = 0usize;
    let mut out = Vec::with_capacity((bytes.len() / 3).max(4));
    while i < bytes.len() {
        let b0 = bytes[i];
        match b0 {
            b' ' | b'\t' | b'\n' | b'\r' => i += 1,
            b'(' => {
                out.push(Tok::LParen);
                i += 1;
            }
            b')' => {
                out.push(Tok::RParen);
                i += 1;
            }
            b',' => {
                out.push(Tok::Comma);
                i += 1;
            }
            b'{' => {
                out.push(Tok::LBrace);
                i += 1;
            }
            b'}' => {
                out.push(Tok::RBrace);
                i += 1;
            }
            b';' => {
                out.push(Tok::Semi);
                i += 1;
            }
            b':' => {
                out.push(Tok::Colon);
                i += 1;
            }
            b'!' => {
                out.push(Tok::Bang);
                i += 1;
            }
            b'[' => {
                i += 1;
                let start = i;
                while i < bytes.len() && bytes[i] != b']' {
                    i += 1;
                }
                if i >= bytes.len() {
                    return Err(ErrKind::Value);
                }
                let name = src[start..i].to_string();
                i += 1;
                out.push(Tok::Book(name));
            }
            b'\'' => {
                i += 1;
                let start = i;
                let mut escaped = false;
                loop {
                    if i >= bytes.len() {
                        return Err(ErrKind::Value);
                    }
                    if bytes[i] == b'\'' {
                        if i + 1 < bytes.len() && bytes[i + 1] == b'\'' {
                            escaped = true;
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
                let raw = &src[start..i];
                i += 1;
                let name = if escaped {
                    raw.replace("''", "'")
                } else {
                    raw.to_string()
                };
                out.push(Tok::Ident(name));
            }
            b'@' => {
                out.push(Tok::At);
                i += 1;
            }
            b'%' => {
                out.push(Tok::Percent);
                i += 1;
            }
            b'"' => {
                i += 1;
                let start = i;
                let mut escaped = false;
                loop {
                    if i >= bytes.len() {
                        return Err(ErrKind::Value);
                    }
                    if bytes[i] == b'"' {
                        if i + 1 < bytes.len() && bytes[i + 1] == b'"' {
                            escaped = true;
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
                let raw = &src[start..i];
                i += 1;
                let s = if escaped {
                    raw.replace("\"\"", "\"")
                } else {
                    raw.to_string()
                };
                out.push(Tok::Str(s));
            }
            b'<' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    out.push(Tok::Op("<="));
                    i += 2;
                } else if i + 1 < bytes.len() && bytes[i + 1] == b'>' {
                    out.push(Tok::Op("<>"));
                    i += 2;
                } else {
                    out.push(Tok::Op("<"));
                    i += 1;
                }
            }
            b'>' => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'=' {
                    out.push(Tok::Op(">="));
                    i += 2;
                } else {
                    out.push(Tok::Op(">"));
                    i += 1;
                }
            }
            b'=' => {
                out.push(Tok::Op("="));
                i += 1;
            }
            b'+' | b'-' | b'*' | b'/' | b'^' | b'&' => {
                let s = match b0 {
                    b'+' => "+",
                    b'-' => "-",
                    b'*' => "*",
                    b'/' => "/",
                    b'^' => "^",
                    _ => "&",
                };
                out.push(Tok::Op(s));
                i += 1;
            }
            b'#' => {
                let start = i;
                i += 1;
                while i < bytes.len() {
                    let b = bytes[i];
                    if b.is_ascii_alphanumeric() || matches!(b, b'/' | b'!' | b'_' | b'?') {
                        // Note: keep exact boundary as before — '?' is NOT consumed when part of #NAME? Wait: check old line 296!
                        if b == b'?' {
                            break;
                        }
                        i += 1;
                    } else if b >= 0x80 {
                        let ch = src[i..].chars().next().unwrap();
                        if ch.is_alphanumeric() {
                            i += ch.len_utf8();
                        } else {
                            break;
                        }
                    } else {
                        break;
                    }
                }
                // Check old line 296 vs #NAME?: wait — in old line 296 `?` was NOT in `is_alphanumeric() || '/' || '!' || '_'`, wait: how did #NAME? lex before?!
                // Let's check: if bytes.get(i) == Some(&b'?') and &src[start..i] eq_ignore_ascii_case("#NAME"), consume `?`? Wait: old line 296 did NOT have `?`! Keep 100% identical to old line 296!
                let lit = &src[start..i];
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
                out.push(Tok::Ident(format!("\u{0}ERR{}=", e as u8)));
            }
            _ => {
                if b0.is_ascii_digit() || (b0 == b'.' && bytes.get(i + 1).is_some_and(u8::is_ascii_digit)) {
                    let start = i;
                    while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                        i += 1;
                    }
                    if i < bytes.len() && matches!(bytes[i], b'e' | b'E') {
                        let save = i;
                        i += 1;
                        if i < bytes.len() && matches!(bytes[i], b'+' | b'-') {
                            i += 1;
                        }
                        if i < bytes.len() && bytes[i].is_ascii_digit() {
                            while i < bytes.len() && bytes[i].is_ascii_digit() {
                                i += 1;
                            }
                        } else {
                            i = save;
                        }
                    }
                    out.push(Tok::Num(src[start..i].parse().map_err(|_| ErrKind::Value)?));
                } else {
                    let ch0 = if b0 < 0x80 {
                        b0 as char
                    } else {
                        src[i..].chars().next().ok_or(ErrKind::Value)?
                    };
                    if ch0.is_alphabetic() || matches!(ch0, '_' | '$' | '\\') {
                        let start = i;
                        i += ch0.len_utf8();
                        while i < bytes.len() {
                            let nb = bytes[i];
                            if nb < 0x80 {
                                if nb.is_ascii_alphanumeric() || matches!(nb, b'_' | b'.' | b'$' | b'\\') {
                                    i += 1;
                                } else {
                                    break;
                                }
                            } else {
                                let ch = src[i..].chars().next().unwrap();
                                if ch.is_alphanumeric() {
                                    i += ch.len_utf8();
                                } else {
                                    break;
                                }
                            }
                        }
                        out.push(Tok::Ident(src[start..i].to_string()));
                    } else {
                        return Err(ErrKind::Value);
                    }
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
        // A LAMBDA returned by an expression or a sheet-qualified name can be called directly.
        if matches!(node, Node::Func(..) | Node::ScopedName { book: None, .. }) {
            while self.eat(&Tok::LParen) {
                let mut call = vec![node];
                if !self.eat(&Tok::RParen) {
                    loop {
                        call.push(self.parse_expr()?);
                        if self.eat(&Tok::RParen) { break; }
                        if !self.eat(&Tok::Comma) { return Err(ErrKind::Value); }
                    }
                }
                node = Node::Func(FuncName::from_id(FuncId::InternalCall), call);
            }
        }
        while matches!(self.peek(), Some(Tok::Percent)) {
            self.pos += 1;
            node = Node::Percent(Box::new(node));
        }
        Ok(node)
    }

    fn parse_primary(&mut self) -> Result<Node, ErrKind> {
        match self.next() {
            Some(Tok::Num(n)) => Ok(Node::Number(n)),
            Some(Tok::Str(s)) => Ok(Node::Text(s.into())),
            Some(Tok::LParen) => {
                let e = self.parse_expr()?;
                if !self.eat(&Tok::RParen) {
                    return Err(ErrKind::Value);
                }
                Ok(e)
            }
            Some(Tok::LBrace) => {
                let mut rows: Vec<Vec<crate::Value>> = vec![Vec::new()];
                loop {
                    let v = match self.next() {
                        Some(Tok::Num(n)) => FValue::number(n),
                        Some(Tok::Str(s)) => FValue::Text(s.into()),
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
                    rows.last_mut().unwrap().push(from_fvalue(&v));
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
                for row in rows {
                    let end = data.len() + cols;
                    data.extend(row);
                    data.resize(end,crate::Value::Empty);
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
                    return Ok(Node::Func(FuncName::from_raw(&name), args));
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
    let bytes = s.as_bytes();
    let col_abs = bytes.first() == Some(&b'$');
    let mut i = usize::from(col_abs);
    let mut col = 0_i64;
    while let Some(c) = bytes.get(i).filter(|c| c.is_ascii_alphabetic()) {
        col = col.checked_mul(26)?.checked_add((c.to_ascii_uppercase() - b'A' + 1) as i64)?;
        i += 1;
    }
    if col == 0 { return None; }
    let col = col - 1;
    let row_abs = bytes.get(i) == Some(&b'$');
    i += usize::from(row_abs);
    if !bytes.get(i)?.is_ascii_digit() { return None; }
    let row = s[i..].parse::<i64>().ok()?.checked_sub(1)?;
    if row < 0 { return None; }
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

type RangeKey = (usize, i64, i64, i64, i64);

/// Zero-allocation resolved range reference (coordinates already shifted by `Calc.ref_offset`).
struct ResolvedRange<'n> {
    book: Option<Cow<'n, str>>,
    sheet: Option<Cow<'n, str>>,
    sheet2: Option<Cow<'n, str>>,
    a_row: i64,
    a_col: i64,
    b_row: i64,
    b_col: i64,
    a_row_abs: bool,
    a_col_abs: bool,
    b_row_abs: bool,
    b_col_abs: bool,
}

impl<'n> ResolvedRange<'n> {
    #[inline]
    fn r1(&self) -> i64 { self.a_row.min(self.b_row) }
    #[inline]
    fn c1(&self) -> i64 { self.a_col.min(self.b_col) }
    #[inline]
    fn r2(&self) -> i64 { self.a_row.max(self.b_row) }
    #[inline]
    fn c2(&self) -> i64 { self.a_col.max(self.b_col) }
    #[inline]
    fn is_ext(&self) -> bool { self.book.is_some() }
    #[inline]
    fn is_3d(&self) -> bool { self.sheet2.is_some() }
}

/// Lowercase lookup index for `wb.names`, replacing O(N) linear scans on workbooks with many names.
pub(super) struct NameIndex {
    by_lower: FxHashMap<Box<str>, Vec<(Option<usize>, usize)>>,
}

impl NameIndex {
    pub(super) fn build_if_needed(wb: &Workbook) -> Option<Self> {
        if wb.names.len() <= 4 {
            return None;
        }
        let mut by_lower: FxHashMap<Box<str>, Vec<(Option<usize>, usize)>> =
            FxHashMap::with_capacity_and_hasher(wb.names.len(), FxBuildHasher);
        for (index, (nm, scope, _)) in wb.names.iter().enumerate() {
            let key: Box<str> = nm.to_ascii_lowercase().into_boxed_str();
            by_lower.entry(key).or_default().push((*scope, index));
        }
        Some(Self { by_lower })
    }

    pub(super) fn find(&self, name: &str, si: usize, scoped: bool) -> Option<usize> {
        let entries = if !name.bytes().any(|b| b.is_ascii_uppercase()) {
            self.by_lower.get(name)?
        } else {
            let low = name.to_ascii_lowercase();
            self.by_lower.get(low.as_str())?
        };
        let mut global = None;
        for &(scope, index) in entries {
            if scope == Some(si) {
                return Some(index);
            }
            if scope.is_none() && (!scoped || global.is_none()) {
                global = Some(index);
            }
        }
        global
    }
}

/// Packed `FxHashSet<PackedCellKey>` mirror of an incremental recalc `scope`, for O(1) fast membership tests.
pub(super) struct FastScope {
    packed: FxHashSet<PackedCellKey>,
    fallback: FxHashSet<deps::CellKey>,
    dirty_cols: FxHashSet<(usize, u32)>,
}

impl FastScope {
    pub(super) fn from_std(scope: &HashSet<deps::CellKey>) -> Self {
        let mut packed = FxHashSet::with_capacity_and_hasher(scope.len(), FxBuildHasher);
        let mut fallback = FxHashSet::default();
        let mut dirty_cols = FxHashSet::default();
        for &k in scope {
            dirty_cols.insert((k.0, k.2));
            if let Some(pk) = PackedCellKey::pack(k) {
                packed.insert(pk);
            } else {
                fallback.insert(k);
            }
        }
        Self {
            packed,
            fallback,
            dirty_cols,
        }
    }

    #[inline]
    pub(super) fn col_may_be_dirty(&self, si: usize, col: u32) -> bool {
        self.dirty_cols.contains(&(si, col))
    }

    #[inline]
    pub(super) fn contains(&self, key: deps::CellKey) -> bool {
        if let Some(pk) = PackedCellKey::pack(key) {
            self.packed.contains(&pk)
        } else {
            self.fallback.contains(&key)
        }
    }
}

#[derive(Clone, Hash, PartialEq, Eq)]
struct AggCacheKey {
    range: RangeKey,
    sum_range: Option<RangeKey>,
    func: u8,
    crit: String,
}

#[derive(Clone, Hash, PartialEq, Eq)]
enum VolatileArgKey {
    Empty,
    Num(u64),
    Bool(bool),
    Text(std::sync::Arc<str>),
    Err(u8),
    Other(String),
}

impl VolatileArgKey {
    fn from_arg(a: &Arg) -> Self {
        match a {
            Arg::Val(FValue::Empty) => Self::Empty,
            Arg::Val(FValue::Number(n)) => Self::Num(n.to_bits()),
            Arg::Val(FValue::Bool(b)) => Self::Bool(*b),
            Arg::Val(FValue::Text(s)) => Self::Text(std::sync::Arc::clone(s)),
            Arg::Val(FValue::Error(e)) => Self::Err(*e as u8),
            other => Self::Other(format!("{other:?}")),
        }
    }
}

#[derive(Clone, Hash, PartialEq, Eq)]
struct VolatileKey {
    cell: deps::CellKey,
    seq: usize,
    func: FuncId,
    args: Vec<VolatileArgKey>,
}

fn single_sheet_range(node: &Node) -> Option<(RefExpr, RefExpr)> {
    let (a, b) = match node {
        Node::Range(a, b) => (a, b),
        Node::Ref(a) => (a, a),
        _ => return None,
    };
    (a.sheet2.is_none() && b.sheet2.is_none()).then(|| (a.clone(), b.clone()))
}

type EvalEnvironment = Vec<value::EnvFrame>;
type EvalFrame = (deps::CellKey, EvalEnvironment);

struct RangeRemainder {
    range: RangeKey,
    offsets: std::ops::Range<usize>,
}

impl Iterator for RangeRemainder {
    type Item = deps::CellKey;

    fn next(&mut self) -> Option<Self::Item> {
        let (si, r1, c1, _, c2) = self.range;
        let width = (c2 - c1 + 1) as usize;
        let i = self.offsets.next()?;
        Some((si, r1 as u32 + (i / width) as u32, c1 as u32 + (i % width) as u32))
    }
}
type PrefixKey = (usize, i64, i64, bool);
type SumPrefixes = Vec<Result<f64, ErrKind>>;

#[derive(Clone, Hash, PartialEq, Eq)]
pub(crate) struct CriteriaKey {
    pub(crate) function: u8,
    pub(crate) ranges: Vec<RangeKey>,
    pub(crate) criteria: Vec<String>,
}

impl CriteriaKey {
    pub(crate) fn split_ranges(&self) -> Option<(Option<RangeKey>, &[RangeKey])> {
        match self.function {
            1 => (!self.ranges.is_empty()).then_some((None, &self.ranges)),
            2 => {
                let cond = self.ranges.get(..1)?;
                let val = self.ranges.get(1).or_else(|| self.ranges.first()).copied()?;
                Some((Some(val), cond))
            }
            0 | 3 | 4 | 5 => {
                let (&val, cond) = self.ranges.split_first()?;
                (!cond.is_empty()).then_some((Some(val), cond))
            }
            _ => None,
        }
    }
}

const SHARED_SHARDS: usize = 16;

#[derive(Default)]
struct SharedCacheShard {
    ranges: FxHashMap<RangeKey, std::sync::Arc<Vec<FValue>>>,
    range_vals: usize,
    lookups: FxHashMap<(RangeKey, bool), lookup_cache::Index>,
    approx: FxHashMap<(RangeKey, usize, functions::LookupKind), Option<std::sync::Arc<Vec<usize>>>>,
    grouped: FxHashMap<(Option<RangeKey>, Vec<RangeKey>, Vec<bool>), std::sync::Arc<functions::BuiltGroups>>,
    ordered: FxHashMap<(Option<RangeKey>, RangeKey, bool), std::sync::Arc<conditional_index::BuiltOrdered>>,
    cond_cols: FxHashMap<RangeKey, conditional_index::ColumnIndex>,
    criteria: FxHashMap<CriteriaKey, FValue>,
    ext_lookups: FxHashMap<(usize, usize, i64, i64, i64, i64, bool), lookup_cache::Index>,
    ext_ranges: FxHashMap<(usize, usize, u32, u32, u32, u32), std::sync::Arc<Vec<FValue>>>,
    ext_range_vals: usize,
}

pub(crate) struct SharedCalcCaches {
    shards: [std::sync::RwLock<SharedCacheShard>; SHARED_SHARDS],
    ext_bounds: std::sync::RwLock<FxHashMap<(usize, usize), (u32, u32)>>,
    /// Single-flight mutex guarding pure `SharedCondColumn`-based `BuiltOrdered` builds
    /// (zero recursion, zero formula evaluation inside lock -> deadlock-free).
    ordered_cond_mu: std::sync::Mutex<()>,
}

impl Default for SharedCalcCaches {
    fn default() -> Self {
        Self {
            shards: std::array::from_fn(|_| std::sync::RwLock::new(SharedCacheShard::default())),
            ext_bounds: std::sync::RwLock::new(FxHashMap::default()),
            ordered_cond_mu: std::sync::Mutex::new(()),
        }
    }
}

impl SharedCalcCaches {
    #[inline]
    fn shard_of<T: std::hash::Hash>(&self, key: &T) -> &std::sync::RwLock<SharedCacheShard> {
        &self.shards[(fxhash::fx_hash_one(key) as usize) & (SHARED_SHARDS - 1)]
    }

    fn get_range(&self, key: RangeKey) -> Option<std::sync::Arc<Vec<FValue>>> {
        self.shard_of(&key).read().ok()?.ranges.get(&key).cloned()
    }

    fn put_range(&self, key: RangeKey, vals: &std::sync::Arc<Vec<FValue>>) {
        const SHARD_VAL_CAP: usize = 1_048_576;
        if vals.len() > SHARD_VAL_CAP {
            return;
        }
        if let Ok(mut g) = self.shard_of(&key).write() {
            if g.ranges.contains_key(&key) {
                return;
            }
            if g.range_vals + vals.len() > SHARD_VAL_CAP {
                g.ranges.clear();
                g.range_vals = 0;
            }
            g.range_vals += vals.len();
            g.ranges.insert(key, vals.clone());
        }
    }

    fn get_lookup(&self, key: (RangeKey, bool)) -> Option<lookup_cache::Index> {
        self.shard_of(&key).read().ok()?.lookups.get(&key).cloned()
    }

    fn put_lookup(&self, key: (RangeKey, bool), ix: &lookup_cache::Index) {
        if let Ok(mut g) = self.shard_of(&key).write() {
            if g.lookups.len() >= 128 {
                g.lookups.clear();
            }
            g.lookups.entry(key).or_insert_with(|| ix.clone());
        }
    }

    fn get_approx(
        &self,
        key: &(RangeKey, usize, functions::LookupKind),
    ) -> Option<Option<std::sync::Arc<Vec<usize>>>> {
        self.shard_of(key).read().ok()?.approx.get(key).cloned()
    }

    fn put_approx(
        &self,
        key: (RangeKey, usize, functions::LookupKind),
        pos: Option<std::sync::Arc<Vec<usize>>>,
    ) {
        if let Ok(mut g) = self.shard_of(&key).write() {
            if g.approx.len() >= 256 {
                g.approx.clear();
            }
            g.approx.entry(key).or_insert(pos);
        }
    }

    fn get_grouped(
        &self,
        val_range: Option<RangeKey>,
        cond_ranges: &[RangeKey],
        numeric: &[bool],
    ) -> Option<std::sync::Arc<functions::BuiltGroups>> {
        let g = self.shard_of(&(val_range, cond_ranges)).read().ok()?;
        g.grouped
            .iter()
            .find(|((vr, cr, nm), _)| *vr == val_range && cr.as_slice() == cond_ranges && nm.as_slice() == numeric)
            .map(|(_, v)| v.clone())
    }

    fn put_grouped(
        &self,
        val_range: Option<RangeKey>,
        cond_ranges: Vec<RangeKey>,
        numeric: Vec<bool>,
        built: std::sync::Arc<functions::BuiltGroups>,
    ) {
        let shard = self.shard_of(&(val_range, cond_ranges.as_slice()));
        if let Ok(mut g) = shard.write() {
            if g.grouped.len() >= 16 {
                g.grouped.clear();
            }
            g.grouped.entry((val_range, cond_ranges, numeric)).or_insert(built);
        }
    }

    fn get_ordered(
        &self,
        val_range: Option<RangeKey>,
        cond_range: RangeKey,
        need_minmax: bool,
    ) -> Option<std::sync::Arc<conditional_index::BuiltOrdered>> {
        let k = (val_range, cond_range, need_minmax);
        self.shard_of(&k).read().ok()?.ordered.get(&k).cloned()
    }

    fn put_ordered(
        &self,
        val_range: Option<RangeKey>,
        cond_range: RangeKey,
        need_minmax: bool,
        built: std::sync::Arc<conditional_index::BuiltOrdered>,
    ) {
        let k = (val_range, cond_range, need_minmax);
        if let Ok(mut g) = self.shard_of(&k).write() {
            if g.ordered.len() >= 16 {
                g.ordered.clear();
            }
            g.ordered.entry(k).or_insert(built);
        }
    }

    fn get_cond_col(&self, rk: RangeKey) -> Option<conditional_index::ColumnIndex> {
        self.shard_of(&rk).read().ok()?.cond_cols.get(&rk).cloned()
    }

    fn put_cond_col(&self, col: &conditional_index::ColumnIndex) {
        let rk = col.range;
        if let Ok(mut g) = self.shard_of(&rk).write() {
            if let Some(existing) = g.cond_cols.get_mut(&rk) {
                if existing.shared_col.is_none() && col.shared_col.is_some() {
                    existing.shared_col = col.shared_col.clone();
                }
                if !existing.tried_numbers && col.tried_numbers {
                    existing.tried_numbers = true;
                    existing.numbers = col.numbers.clone();
                }
                if !existing.tried_text && col.tried_text {
                    existing.tried_text = true;
                    existing.text = col.text.clone();
                }
                return;
            }
            if g.cond_cols.len() >= 16 {
                g.cond_cols.clear();
            }
            g.cond_cols.insert(rk, col.clone());
        }
    }

    fn get_criteria(&self, key: &CriteriaKey) -> Option<FValue> {
        self.shard_of(key).read().ok()?.criteria.get(key).cloned()
    }

    fn put_criteria(&self, key: CriteriaKey, val: FValue) {
        let shard = self.shard_of(&key);
        if let Ok(mut g) = shard.write() {
            if g.criteria.len() >= 2048 {
                g.criteria.clear();
            }
            g.criteria.insert(key, val);
        }
    }

    fn get_ext_bounds(&self, li: usize, slot: usize) -> Option<(u32, u32)> {
        self.ext_bounds.read().ok()?.get(&(li, slot)).copied()
    }

    fn put_ext_bounds(&self, li: usize, slot: usize, b: (u32, u32)) {
        if let Ok(mut g) = self.ext_bounds.write() {
            g.insert((li, slot), b);
        }
    }

    fn get_ext_lookup(
        &self,
        key: &(usize, usize, i64, i64, i64, i64, bool),
    ) -> Option<lookup_cache::Index> {
        self.shard_of(key).read().ok()?.ext_lookups.get(key).cloned()
    }

    fn put_ext_lookup(
        &self,
        key: (usize, usize, i64, i64, i64, i64, bool),
        ix: &lookup_cache::Index,
    ) {
        if let Ok(mut g) = self.shard_of(&key).write() {
            if g.ext_lookups.len() >= 32 {
                g.ext_lookups.clear();
            }
            g.ext_lookups.entry(key).or_insert_with(|| ix.clone());
        }
    }

    fn get_ext_range(
        &self,
        key: &(usize, usize, u32, u32, u32, u32),
    ) -> Option<std::sync::Arc<Vec<FValue>>> {
        self.shard_of(key).read().ok()?.ext_ranges.get(key).cloned()
    }

    fn put_ext_range(
        &self,
        key: (usize, usize, u32, u32, u32, u32),
        vals: &std::sync::Arc<Vec<FValue>>,
    ) {
        const SHARD_VAL_CAP: usize = 1_048_576;
        if vals.len() > SHARD_VAL_CAP {
            return;
        }
        if let Ok(mut g) = self.shard_of(&key).write() {
            if g.ext_ranges.contains_key(&key) {
                return;
            }
            if g.ext_range_vals + vals.len() > SHARD_VAL_CAP {
                g.ext_ranges.clear();
                g.ext_range_vals = 0;
            }
            g.ext_range_vals += vals.len();
            g.ext_ranges.insert(key, vals.clone());
        }
    }
}

#[inline]
pub(super) fn sum_f64_8way(slice: &[f64]) -> f64 {
    let mut acc = [0.0f64; 8];
    let mut chunks = slice.chunks_exact(8);
    for c in &mut chunks {
        acc[0] += c[0];
        acc[1] += c[1];
        acc[2] += c[2];
        acc[3] += c[3];
        acc[4] += c[4];
        acc[5] += c[5];
        acc[6] += c[6];
        acc[7] += c[7];
    }
    let mut rem = 0.0f64;
    for &x in chunks.remainder() {
        rem += x;
    }
    ((acc[0] + acc[1]) + (acc[2] + acc[3])) + ((acc[4] + acc[5]) + ((acc[6] + acc[7]) + rem))
}

#[inline]
pub(super) fn min_f64_4way(slice: &[f64]) -> f64 {
    let mut acc = [f64::INFINITY; 4];
    let mut chunks = slice.chunks_exact(4);
    for c in &mut chunks {
        acc[0] = acc[0].min(c[0]);
        acc[1] = acc[1].min(c[1]);
        acc[2] = acc[2].min(c[2]);
        acc[3] = acc[3].min(c[3]);
    }
    let mut out = acc[0].min(acc[1]).min(acc[2].min(acc[3]));
    for &x in chunks.remainder() {
        out = out.min(x);
    }
    out
}

#[inline]
pub(super) fn max_f64_4way(slice: &[f64]) -> f64 {
    let mut acc = [f64::NEG_INFINITY; 4];
    let mut chunks = slice.chunks_exact(4);
    for c in &mut chunks {
        acc[0] = acc[0].max(c[0]);
        acc[1] = acc[1].max(c[1]);
        acc[2] = acc[2].max(c[2]);
        acc[3] = acc[3].max(c[3]);
    }
    let mut out = acc[0].max(acc[1]).max(acc[2].max(acc[3]));
    for &x in chunks.remainder() {
        out = out.max(x);
    }
    out
}

#[inline]
pub(super) fn dot_f64_8way(a: &[f64], b: &[f64]) -> f64 {
    let n = a.len().min(b.len());
    let a = &a[..n];
    let b = &b[..n];
    let mut acc = [0.0f64; 8];
    let mut i = 0usize;
    while i + 8 <= n {
        acc[0] += a[i] * b[i];
        acc[1] += a[i + 1] * b[i + 1];
        acc[2] += a[i + 2] * b[i + 2];
        acc[3] += a[i + 3] * b[i + 3];
        acc[4] += a[i + 4] * b[i + 4];
        acc[5] += a[i + 5] * b[i + 5];
        acc[6] += a[i + 6] * b[i + 6];
        acc[7] += a[i + 7] * b[i + 7];
        i += 8;
    }
    let mut rem = 0.0f64;
    while i < n {
        rem += a[i] * b[i];
        i += 1;
    }
    ((acc[0] + acc[1]) + (acc[2] + acc[3])) + ((acc[4] + acc[5]) + ((acc[6] + acc[7]) + rem))
}

fn named_definition(wb: &Workbook, name: &str, si: usize, scoped: bool) -> Option<usize> {
    let mut global = None;
    for (index, (nm, scope, _)) in wb.names.iter().enumerate() {
        if !nm.eq_ignore_ascii_case(name) { continue; }
        if *scope == Some(si) { return Some(index); }
        if scope.is_none() && (!scoped || global.is_none()) { global = Some(index); }
    }
    global
}

#[derive(Clone, Copy)]
struct BatchResults<'a> {
    values: &'a [(usize, u32, u32, FValue)],
    pending: &'a [usize],
}

impl<'a> BatchResults<'a> {
    #[inline]
    fn find_ready_slot(&self, key: deps::CellKey) -> Option<usize> {
        let slot = self
            .values
            .binary_search_by_key(&key, |&(si, r, c, _)| (si, r, c))
            .ok()?;
        if !self.pending.is_empty() && self.pending.binary_search(&slot).is_ok() {
            return None;
        }
        Some(slot)
    }

    #[inline]
    fn is_ready(&self, key: deps::CellKey) -> bool {
        self.find_ready_slot(key).is_some()
    }

    #[inline]
    fn get(&self, key: deps::CellKey) -> Option<&'a FValue> {
        let slot = self.find_ready_slot(key)?;
        Some(&self.values[slot].3)
    }
}

pub struct Calc<'a> {
    wb: &'a Workbook,
    ctx: functions::EvalCtx,
    restore_tls: bool,
    shared: Option<&'a SharedCalcCaches>,
    parallel_values: Option<dependency_batch::ParallelValues<'a>>,
    batch_results: Option<BatchResults<'a>>,
    memo: FxHashMap<PackedCellKey, FValue>,
    memo_overflow: FxHashMap<deps::CellKey, FValue>,
    visiting: FxHashSet<PackedCellKey>,
    visiting_overflow: FxHashSet<deps::CellKey>,
    /// LET / LAMBDA 词法环境帧栈（`<= 4` 绑定内联零堆分配）
    env_stack: EvalEnvironment,
    active_names: FxHashSet<(usize, deps::CellKey, usize)>,
    named_calls: Vec<(usize, deps::CellKey, usize, Vec<FValue>)>,
    scheduling: bool,
    requested: Option<EvalFrame>,
    range_remainder: Option<RangeRemainder>,
    /// 正在求值的单元格（供 ROW()/COLUMN() 用）
    cur: (u32, u32),
    cur_sheet: usize,
    /// 当前模板 AST 的相对坐标偏移 `(dr, dc)`（避免每次求值克隆并 `relocate` AST）
    ref_offset: (i64, i64),
    has_tables: bool,
    name_index: Option<NameIndex>,
    array_index: Option<Vec<FxHashMap<CellRef, (u32, u32, u32, u32)>>>,
    /// 同一次重算的溢出稳定化阶段重放随机/时间采样，不反复生成新值。
    volatile_values: FxHashMap<VolatileKey, FValue>,
    volatile_counts: FxHashMap<deps::CellKey, usize>,
    /// 进入时外层的日期体系，Drop 时恢复
    prev_system: crate::model::DateSystem,
    /// 进入时外层的 NOW 快照，Drop 时恢复
    prev_now: Option<f64>,
    /// 区域内容缓存：同一轮重算里同一块区域只物化一次
    range_cache: FxHashMap<RangeKey, (std::sync::Arc<Vec<FValue>>, u64)>,
    /// 每张表的实际使用范围（最大行、最大列）
    bounds: FxHashMap<usize, (u32, u32)>,
    col_bounds: FxHashMap<(usize, u32), u32>,
    ext_bounds: FxHashMap<(usize, usize), (u32, u32)>,
    ext_range_cache: FxHashMap<(usize, usize, u32, u32, u32, u32), std::sync::Arc<Vec<FValue>>>,
    ext_lookup_index: FxHashMap<(usize, usize, i64, i64, i64, i64, bool), lookup_cache::Index>,
    /// 递归深度：整列引用会把循环引用放大成深递归，超过就返回错误而不是爆栈
    depth: usize,
    approx_positions: FxHashMap<(RangeKey, usize, functions::LookupKind), Option<std::sync::Arc<Vec<usize>>>>,
    position_count: usize,
    lookup_index: FxHashMap<(RangeKey, bool), lookup_cache::Index>,
    /// 增量重算的范围：不在其中的公式单元格一律用缓存值
    scope: Option<&'a std::collections::HashSet<self::deps::CellKey>>,
    pub(super) fast_scope: Option<&'a FastScope>,
    pub(super) cancel: Option<&'a std::sync::atomic::AtomicBool>,
    pub(super) deadline: Option<std::time::Instant>,
    pub(super) aborted: bool,
    pub(super) deps_watch: Option<&'a self::deps::Deps>,
    /// 区域物化实际读取的单元格数（测试/诊断用）
    pub range_reads: usize,
    /// 区域聚合结果缓存
    agg_cache: FxHashMap<AggCacheKey, FValue>,
    /// 区域缓存已占用的值数量，防止把逐行累积的区间全缓存下来
    cache_vals: usize,
    count_indexes: functions::CountIndexes,
    cache_order: std::collections::BTreeSet<(u64, RangeKey)>,
    cache_clock: u64,
    criteria_cache: FxHashMap<CriteriaKey, FValue>,
    grouped_criteria: [functions::GroupedCriteria; 2],
    ordered_criteria: conditional_index::OrderedCriteria,
    match_indexes: conditional_index::MatchIndexes,
    sum_prefixes: FxHashMap<PrefixKey, SumPrefixes>,
    sum_ends: FxHashMap<PrefixKey, i64>,
    prefix_values: usize,
    pub agg_hits: u64,
    pub agg_miss: u64,
}

#[derive(Clone, Debug)]
pub enum Arg {
    Val(FValue),
    Range { values: std::sync::Arc<Vec<FValue>>, rows: usize, cols: usize },
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

    /// 零拷贝二维视图：区域和数组直接借用底层切片，标量才构造单元素 Cow
    pub fn grid_slice(&self) -> (usize, usize, std::borrow::Cow<'_, [FValue]>) {
        match self {
            Arg::Range { values, rows, cols } => (*rows, *cols, std::borrow::Cow::Borrowed(values.as_slice())),
            Arg::Val(FValue::Array(a)) => (a.rows, a.cols, std::borrow::Cow::Borrowed(a.data.as_slice())),
            Arg::Val(v) => (1, 1, std::borrow::Cow::Borrowed(std::slice::from_ref(v))),
        }
    }

    pub fn values(&self) -> Vec<FValue> {
        match self {
            Arg::Val(FValue::Array(a)) => a.data.as_ref().clone(),
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
            Arg::Val(FValue::Array(a)) => (a.rows, a.cols, a.data.as_ref().clone()),
            Arg::Val(v) => (1, 1, vec![v.scalar()]),
            Arg::Range { values, rows, cols } => (*rows, *cols, (**values).clone()),
        }
    }
    /// 数字序列（区域内忽略空/文本/布尔，与 SUM 一致）
    pub fn numbers(&self) -> Result<Vec<f64>, ErrKind> {
        let mut out = Vec::new();
        match self {
            Arg::Val(FValue::Array(a)) => {
                for v in a.data.iter() {
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
        if self.restore_tls {
            functions::set_current_date_system(self.prev_system);
            functions::set_now_snapshot(self.prev_now);
        }
    }
}

impl<'a> Calc<'a> {
    pub fn new(wb: &'a Workbook) -> Self {
        let prev_system = functions::current_date_system();
        functions::set_current_date_system(wb.date_system);
        let prev_now = functions::now_snapshot();
        let snap = match prev_now {
            Some(v) => v,
            None => {
                let now = functions::now_serial_uncached_in(wb.date_system);
                functions::set_now_snapshot(Some(now));
                now
            }
        };
        let ctx = functions::EvalCtx {
            date_system: wb.date_system,
            now_snapshot: Some(snap),
        };
        Self::with_ctx(wb, ctx, true, prev_system, prev_now, None, None)
    }

    pub(crate) fn new_worker(
        wb: &'a Workbook,
        ctx: functions::EvalCtx,
        scope: Option<&'a HashSet<deps::CellKey>>,
        shared: Option<&'a SharedCalcCaches>,
    ) -> Self {
        Self::with_ctx(wb, ctx, false, wb.date_system, None, scope, shared)
    }

    fn with_ctx(
        wb: &'a Workbook,
        ctx: functions::EvalCtx,
        restore_tls: bool,
        prev_system: crate::model::DateSystem,
        prev_now: Option<f64>,
        scope: Option<&'a HashSet<deps::CellKey>>,
        shared: Option<&'a SharedCalcCaches>,
    ) -> Self {
        let has_tables = wb.sheets.iter().any(|s| !s.tables.is_empty());
        let name_index = NameIndex::build_if_needed(wb);
        Self {
            wb,
            ctx,
            restore_tls,
            shared,
            parallel_values: None,
            batch_results: None,
            memo: FxHashMap::default(),
            memo_overflow: FxHashMap::default(),
            visiting: FxHashSet::default(),
            visiting_overflow: FxHashSet::default(),
            env_stack: Vec::new(),
            cur: (0, 0),
            ref_offset: (0, 0),
            has_tables,
            name_index,
            array_index: None,
            range_cache: FxHashMap::default(),
            bounds: FxHashMap::default(),
            col_bounds: FxHashMap::default(),
            ext_bounds: FxHashMap::default(),
            ext_range_cache: FxHashMap::default(),
            ext_lookup_index: FxHashMap::default(),
            lookup_index: FxHashMap::default(),
            approx_positions: FxHashMap::default(),
            position_count: 0,
            scope,
            fast_scope: None,
            cancel: None,
            deadline: None,
            aborted: false,
            deps_watch: None,
            scheduling: false,
            requested: None,
            range_remainder: None,
            active_names: FxHashSet::default(),
            named_calls: Vec::new(),
            agg_cache: FxHashMap::default(),
            cache_vals: 0,
            count_indexes: Default::default(),
            depth: 0,
            range_reads: 0,
            cache_order: Default::default(),
            cache_clock: 0,
            criteria_cache: FxHashMap::default(),
            grouped_criteria: Default::default(),
            ordered_criteria: Default::default(),
            match_indexes: Default::default(),
            sum_prefixes: FxHashMap::default(),
            sum_ends: FxHashMap::default(),
            prefix_values: 0,
            agg_hits: 0,
            agg_miss: 0,
            cur_sheet: 0,
            volatile_values: FxHashMap::default(),
            volatile_counts: FxHashMap::default(),
            prev_system,
            prev_now,
        }
    }

    #[inline]
    pub(super) fn check_abort(&mut self) -> bool {
        if self.aborted {
            return true;
        }
        if self
            .cancel
            .is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed))
            || self
                .deadline
                .is_some_and(|d| std::time::Instant::now() >= d)
        {
            self.aborted = true;
            return true;
        }
        false
    }

    #[inline]
    fn ready_dirty_value_at(&self, si: usize, abs_row: u32, col: u32) -> Option<crate::Value> {
        let k = (si, abs_row, col);
        if let Some(m) = self.get_memo(k) {
            return Some(from_fvalue(&m.scalar()));
        }
        if let Some(pv) = self.parallel_values
            && let Some(v) = pv.get(k)
        {
            return Some(from_fvalue(&v.scalar()));
        }
        None
    }

    #[inline]
    fn has_memo(&self, key: deps::CellKey) -> bool {
        let in_local = if let Some(pk) = PackedCellKey::pack(key) {
            self.memo.contains_key(&pk)
        } else {
            self.memo_overflow.contains_key(&key)
        };
        in_local || self.batch_results.is_some_and(|br| br.is_ready(key))
    }

    #[inline]
    fn get_memo(&self, key: deps::CellKey) -> Option<&FValue> {
        let local = if let Some(pk) = PackedCellKey::pack(key) {
            self.memo.get(&pk)
        } else {
            self.memo_overflow.get(&key)
        };
        local.or_else(|| self.batch_results.and_then(|br| br.get(key)))
    }

    #[inline]
    pub(super) fn insert_memo(&mut self, key: deps::CellKey, val: FValue) {
        if let Some(pk) = PackedCellKey::pack(key) {
            self.memo.insert(pk, val);
        } else {
            self.memo_overflow.insert(key, val);
        }
    }

    #[inline]
    fn remove_memo(&mut self, key: deps::CellKey) {
        if let Some(pk) = PackedCellKey::pack(key) {
            self.memo.remove(&pk);
        } else {
            self.memo_overflow.remove(&key);
        }
    }

    #[inline]
    fn has_visiting(&self, key: deps::CellKey) -> bool {
        if let Some(pk) = PackedCellKey::pack(key) {
            self.visiting.contains(&pk)
        } else {
            self.visiting_overflow.contains(&key)
        }
    }

    #[inline]
    fn insert_visiting(&mut self, key: deps::CellKey) -> bool {
        if let Some(pk) = PackedCellKey::pack(key) {
            self.visiting.insert(pk)
        } else {
            self.visiting_overflow.insert(key)
        }
    }

    #[inline]
    fn remove_visiting(&mut self, key: deps::CellKey) {
        if let Some(pk) = PackedCellKey::pack(key) {
            self.visiting.remove(&pk);
        } else {
            self.visiting_overflow.remove(&key);
        }
    }

    #[inline]
    fn clear_visiting(&mut self) {
        self.visiting.clear();
        if !self.visiting_overflow.is_empty() {
            self.visiting_overflow.clear();
        }
    }

    #[inline]
    fn in_scope(&self, key: deps::CellKey) -> bool {
        if let Some(fs) = self.fast_scope {
            fs.contains(key)
        } else if let Some(s) = self.scope {
            s.contains(&key)
        } else {
            true
        }
    }

    #[inline]
    fn outside_scope(&self, key: deps::CellKey) -> bool {
        !self.in_scope(key)
    }

    #[inline]
    fn r_row(&self, r: &RefExpr) -> i64 {
        if r.row_abs { r.row } else { r.row + self.ref_offset.0 }
    }

    #[inline]
    fn r_col(&self, r: &RefExpr) -> i64 {
        if r.col_abs { r.col } else { r.col + self.ref_offset.1 }
    }

    #[inline]
    pub(super) fn eval_node_shifted(&mut self, node: &Node, si: usize, dr: i64, dc: i64) -> FValue {
        let prev = self.ref_offset;
        self.ref_offset = (dr, dc);
        let out = self.eval_node(node, si);
        self.ref_offset = prev;
        out
    }

    #[inline]
    pub(super) fn reset_partial(&mut self, key: deps::CellKey) {
        self.requested = None;
        self.range_remainder = None;
        self.clear_visiting();
        self.active_names.clear();
        self.named_calls.clear();
        self.env_stack.clear();
        self.remove_memo(key);
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

    fn ext_sheet_bounds(&mut self, li: usize, slot: usize) -> (u32, u32) {
        if let Some(&b) = self.ext_bounds.get(&(li, slot)) {
            return b;
        }
        if let Some(shared) = self.shared
            && let Some(b) = shared.get_ext_bounds(li, slot)
        {
            self.ext_bounds.insert((li, slot), b);
            return b;
        }
        let link = &self.wb.links[li];
        let mut max_r = 0u32;
        let mut max_c = 0u32;
        for &(s, r, c) in link.cache.range((slot, 0, 0)..=(slot, u32::MAX, u32::MAX)).map(|(k, _)| k) {
            let _ = s;
            max_r = max_r.max(r);
            max_c = max_c.max(c);
        }
        let b = (max_r, max_c);
        self.ext_bounds.insert((li, slot), b);
        if let Some(shared) = self.shared {
            shared.put_ext_bounds(li, slot, b);
        }
        b
    }

    /// 外部引用单格（已解偏移坐标）：从 links 的缓存里取；没有缓存（链接没解析过）就是 #REF!
    fn link_cell_at(&self, book: Option<&str>, sheet: Option<&str>, row: i64, col: i64) -> FValue {
        if row < 0 || col < 0 {
            return FValue::Error(ErrKind::Ref);
        }
        let Some(li) = crate::xlinks::resolve_book(self.wb, book.unwrap_or("")) else {
            return FValue::Error(ErrKind::Ref);
        };
        let link = &self.wb.links[li];
        if link.cache.is_empty() {
            return FValue::Error(ErrKind::Ref);
        }
        let Some(slot) = crate::xlinks::sheet_slot(link, sheet) else {
            return FValue::Error(ErrKind::Ref);
        };
        match link.cache.get(&(slot, row as u32, col as u32)) {
            Some(v) => to_fvalue(v),
            None => FValue::Empty,
        }
    }

    /// 外部引用单格：从 links 的缓存里取；没有缓存（链接没解析过）就是 #REF!
    fn link_cell(&self, r: &RefExpr) -> FValue {
        self.link_cell_at(r.book.as_deref(), r.sheet.as_deref(), self.r_row(r), self.r_col(r))
    }

    /// 外部引用区域（已解偏移坐标）：按缓存矩形取，缺的格当空（带 O(log K) BTreeMap 切片与缓存）
    fn link_range_coords(
        &mut self,
        book: Option<&str>,
        sheet: Option<&str>,
        ar: i64,
        ac: i64,
        br: i64,
        bc: i64,
    ) -> Result<Arg, ErrKind> {
        let Some(li) = crate::xlinks::resolve_book(self.wb, book.unwrap_or("")) else {
            return Err(ErrKind::Ref);
        };
        if self.wb.links[li].cache.is_empty() {
            return Err(ErrKind::Ref);
        }
        let Some(slot) = crate::xlinks::sheet_slot(&self.wb.links[li], sheet) else {
            return Err(ErrKind::Ref);
        };
        let mut r1 = ar.min(br).max(0) as u32;
        let mut c1 = ac.min(bc).max(0) as u32;
        let mut r2 = ar.max(br).max(0) as u32;
        let mut c2 = ac.max(bc).max(0) as u32;
        let full_cols = c1 == 0 && c2 == 16_383;
        let full_rows = r1 == 0 && r2 == 1_048_575;
        let huge = (r2 as u64 - r1 as u64 + 1) * (c2 as u64 - c1 as u64 + 1) > 200_000;
        if full_cols || full_rows || huge {
            let (max_r, max_c) = self.ext_sheet_bounds(li, slot);
            if full_rows || huge {
                r2 = r2.min(max_r);
            }
            if full_cols || huge {
                c2 = c2.min(max_c);
            }
            if r1 > r2 || c1 > c2 {
                r1 = 0;
                c1 = 0;
                r2 = 0;
                c2 = 0;
            }
        }
        let rows = (r2 - r1 + 1) as usize;
        let cols = (c2 - c1 + 1) as usize;
        let ekey = (li, slot, r1, c1, r2, c2);
        if let Some(cached) = self.ext_range_cache.get(&ekey) {
            return Ok(Arg::Range {
                values: cached.clone(),
                rows,
                cols,
            });
        }
        if let Some(shared) = self.shared
            && let Some(cached) = shared.get_ext_range(&ekey)
        {
            if self.ext_range_cache.len() >= 16 {
                self.ext_range_cache.clear();
            }
            self.ext_range_cache.insert(ekey, cached.clone());
            return Ok(Arg::Range {
                values: cached,
                rows,
                cols,
            });
        }
        let link = &self.wb.links[li];
        let mut vals = vec![FValue::Empty; rows * cols];
        for (&(_, r, c), v) in link.cache.range((slot, r1, c1)..=(slot, r2, c2)) {
            if c >= c1 && c <= c2 {
                vals[(r - r1) as usize * cols + (c - c1) as usize] = to_fvalue(v);
            }
        }
        let arc_vals = std::sync::Arc::new(vals);
        if rows * cols <= 1_000_000 {
            if let Some(shared) = self.shared {
                shared.put_ext_range(ekey, &arc_vals);
            }
            if self.ext_range_cache.len() >= 16 {
                self.ext_range_cache.clear();
            }
            self.ext_range_cache.insert(ekey, arc_vals.clone());
        }
        Ok(Arg::Range {
            values: arc_vals,
            rows,
            cols,
        })
    }

    /// 外部引用区域：按缓存矩形取，缺的格当空
    fn link_range(&mut self, a: &RefExpr, b: &RefExpr) -> Result<Arg, ErrKind> {
        self.link_range_coords(
            a.book.as_deref().or(b.book.as_deref()),
            a.sheet.as_deref().or(b.sheet.as_deref()),
            self.r_row(a),
            self.r_col(a),
            self.r_row(b),
            self.r_col(b),
        )
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
        let row = self.r_row(r);
        let col = self.r_col(r);
        if row < 0 || col < 0 {
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
            let v = match link.cache.get(&(slot, row as u32, col as u32)) {
                Some(v) => to_fvalue(v),
                None => FValue::Empty,
            };
            vals.push(v);
        }
        FValue::Array(std::sync::Arc::new(value::Array::new(vals.len().max(1), 1, vals)))
    }

    /// 外部工作簿三维区域 `[1]First:Third!A1:B2` → 纵向拼接的多表二维网格。
    fn link_three_d_range(&mut self, a: &RefExpr, b: &RefExpr) -> Result<FValue, ErrKind> {
        let book = a.book.as_deref().or(b.book.as_deref()).unwrap_or("");
        let Some(li) = crate::xlinks::resolve_book(self.wb, book) else {
            return Err(ErrKind::Ref);
        };
        if self.wb.links[li].cache.is_empty() && !self.wb.links[li].refreshed {
            return Err(ErrKind::Ref);
        }
        let rep = if a.sheet2.is_some() { a } else { b };
        let slots = self.link_three_d_slots(&self.wb.links[li], rep)?;
        let (ar, ac) = (self.r_row(a), self.r_col(a));
        let (br, bc) = (self.r_row(b), self.r_col(b));
        let r1 = ar.min(br).max(0) as u32;
        let c1 = ac.min(bc).max(0) as u32;
        let mut r2 = ar.max(br).max(0) as u32;
        let mut c2 = ac.max(bc).max(0) as u32;
        let full_cols = c1 == 0 && c2 == 16_383;
        let full_rows = r1 == 0 && r2 == 1_048_575;
        let huge = (r2 as u64 - r1 as u64 + 1) * (c2 as u64 - c1 as u64 + 1) > 200_000;
        if full_cols || full_rows || huge {
            let mut max_r = 0u32;
            let mut max_c = 0u32;
            for &s in &slots {
                let (sr, sc) = self.ext_sheet_bounds(li, s);
                max_r = max_r.max(sr);
                max_c = max_c.max(sc);
            }
            if full_rows || huge {
                r2 = r2.min(max_r);
            }
            if full_cols || huge {
                c2 = c2.min(max_c);
            }
            if r1 > r2 || c1 > c2 {
                return Ok(FValue::Array(std::sync::Arc::new(value::Array::new(1, 1, vec![FValue::Empty]))));
            }
        }
        let rows_per = (r2 - r1 + 1) as usize;
        let cols = (c2 - c1 + 1) as usize;
        let link = &self.wb.links[li];
        let mut vals = vec![FValue::Empty; slots.len() * rows_per * cols];
        for (idx, &slot) in slots.iter().enumerate() {
            let sheet_base = idx * rows_per * cols;
            for (&(_, r, c), v) in link.cache.range((slot, r1, c1)..=(slot, r2, c2)) {
                if c >= c1 && c <= c2 {
                    vals[sheet_base + (r - r1) as usize * cols + (c - c1) as usize] = to_fvalue(v);
                }
            }
        }
        Ok(FValue::Array(std::sync::Arc::new(value::Array::new(
            (slots.len() * rows_per).max(1),
            cols.max(1),
            vals,
        ))))
    }

    fn defined_name_index(&self, name: &str, si: usize, scoped: bool) -> Option<usize> {
        if let Some(ref ix) = self.name_index {
            ix.find(name, si, scoped)
        } else {
            named_definition(self.wb, name, si, scoped)
        }
    }

    fn eval_defined_name(&mut self, name: &str, si: usize, scoped: bool) -> FValue {
        let Some(index) = self.defined_name_index(name, si, scoped) else { return FValue::Error(ErrKind::Name); };
        self.eval_definition(index, si)
    }

    fn eval_definition(&mut self, index: usize, si: usize) -> FValue {
        let key = (index, (self.cur_sheet, self.cur.0, self.cur.1), si);
        if self.active_names.len() >= 32 || !self.active_names.insert(key) { return FValue::Error(ErrKind::Name); }
        let body = self.wb.names[index].2.trim_start_matches('=');
        let expanded = if self.has_tables && body.contains('[') {
            crate::table::expand_structured_at(self.wb, si, Some(self.cur.0), Some(self.cur.1), body)
        } else {
            None
        };
        let prev_off = self.ref_offset;
        self.ref_offset = (0, 0);
        let value = compiled::with_shifted(expanded.as_deref().unwrap_or(body), self.cur.0, self.cur.1, |node, dr, dc| {
            self.eval_node_shifted(node, si, dr, dc)
        }).unwrap_or(FValue::Error(ErrKind::Name));
        self.ref_offset = prev_off;
        self.active_names.remove(&key);
        value
    }

    fn call_named_lambda(&mut self, name: &str, si: usize, scoped: bool, args: &[Arg]) -> FValue {
        let Some(index) = self.defined_name_index(name, si, scoped) else {
            return FValue::Error(ErrKind::Name);
        };
        let values: Vec<_> = args.iter().map(|arg| match arg {
            Arg::Val(v) => v.clone(),
            Arg::Range { rows: 1, cols: 1, .. } => arg.first(),
            Arg::Range { rows, cols, values } => FValue::Array(std::sync::Arc::new(
                value::Array::from_shared(*rows, *cols, values.clone()),
            )),
        }).collect();
        let cell = (self.cur_sheet, self.cur.0, self.cur.1);
        let frame_slots = |arity: usize| arity + 2;
        let slots = self.named_calls.iter().map(|(_, _, _, args)| frame_slots(args.len())).sum::<usize>();
        if slots + frame_slots(values.len()) > 1024 || self.named_calls.iter().any(|(i, at, sheet, v)|
            (*i, *at, *sheet) == (index, cell, si) && v == &values) {
            return FValue::Error(ErrKind::Num);
        }
        self.named_calls.push((index, cell, si, values.clone()));
        let caller = std::mem::take(&mut self.env_stack);
        let result = stacker::maybe_grow(256 * 1024, 2 * 1024 * 1024, || {
            match self.eval_definition(index, si) {
                FValue::Lambda(lambda) => self.apply_lambda_val(&lambda, values, si),
                FValue::Error(e) => FValue::Error(e),
                _ => FValue::Error(ErrKind::Value),
            }
        });
        self.env_stack = caller;
        self.named_calls.pop();
        result
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
        let Ok(mut node) = parse_normalized(body) else {
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
        let prev_off = self.ref_offset;
        self.ref_offset = (0, 0);
        self.depth += 1;
        let out = self.eval_node(&node, si);
        self.depth -= 1;
        self.ref_offset = prev_off;
        out
    }

    pub fn cell_value(&mut self, si: usize, r: u32, c: u32) -> FValue {
        if self.scheduling {
            return self.cell_value_inner(si, r, c, false);
        }
        if self.depth >= 2 {
            return self.evaluate_stack((si, r, c));
        }
        self.depth += 1;
        let out = self.cell_value_inner(si, r, c, true);
        self.depth -= 1;
        out
    }

    fn evaluate_stack(&mut self, root: deps::CellKey) -> FValue {
        self.scheduling = true;
        let (si, r, c) = root;
        let ready = self.cell_value_inner(si, r, c, false);
        let Some(frame) = self.requested.take() else {
            self.scheduling = false;
            return ready;
        };
        let outer = std::mem::take(&mut self.env_stack);
        let mut stack = vec![(frame, None::<(RangeRemainder, EvalEnvironment)>)];
        let result = loop {
            let ((key, env), remainder) = stack.last_mut().unwrap();
            if let Some((cells, bindings)) = remainder {
                let next = cells.find(|key| {
                    let (si, row, col) = *key;
                    !self.has_memo(*key)
                        && !self.has_visiting(*key)
                        && !self.parallel_values.is_some_and(|pv| pv.is_ready(*key))
                        && self.wb.sheets[si].get(row, col).is_some_and(|c| {
                            c.formula.is_some()
                                && (self.in_scope(*key) || callable_cache_candidate(&c.value))
                        })
                });
                if let Some(key) = next {
                    let frame = (key, bindings.clone());
                    stack.push((frame, None));
                    continue;
                }
                *remainder = None;
            }
            self.remove_visiting(*key);
            self.env_stack = std::mem::take(env);
            let (si, r, c) = *key;
            let value = self.cell_value_inner(si, r, c, true);
            *env = std::mem::take(&mut self.env_stack);
            if let Some(dependency) = self.requested.take() {
                self.insert_visiting(*key);
                *remainder = self.range_remainder.take().map(|cells| (cells, dependency.1.clone()));
                stack.push((dependency, None));
                continue;
            }
            stack.pop();
            if stack.is_empty() { break value; }
        };
        self.env_stack = outer;
        self.scheduling = false;
        result
    }

    pub(super) fn cell_value_inner(&mut self, si: usize, r: u32, c: u32, evaluate: bool) -> FValue {
        if self.requested.is_some() { return FValue::Error(ErrKind::Ref); }
        if self.check_abort() { return FValue::Error(ErrKind::Value); }
        let key = (si, r, c);
        if (!self.memo.is_empty() || !self.memo_overflow.is_empty())
            && let Some(v) = self.get_memo(key)
        {
            return v.clone();
        }
        let cell = match self.wb.sheets.get(si).and_then(|s| s.get_view(r, c)) {
            None => return FValue::Empty,
            Some(crate::store::CellView::Number(n)) => return FValue::Number(n),
            Some(crate::store::CellView::Value(v)) => return to_fvalue(v),
            Some(crate::store::CellView::Cell(c)) => c,
        };
        let Some(f_arc) = cell.formula.as_ref() else {
            return to_fvalue(&cell.value);
        };
        if let Some(pv) = self.parallel_values
            && let Some(fv) = pv.get(key)
        {
            return fv.clone();
        }
        if let Some(br) = self.batch_results
            && let Some(fv) = br.get(key)
        {
            return fv.clone();
        }
        let outside_scope = self.outside_scope(key);
        if outside_scope && !callable_cache_candidate(&cell.value) {
            return to_fvalue(&cell.value);
        }
        if !self.insert_visiting(key) {
            CYCLE_CELLS.with(|cells| {
                cells.borrow_mut().insert(key);
            });
            if self.wb.calc_iterate {
                let prev = to_fvalue(&cell.value);
                return match prev {
                    FValue::Error(_) | FValue::Empty => FValue::number(0.0),
                    v => v,
                };
            }
            return FValue::Error(ErrKind::Circular);
        }
        if !evaluate {
            self.remove_visiting(key);
            self.requested = Some((key, self.env_stack.clone()));
            return FValue::Error(ErrKind::Ref);
        }
        let expanded = if self.has_tables && f_arc.contains('[') {
            let raw = f_arc.strip_prefix('=').unwrap_or(f_arc);
            crate::table::expand_structured_at(self.wb, si, Some(r), Some(c), raw)
        } else {
            None
        };
        let previous = (self.cur_sheet, self.cur, self.ref_offset);
        self.cur_sheet = si;
        self.cur = (r, c);
        self.ref_offset = (0, 0);
        if !self.volatile_counts.is_empty() {
            self.volatile_counts.remove(&key);
        }
        if let Some(dw) = self.deps_watch {
            dw.clear_dynamic_watch(key);
        }
        let eval_dispatch = |this: &mut Self, node: &Node, dr: i64, dc: i64, kind: compiled::ScalarKind| {
            let fast = if this.env_stack.is_empty() {
                match kind {
                    compiled::ScalarKind::SameSheet => {
                        compiled::eval_projected_scalar(node, dr, dc, &mut |_, row, col| {
                            Some(this.cell_value(si, row, col))
                        })
                    }
                    compiled::ScalarKind::CrossSheet => {
                        compiled::eval_projected_scalar(node, dr, dc, &mut |sh_opt, row, col| {
                            let target_si = match sh_opt {
                                Some(n) => this.sheet_index(Some(n), si).ok()?,
                                None => si,
                            };
                            Some(this.cell_value(target_si, row, col))
                        })
                    }
                    compiled::ScalarKind::None => None,
                }
            } else {
                None
            };
            fast.unwrap_or_else(|| this.eval_node_shifted(node, si, dr, dc))
        };
        let mut v = if let Some(ref exp) = expanded {
            compiled::with_eval_str(exp, r, c, |node, dr, dc, kind| {
                eval_dispatch(self, node, dr, dc, kind)
            })
            .unwrap_or_else(FValue::Error)
        } else {
            compiled::with_eval_arc(f_arc, r, c, |node, dr, dc, kind| {
                eval_dispatch(self, node, dr, dc, kind)
            })
            .unwrap_or_else(FValue::Error)
        };
        if outside_scope && !matches!(v, FValue::Lambda(_)) {
            v = to_fvalue(&cell.value);
        }
        if let FValue::Array(a) = &v {
            if array_blocked(self.wb, si, r, c, a, true) {
                v = FValue::Error(ErrKind::Spill);
            }
        }
        (self.cur_sheet, self.cur, self.ref_offset) = previous;
        self.remove_visiting(key);
        if self.requested.is_none() && !self.aborted {
            if let Some(dw) = self.deps_watch {
                dw.mark_dynamic_evaluated(key);
            }
            self.insert_memo(key, v.clone());
        }
        v
    }

    fn array_anchor_rect(&mut self, si: usize, anchor: CellRef) -> Option<(u32, u32, u32, u32)> {
        let sh = self.wb.sheets.get(si)?;
        if sh.arrays.is_empty() {
            return None;
        }
        if sh.arrays.len() <= 8 {
            return sh.arrays.iter().find(|(a, _)| *a == anchor).map(|(_, r)| *r);
        }
        let wb = self.wb;
        let idx = self.array_index.get_or_insert_with(|| {
            wb.sheets
                .iter()
                .map(|s| {
                    let mut m = FxHashMap::with_capacity_and_hasher(s.arrays.len(), FxBuildHasher);
                    for &(a, rect) in &s.arrays {
                        m.entry(a).or_insert(rect);
                    }
                    m
                })
                .collect()
        });
        idx.get(si).and_then(|m| m.get(&anchor).copied())
    }

    fn materialized_spill(&mut self, si: usize, r: u32, c: u32) -> Option<value::Array> {
        let sh = self.wb.sheets.get(si)?;
        let anchor = CellRef::new(r, c);
        if let Some(&(rows, cols)) = sh.spill_shapes.get(&anchor) {
            if rows == 0 || cols == 0 { return None; }
            let data = (0..rows)
                .flat_map(|rr| (0..cols).map(move |cc| to_fvalue(&sh.value(r + rr, c + cc))))
                .collect();
            return Some(value::Array::new(rows as usize, cols as usize, data));
        }
        if let Some((r1, c1, r2, c2)) = self.array_anchor_rect(si, anchor) {
            let sh = &self.wb.sheets[si];
            let rows = (r2 - r1 + 1) as usize;
            let cols = (c2 - c1 + 1) as usize;
            let data = (r1..=r2)
                .flat_map(|rr| (c1..=c2).map(move |cc| to_fvalue(&sh.value(rr, cc))))
                .collect();
            return Some(value::Array::new(rows, cols, data));
        }
        None
    }

    fn spill_value(&mut self, si: usize, r: u32, c: u32) -> FValue {
        if self.wb.sheets.get(si).is_none() {
            return FValue::Error(ErrKind::Ref);
        }
        let key = (si, r, c);
        if self.outside_scope(key) {
            if let Some(a) = self.materialized_spill(si, r, c) {
                return FValue::Array(std::sync::Arc::new(a));
            }
            if let crate::Value::Error(error) = self.wb.sheets[si].value(r, c) {
                return FValue::Error(error);
            }
            return FValue::Error(ErrKind::Ref);
        }
        match self.cell_value(si, r, c) {
            FValue::Array(a) => FValue::Array(a),
            FValue::Error(e) => FValue::Error(e),
            _ => FValue::Error(ErrKind::Ref),
        }
    }

    fn implicit_value(&mut self, n: &Node, si: usize) -> FValue {
        match n {
            Node::Range(a, b) => self.implicit_range(a, b, si),
            _ => self.eval_node(n, si).scalar(),
        }
    }

    fn implicit_range(&mut self, a: &RefExpr, b: &RefExpr, si: usize) -> FValue {
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
        let (ar, ac) = (self.r_row(a), self.r_col(a));
        let (br, bc) = (self.r_row(b), self.r_col(b));
        let (r1, c1) = (ar.min(br).max(0) as u32, ac.min(bc).max(0) as u32);
        let (mut r2, mut c2) = (ar.max(br).max(0) as u32, ac.max(bc).max(0) as u32);
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
            if fc >= c1 as i64 && fc <= c2 as i64 {
                Some((r1, fc as u32))
            } else {
                None
            }
        } else if c1 == c2 {
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

    fn materialize_single_col_pages(&mut self, key: RangeKey, length: usize) -> Option<Vec<FValue>> {
        let (si, r1_i, c1_i, r2_i, c2_i) = key;
        if c1_i != c2_i || r1_i < 0 || c1_i < 0 || r2_i < r1_i {
            return None;
        }
        let sheet = self.wb.sheets.get(si)?;
        let (r1, r2, col) = (r1_i as u32, r2_i as u32, c1_i as u32);
        let pages = sheet.cf_pages((r1, col, r2, col));
        for p in &pages {
            let crate::store::ColSlab::Mixed(cs) = p.cells.as_ref() else {
                continue; // O(1) skip: Numbers and Values slabs never contain formulas!
            };
            for ro in p.start..p.end {
                let cell = &cs[ro];
                if cell.formula.is_some() {
                    let abs_row = p.page * crate::store::PAGE_ROWS + ro as u32;
                    let k = (si, abs_row, col);
                    if self.has_memo(k) {
                        continue;
                    }
                    if self.parallel_values.is_some_and(|pv| pv.is_ready(k)) {
                        continue;
                    }
                    let out_of_scope_valid = self.outside_scope(k)
                        && !callable_cache_candidate(&cell.value);
                    if !out_of_scope_valid {
                        return None;
                    }
                }
            }
        }
        let mut values = vec![FValue::Empty; length];
        for (pi, p) in pages.iter().enumerate() {
            if (pi & 31) == 31 && self.check_abort() {
                return None;
            }
            let page_base = p.page * crate::store::PAGE_ROWS;
            match p.cells.as_ref() {
                crate::store::ColSlab::Numbers(ns) => {
                    let span = p.end - p.start;
                    let dst_base = (page_base + p.start as u32 - r1) as usize;
                    if crate::store::NumberSlab::all_present_in(&ns.present, p.start, span) {
                        for (i, &n) in ns.values[p.start..p.end].iter().enumerate() {
                            values[dst_base + i] = FValue::Number(n);
                        }
                    } else {
                        for ro in p.start..p.end {
                            if (ns.present[ro / 64] & (1u64 << (ro % 64))) != 0 {
                                let idx = (page_base + ro as u32 - r1) as usize;
                                values[idx] = FValue::Number(ns.values[ro]);
                            }
                        }
                    }
                }
                crate::store::ColSlab::Values(vs) => {
                    for ro in p.start..p.end {
                        let v = &vs[ro];
                        if !v.is_empty() {
                            let idx = (page_base + ro as u32 - r1) as usize;
                            values[idx] = to_fvalue(v);
                        }
                    }
                }
                crate::store::ColSlab::Mixed(cs) => {
                    for ro in p.start..p.end {
                        let cell = &cs[ro];
                        if cell.is_blank() {
                            continue;
                        }
                        let abs_row = page_base + ro as u32;
                        let idx = (abs_row - r1) as usize;
                        let fv = if cell.formula.is_some() {
                            let k = (si, abs_row, col);
                            if let Some(m) = self.get_memo(k) {
                                m.scalar()
                            } else if let Some(pv) = self.parallel_values
                                && let Some(val) = pv.get(k)
                            {
                                val.scalar()
                            } else {
                                to_fvalue(&cell.value)
                            }
                        } else {
                            to_fvalue(&cell.value)
                        };
                        values[idx] = fv;
                    }
                }
            }
        }
        self.range_reads += length;
        Some(values)
    }

    fn materialize_range(&mut self, key: RangeKey, length: usize) -> Result<std::sync::Arc<Vec<FValue>>, ErrKind> {
        if self.requested.is_some() { return Err(ErrKind::Ref); }
        if let Some(values) = self.cache_get(key) { return Ok(values); }
        if let Some(shared) = self.shared
            && let Some(values) = shared.get_range(key)
        {
            return Ok(self.cache_put(key, values));
        }
        if length >= 64
            && let Some(values) = self.materialize_single_col_pages(key, length)
        {
            let arc = std::sync::Arc::new(values);
            if let Some(shared) = self.shared {
                shared.put_range(key, &arc);
            }
            return Ok(self.cache_put(key, arc));
        }
        if self.aborted {
            return Err(ErrKind::Value);
        }
        let mut values = Vec::with_capacity(length);
        let (si, r1, c1, r2, c2) = key;
        let mut step = 0usize;
        for row in r1..=r2 {
            for col in c1..=c2 {
                if (step & 8191) == 8191 && self.check_abort() {
                    return Err(ErrKind::Value);
                }
                step += 1;
                self.range_reads += 1;
                values.push(self.cell_value(si, row as u32, col as u32).scalar());
                if self.aborted {
                    return Err(ErrKind::Value);
                }
                if self.requested.is_some() {
                    self.range_remainder = Some(RangeRemainder { range: key, offsets: values.len()..length });
                    return Err(ErrKind::Ref);
                }
            }
        }
        let arc = std::sync::Arc::new(values);
        if !self.aborted {
            if let Some(shared) = self.shared {
                shared.put_range(key, &arc);
            }
            return Ok(self.cache_put(key, arc));
        }
        Ok(arc)
    }

    fn range_values_coords(&mut self, si: usize, ar: i64, ac: i64, br: i64, bc: i64) -> Result<Arg, ErrKind> {
        let (r1, c1) = (ar.min(br).max(0) as u32, ac.min(bc).max(0) as u32);
        let (mut r2, mut c2) = (ar.max(br).max(0) as u32, ac.max(bc).max(0) as u32);
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
                values: std::sync::Arc::new(Vec::new()),
                rows: 0,
                cols: 0,
            });
        }
        let huge = (r2 as u64 - r1 as u64 + 1) * (c2 as u64 - c1 as u64 + 1) > 200_000;
        if huge {
            r2 = r2.min(max_r);
            c2 = c2.min(max_c);
            if r1 > r2 || c1 > c2 {
                return Ok(Arg::Range {
                    values: std::sync::Arc::new(Vec::new()),
                    rows: 0,
                    cols: 0,
                });
            }
        }
        let rows = (r2 - r1 + 1) as usize;
        let cols = (c2 - c1 + 1) as usize;
        let key = (si, r1 as i64, c1 as i64, r2 as i64, c2 as i64);
        let values = self.materialize_range(key, rows * cols)?;
        Ok(Arg::Range { values, rows, cols })
    }

    fn range_values_resolved(&mut self, rr: &ResolvedRange<'_>, cur: usize) -> Result<Arg, ErrKind> {
        if rr.is_ext() {
            return self.link_range_coords(
                rr.book.as_deref(),
                rr.sheet.as_deref(),
                rr.a_row,
                rr.a_col,
                rr.b_row,
                rr.b_col,
            );
        }
        let si = self.sheet_index(rr.sheet.as_deref(), cur)?;
        self.range_values_coords(si, rr.a_row, rr.a_col, rr.b_row, rr.b_col)
    }

    fn range_values(&mut self, a: &RefExpr, b: &RefExpr, cur: usize) -> Result<Arg, ErrKind> {
        if a.book.is_some() || b.book.is_some() {
            return self.link_range(a, b);
        }
        let si = self.sheet_index(a.sheet.as_deref(), cur)?;
        self.range_values_coords(si, self.r_row(a), self.r_col(a), self.r_row(b), self.r_col(b))
    }

    fn resolve_range_node<'n>(&self, node: &'n Node, si: usize) -> Option<ResolvedRange<'n>> {
        match node {
            Node::Range(a, b) if a.sheet2.is_none() && b.sheet2.is_none() => Some(ResolvedRange {
                book: a.book.as_deref().or(b.book.as_deref()).map(Cow::Borrowed),
                sheet: a.sheet.as_deref().or(b.sheet.as_deref()).map(Cow::Borrowed),
                sheet2: None,
                a_row: self.r_row(a),
                a_col: self.r_col(a),
                b_row: self.r_row(b),
                b_col: self.r_col(b),
                a_row_abs: a.row_abs,
                a_col_abs: a.col_abs,
                b_row_abs: b.row_abs,
                b_col_abs: b.col_abs,
            }),
            Node::Ref(a) if a.sheet2.is_none() => {
                let ar = self.r_row(a);
                let ac = self.r_col(a);
                Some(ResolvedRange {
                    book: a.book.as_deref().map(Cow::Borrowed),
                    sheet: a.sheet.as_deref().map(Cow::Borrowed),
                    sheet2: None,
                    a_row: ar,
                    a_col: ac,
                    b_row: ar,
                    b_col: ac,
                    a_row_abs: a.row_abs,
                    a_col_abs: a.col_abs,
                    b_row_abs: a.row_abs,
                    b_col_abs: a.col_abs,
                })
            }
            Node::Name(n) if !self.env_stack.iter().rev().any(|env| env.contains_key(n)) => {
                self.defined_name_index(n, si, false).and_then(|idx| {
                    let src = self.wb.names[idx].2.trim_start_matches('=');
                    let parsed = compile_at(src, self.cur.0, self.cur.1).ok()?;
                    let (a, b) = single_sheet_range(&parsed)?;
                    Some(ResolvedRange {
                        book: a.book.or(b.book).map(Cow::Owned),
                        sheet: a.sheet.or(b.sheet).map(Cow::Owned),
                        sheet2: None,
                        a_row: a.row,
                        a_col: a.col,
                        b_row: b.row,
                        b_col: b.col,
                        a_row_abs: a.row_abs,
                        a_col_abs: a.col_abs,
                        b_row_abs: b.row_abs,
                        b_col_abs: b.col_abs,
                    })
                })
            }
            Node::ScopedName {
                book: None,
                sheet: Some(sh),
                name,
            } => {
                let target = self.sheet_index(Some(sh), si).ok()?;
                self.defined_name_index(name, target, true).and_then(|idx| {
                    let src = self.wb.names[idx].2.trim_start_matches('=');
                    let parsed = compile_at(src, self.cur.0, self.cur.1).ok()?;
                    let (a, b) = single_sheet_range(&parsed)?;
                    let sheet = a
                        .sheet
                        .or(b.sheet)
                        .unwrap_or_else(|| self.wb.sheets[target].name.clone());
                    Some(ResolvedRange {
                        book: a.book.or(b.book).map(Cow::Owned),
                        sheet: Some(Cow::Owned(sheet)),
                        sheet2: None,
                        a_row: a.row,
                        a_col: a.col,
                        b_row: b.row,
                        b_col: b.col,
                        a_row_abs: a.row_abs,
                        a_col_abs: a.col_abs,
                        b_row_abs: b.row_abs,
                        b_col_abs: b.col_abs,
                    })
                })
            }
            _ => None,
        }
    }

    fn sumif_amount_node_shifted(&self, args: &[Node]) -> Option<Node> {
        let [criteria, _, amounts] = args else { return None };
        let (height, width) = match criteria {
            Node::Range(a, b) => (
                (self.r_row(a) - self.r_row(b)).abs(),
                (self.r_col(a) - self.r_col(b)).abs(),
            ),
            Node::Ref(_) => (0, 0),
            _ => return None,
        };
        let start = match amounts {
            Node::Ref(a) => RefExpr {
                row: self.r_row(a),
                col: self.r_col(a),
                row_abs: true,
                col_abs: true,
                ..a.clone()
            },
            Node::Range(a, b) => RefExpr {
                row: self.r_row(a).min(self.r_row(b)),
                col: self.r_col(a).min(self.r_col(b)),
                row_abs: true,
                col_abs: true,
                ..a.clone()
            },
            _ => return None,
        };
        let end = RefExpr {
            row: (start.row + height).min(1_048_575),
            col: (start.col + width).min(16_383),
            row_abs: true,
            col_abs: true,
            ..start.clone()
        };
        Some(Node::Range(start, end))
    }

    fn resolve_sumif_amount<'n>(&self, args: &'n [Node], si: usize) -> Option<ResolvedRange<'n>> {
        let [criteria, _, amounts] = args else { return None };
        let (height, width) = match criteria {
            Node::Range(a, b) => (
                (self.r_row(a) - self.r_row(b)).abs(),
                (self.r_col(a) - self.r_col(b)).abs(),
            ),
            Node::Ref(_) => (0, 0),
            _ => return self.resolve_range_node(amounts, si),
        };
        let base = self.resolve_range_node(amounts, si)?;
        let r1 = base.r1();
        let c1 = base.c1();
        let r2 = (r1 + height).min(1_048_575);
        let c2 = (c1 + width).min(16_383);
        Some(ResolvedRange {
            book: base.book,
            sheet: base.sheet,
            sheet2: base.sheet2,
            a_row: r1,
            a_col: c1,
            b_row: r2,
            b_col: c2,
            a_row_abs: true,
            a_col_abs: true,
            b_row_abs: true,
            b_col_abs: true,
        })
    }

    pub fn eval_node(&mut self, node: &Node, si: usize) -> FValue {
        if self.requested.is_some() { return FValue::Error(ErrKind::Ref); }
        match node {
            Node::Empty => FValue::Empty,
            Node::Name(n) => {
                if !self.env_stack.is_empty()
                    && let Some(v) = self.env_stack.iter().rev().find_map(|m| m.get(n).cloned())
                {
                    return v;
                }
                self.eval_defined_name(n, si, false)
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
                        return self.eval_defined_name(name, si2, true);
                    }
                    if let Some(li) = crate::xlinks::resolve_book(self.wb, sh) {
                        return self.eval_ext_defined_name(li, None, name, si);
                    }
                }
                FValue::Error(ErrKind::Name)
            }
            Node::Number(n) => FValue::Number(*n),
            Node::Text(s) => FValue::Text(s.clone()),
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
                    let row = self.r_row(r).max(0) as u32;
                    let col = self.r_col(r).max(0) as u32;
                    let mut vals = Vec::with_capacity(idxs.len());
                    for idx in idxs {
                        vals.push(self.cell_value(idx, row, col).scalar());
                    }
                    return FValue::Array(std::sync::Arc::new(value::Array::new(vals.len(), 1, vals)));
                }
                if r.book.is_some() {
                    return self.link_cell(r);
                }
                let row = self.r_row(r);
                let col = self.r_col(r);
                match self.sheet_index(r.sheet.as_deref(), si) {
                    Ok(idx) if row >= 0 && col >= 0 => {
                        self.cell_value(idx, row as u32, col as u32).scalar()
                    }
                    _ => FValue::Error(ErrKind::Ref),
                }
            }
            Node::Spill(r) => {
                if r.book.is_some() {
                    return FValue::Error(ErrKind::Ref);
                }
                let row = self.r_row(r);
                let col = self.r_col(r);
                match self.sheet_index(r.sheet.as_deref(), si) {
                    Ok(idx) if row >= 0 && col >= 0 => {
                        self.spill_value(idx, row as u32, col as u32)
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
                    let (ar, ac) = (self.r_row(a), self.r_col(a));
                    let (br, bc) = (self.r_row(b), self.r_col(b));
                    let mut vals = Vec::new();
                    let mut rows = 0usize;
                    let mut cols = 0usize;
                    for idx in idxs {
                        match self.range_values_coords(idx, ar, ac, br, bc) {
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
                    FValue::Array(std::sync::Arc::new(value::Array::new(rows.max(1), cols.max(1), vals)))
                } else {
                    match self.range_values(a, b, si) {
                        Ok(Arg::Range { values, rows, cols }) => {
                            if rows == 1 && cols == 1 {
                                values.first().cloned().unwrap_or(FValue::Empty)
                            } else {
                                FValue::Array(std::sync::Arc::new(value::Array::from_shared(
                                    rows,
                                    cols,
                                    values,
                                )))
                            }
                        }
                        Ok(_) => FValue::Error(ErrKind::Value),
                        Err(e) => FValue::Error(e),
                    }
                }
            }
            Node::Unary(neg, e) => {
                self.eval_numeric_unary(e, si, |n| if *neg { -n } else { n })
            }
            Node::Percent(e) => {
                self.eval_numeric_unary(e, si, |n| n / 100.0)
            }
            Node::Binary(op, l, r) => self.eval_binary(*op, l, r, si),
            Node::Array(rows, cols, data) => {
                FValue::Array(std::sync::Arc::new(value::Array::new(*rows, *cols, data.iter().map(to_fvalue).collect())))
            }
            Node::Func(name, nodes) => self.eval_func(name, nodes, si),
        }
    }

    fn eval_func(&mut self, name: &FuncName, nodes: &[Node], si: usize) -> FValue {
        let id = name.id();
        match id {
            FuncId::If if nodes.len() >= 2 => {
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
            FuncId::IfError if nodes.len() >= 2 => {
                let v = self.eval_node(&nodes[0], si);
                if v.is_error() {
                    self.eval_node(&nodes[1], si)
                } else {
                    v
                }
            }
            // LET(x, 1, y, x+1, y*2)：使用内联小帧（<= 4 绑定零堆分配）
            FuncId::Let if nodes.len() >= 3 => {
                self.env_stack.push(value::EnvFrame::new_small());
                let mut i = 0;
                while i + 2 < nodes.len() {
                    if let Node::Name(nm) = &nodes[i] {
                        let v = match &nodes[i + 1] {
                            Node::Range(a, b) => match self.range_values(a, b, si) {
                                Ok(Arg::Range { values, rows, cols }) => FValue::Array(
                                    std::sync::Arc::new(value::Array::from_shared(rows, cols, values)),
                                ),
                                Ok(Arg::Val(v)) => v,
                                Err(e) => FValue::Error(e),
                            },
                            other => self.eval_node(other, si),
                        };
                        self.env_stack.last_mut().unwrap().insert(nm.as_str(), v);
                    }
                    i += 2;
                }
                let out = self.eval_node(nodes.last().unwrap(), si);
                self.env_stack.pop();
                out
            }
            FuncId::IsFormula | FuncId::FormulaText => {
                let Some(Node::Ref(re)) = nodes.first() else {
                    return FValue::Error(ErrKind::Value);
                };
                match self.sheet_index(re.sheet.as_deref(), si) {
                    Ok(idx) => {
                        let r = self.r_row(re).max(0) as u32;
                        let c = self.r_col(re).max(0) as u32;
                        let formula = self.wb.sheets[idx].formula_of(r, c);
                        match formula {
                            Some(f) if id == FuncId::FormulaText => {
                                FValue::Text(format!("={}", f.trim_start_matches('=')).into())
                            }
                            Some(_) => FValue::Bool(true),
                            None if id == FuncId::FormulaText => FValue::Error(ErrKind::Na),
                            None => FValue::Bool(false),
                        }
                    }
                    Err(e) => FValue::Error(e),
                }
            }
            FuncId::Sheet => {
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
                    None | Some(_) => FValue::number((si + 1) as f64),
                }
            }
            FuncId::Sheets => {
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
            // ============ LAMBDA 家族
            FuncId::Map | FuncId::Reduce | FuncId::Scan | FuncId::ByRow | FuncId::ByCol | FuncId::MakeArray
                if nodes.len() >= 2 =>
            {
                let lam = nodes.last().unwrap();
                match id {
                    FuncId::Map => {
                        let input = self.operand_block(&nodes[0], si);
                        let vals = input.slice();
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
                        FValue::Array(std::sync::Arc::new(value::Array::new(1, out.len(), out)))
                    }
                    FuncId::Reduce => {
                        let mut acc = self.eval_node(&nodes[0], si);
                        let input = self.operand_block(&nodes[1], si);
                        let vals = input.slice();
                        for v in vals {
                            acc = self.apply_lambda(lam, vec![acc, v.clone()], si);
                        }
                        acc
                    }
                    FuncId::Scan => {
                        let init = self.eval_node(&nodes[0], si);
                        let input = self.operand_block(&nodes[1], si);
                        let vals = input.slice();
                        let mut acc = init;
                        let mut out = Vec::with_capacity(vals.len());
                        for v in vals {
                            acc = self.apply_lambda(lam, vec![acc, v.clone()], si);
                            out.push(acc.clone());
                        }
                        FValue::Array(std::sync::Arc::new(value::Array::new(1, out.len(), out)))
                    }
                    FuncId::MakeArray => {
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
                        FValue::Array(std::sync::Arc::new(value::Array::new(rows, cols, data)))
                    }
                    FuncId::ByRow => {
                        let (rows, cols, data) = match nodes.first() {
                            Some(n) => self.grid_of(n, si),
                            None => (0, 0, Default::default()),
                        };
                        if data.len() == 1 {
                            if let FValue::Error(e) = data[0] {
                                return FValue::Error(e);
                            }
                        }
                        let mut out = Vec::with_capacity(rows);
                        for r in 0..rows {
                            let row: Vec<FValue> = (0..cols).map(|c| data[r * cols + c].clone()).collect();
                            let arg = FValue::Array(std::sync::Arc::new(value::Array::new(1, cols, row)));
                            out.push(self.apply_lambda(lam, vec![arg], si));
                        }
                        FValue::Array(std::sync::Arc::new(value::Array::new(rows.max(1), 1, out)))
                    }
                    _ => {
                        let (rows, cols, data) = match nodes.first() {
                            Some(n) => self.grid_of(n, si),
                            None => (0, 0, Default::default()),
                        };
                        if data.len() == 1 {
                            if let FValue::Error(e) = data[0] {
                                return FValue::Error(e);
                            }
                        }
                        let mut out = Vec::with_capacity(cols);
                        for c in 0..cols {
                            let col: Vec<FValue> = (0..rows).map(|r| data[r * cols + c].clone()).collect();
                            let arg = FValue::Array(std::sync::Arc::new(value::Array::new(rows, 1, col)));
                            out.push(self.apply_lambda(lam, vec![arg], si));
                        }
                        FValue::Array(std::sync::Arc::new(value::Array::new(1, cols.max(1), out)))
                    }
                }
            }
            FuncId::Lambda if !nodes.is_empty() => {
                let params = nodes[..nodes.len() - 1]
                    .iter()
                    .filter_map(|p| match p {
                        Node::Name(n) => Some(n.as_str().into()),
                        _ => None,
                    })
                    .collect();
                let mut body = nodes[nodes.len() - 1].clone();
                if self.ref_offset != (0, 0) {
                    compiled::relocate(&mut body, self.ref_offset.0, self.ref_offset.1);
                }
                FValue::Lambda(std::sync::Arc::new(crate::formula::value::LambdaVal {
                    params,
                    body,
                    env: self.env_stack.clone(),
                }))
            }
            FuncId::InternalCall if !nodes.is_empty() => {
                if let Node::ScopedName { book: None, sheet: Some(sheet), name: scoped_name } = &nodes[0] {
                    let Ok(target) = self.sheet_index(Some(sheet), si) else { return FValue::Error(ErrKind::Ref); };
                    let args = self.function_args(FuncId::Other, &nodes[1..], si);
                    if self.requested.is_some() { return FValue::Error(ErrKind::Ref); }
                    return self.call_named_lambda(scoped_name, target, true, &args);
                }
                let target = self.eval_node(&nodes[0], si);
                match target {
                    FValue::Lambda(l) => {
                        let params = l.params.clone();
                        let body = l.body.clone();
                        let captured = l.env.clone();
                        let mut bound = value::EnvFrame::new_small();
                        for (i, p) in params.iter().enumerate() {
                            let v = match nodes.get(i + 1) {
                                Some(n) => self.eval_node(n, si),
                                None => FValue::Empty,
                            };
                            bound.insert(p.clone(), v);
                        }
                        let saved = std::mem::replace(&mut self.env_stack, captured);
                        let prev_off = self.ref_offset;
                        self.ref_offset = (0, 0);
                        self.env_stack.push(bound);
                        let out = self.eval_node(&body, si);
                        self.env_stack = saved;
                        self.ref_offset = prev_off;
                        out
                    }
                    _ => FValue::Error(ErrKind::Value),
                }
            }
            FuncId::IsOmitted => match nodes.first() {
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
            FuncId::Cell => {
                let info = match nodes.first() {
                    Some(n) => self.eval_node(n, si).to_text().unwrap_or_default().to_lowercase(),
                    None => return FValue::Error(ErrKind::Value),
                };
                let (r, c, sheet) = match nodes.get(1) {
                    Some(Node::Ref(re)) => (
                        self.r_row(re).max(0) as u32,
                        self.r_col(re).max(0) as u32,
                        re.sheet.as_deref(),
                    ),
                    Some(Node::Range(a, _)) => (
                        self.r_row(a).max(0) as u32,
                        self.r_col(a).max(0) as u32,
                        a.sheet.as_deref(),
                    ),
                    _ => (self.cur.0, self.cur.1, None),
                };
                let idx = match self.sheet_index(sheet, si) {
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
                    "prefix" | "format" => FValue::Text("".into()),
                    "protect" => FValue::number(0.0),
                    "parentheses" => FValue::number(0.0),
                    "filename" | "color" => FValue::Text("".into()),
                    _ => FValue::Error(ErrKind::Value),
                }
            }
            FuncId::Areas => match nodes.first() {
                Some(Node::Range(..)) | Some(Node::Ref(_)) => FValue::number(1.0),
                _ => FValue::Error(ErrKind::Value),
            },
            FuncId::Info => {
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
                    "directory" | "memavail" | "memused" | "totmem" => FValue::Text("".into()),
                    _ => FValue::Error(ErrKind::Value),
                }
            }
            FuncId::Row | FuncId::Column => {
                let (r, c) = match nodes.first() {
                    Some(Node::Ref(re)) => (self.r_row(re).max(0) as u32, self.r_col(re).max(0) as u32),
                    Some(Node::Range(a, _)) => (self.r_row(a).max(0) as u32, self.r_col(a).max(0) as u32),
                    _ => self.cur,
                };
                if id == FuncId::Row {
                    FValue::number((r + 1) as f64)
                } else {
                    FValue::number((c + 1) as f64)
                }
            }
            FuncId::Indirect => {
                let txt = match nodes.first() {
                    Some(n) => self.eval_node(n, si).to_text().unwrap_or_default(),
                    None => String::new(),
                };
                let (sheet, rest) = match txt.split_once('!') {
                    Some((sh, r)) => (Some(sh.trim_start_matches('\'').trim_end_matches('\'')), r),
                    None => (None, txt.as_str()),
                };
                if let Some((a, b)) = rest.split_once(':') {
                    match (parse_a1(a.trim()), parse_a1(b.trim())) {
                        (Some(ra), Some(rb)) => {
                            let idx = match self.sheet_index(sheet, si) {
                                Ok(i) => i,
                                Err(e) => return FValue::Error(e),
                            };
                            if let Some(dw) = self.deps_watch {
                                dw.record_dynamic_watch(
                                    (self.cur_sheet, self.cur.0, self.cur.1),
                                    idx,
                                    ra.row,
                                    ra.col,
                                    rb.row,
                                    rb.col,
                                );
                            }
                            match self.range_values_coords(idx, ra.row, ra.col, rb.row, rb.col) {
                                Ok(Arg::Range { values, rows, cols }) => FValue::Array(
                                    std::sync::Arc::new(value::Array::from_shared(rows, cols, values)),
                                ),
                                Ok(_) => FValue::Error(ErrKind::Value),
                                Err(e) => FValue::Error(e),
                            }
                        }
                        _ => FValue::Error(ErrKind::Ref),
                    }
                } else {
                    match parse_a1(rest.trim()) {
                        Some(re) => match self.sheet_index(sheet, si) {
                            Ok(idx) if re.row >= 0 && re.col >= 0 => {
                                if let Some(dw) = self.deps_watch {
                                    dw.record_dynamic_watch(
                                        (self.cur_sheet, self.cur.0, self.cur.1),
                                        idx,
                                        re.row,
                                        re.col,
                                        re.row,
                                        re.col,
                                    );
                                }
                                self.cell_value(idx, re.row as u32, re.col as u32).scalar()
                            }
                            _ => FValue::Error(ErrKind::Ref),
                        },
                        None => FValue::Error(ErrKind::Ref),
                    }
                }
            }
            FuncId::Offset if nodes.len() >= 3 => {
                let (base_sheet, base_r, base_c, base_h, base_w) = match nodes.first() {
                    Some(Node::Ref(re)) => (re.sheet.as_deref(), self.r_row(re), self.r_col(re), 1i64, 1i64),
                    Some(Node::Range(a, b)) => {
                        let (ar, ac) = (self.r_row(a), self.r_col(a));
                        let (br, bc) = (self.r_row(b), self.r_col(b));
                        (a.sheet.as_deref(), ar, ac, (br - ar).abs() + 1, (bc - ac).abs() + 1)
                    }
                    _ => return FValue::Error(ErrKind::Ref),
                };
                let dr = self.eval_node(&nodes[1], si).to_number().unwrap_or(0.0) as i64;
                let dc = self.eval_node(&nodes[2], si).to_number().unwrap_or(0.0) as i64;
                let h = if nodes.len() > 3 {
                    self.eval_node(&nodes[3], si).to_number().unwrap_or(1.0) as i64
                } else {
                    base_h
                };
                let w = if nodes.len() > 4 {
                    self.eval_node(&nodes[4], si).to_number().unwrap_or(1.0) as i64
                } else {
                    base_w
                };
                let r0 = base_r + dr;
                let c0 = base_c + dc;
                if r0 < 0 || c0 < 0 || h <= 0 || w <= 0 {
                    return FValue::Error(ErrKind::Ref);
                }
                let idx = match self.sheet_index(base_sheet, si) {
                    Ok(i) => i,
                    Err(e) => return FValue::Error(e),
                };
                if let Some(dw) = self.deps_watch {
                    dw.record_dynamic_watch(
                        (self.cur_sheet, self.cur.0, self.cur.1),
                        idx,
                        r0,
                        c0,
                        r0 + h - 1,
                        c0 + w - 1,
                    );
                }
                match self.range_values_coords(idx, r0, c0, r0 + h - 1, c0 + w - 1) {
                    Ok(Arg::Range { values, rows, cols }) => FValue::Array(
                        std::sync::Arc::new(value::Array::from_shared(rows, cols, values)),
                    ),
                    _ => FValue::Error(ErrKind::Ref),
                }
            }
            FuncId::Address if nodes.len() >= 2 => {
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
                    body.into()
                } else {
                    format!("{}!{}", sheet, body).into()
                })
            }
            FuncId::Ifs => {
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
            // ============ INDEX O(1) / 单行单列快速路径：零整表物化
            FuncId::Index
                if (2..=3).contains(&nodes.len())
                    && (self.env_stack.is_empty()
                        || !self.env_stack.iter().rev().any(|m| m.contains_key("INDEX"))) =>
            {
                if let Some(rr) = self.resolve_range_node(&nodes[0], si) {
                    return self.eval_index_fast(&rr, nodes, si);
                }
                self.call_generic_id(id, name.as_str(), nodes, si)
            }
            // ============ 查找函数快速路径：零 `RefExpr`/`String` 克隆
            FuncId::VLookup | FuncId::HLookup | FuncId::Match | FuncId::XMatch | FuncId::XLookup
                if nodes.len() >= 2
                    && (self.env_stack.is_empty()
                        || !self.env_stack.iter().rev().any(|m| m.contains_key(name.as_str()))) =>
            {
                let key = self.eval_node(&nodes[0], si).scalar();
                if let FValue::Error(e) = key {
                    return FValue::Error(e);
                }
                let Some(lr) = self.resolve_range_node(&nodes[1], si) else {
                    return self.call_generic_id(id, name.as_str(), nodes, si);
                };
                self.eval_lookup_fast(id, key, lr, nodes, si)
            }
            // ============ SUMPRODUCT 列式零拷贝切片点积快路径
            FuncId::SumProduct
                if (2..=4).contains(&nodes.len())
                    && (self.env_stack.is_empty()
                        || !self.env_stack.iter().rev().any(|m| m.contains_key("SUMPRODUCT"))) =>
            {
                if let Some(v) = self.try_stream_sumproduct(nodes, si) {
                    return v;
                }
                self.call_generic_id(id, name.as_str(), nodes, si)
            }
            // ============ 区域聚合缓存 + 零物化流式 COUNTIF / 单列及二维/多参聚合
            FuncId::Sum
            | FuncId::SumIf
            | FuncId::CountIf
            | FuncId::Count
            | FuncId::CountA
            | FuncId::Average
            | FuncId::Max
            | FuncId::Min
                if !nodes.is_empty()
                    && (self.env_stack.is_empty()
                        || !self.env_stack.iter().rev().any(|m| m.contains_key(name.as_str()))) =>
            {
                let simple = matches!(
                    id,
                    FuncId::Sum
                        | FuncId::Count
                        | FuncId::CountA
                        | FuncId::Average
                        | FuncId::Max
                        | FuncId::Min
                );
                if simple && nodes.len() > 1 {
                    if let Some(streamed) = self.try_stream_multi_arg_agg(id, nodes, si) {
                        return streamed;
                    }
                    return self.call_generic_id(id, name.as_str(), nodes, si);
                }
                let ok_arity = if simple { nodes.len() == 1 } else { nodes.len() >= 2 };
                if ok_arity {
                    if let Some(rr) = self.resolve_range_node(&nodes[0], si) {
                        if rr.is_3d() || rr.is_ext() {
                            return self.call_generic_id(id, name.as_str(), nodes, si);
                        }
                        let si2 = match self.sheet_index(rr.sheet.as_deref(), si) {
                            Ok(i) => i,
                            Err(e) => return FValue::Error(e),
                        };
                        if id == FuncId::Sum
                            && matches!(nodes[0], Node::Range(..))
                            && let Some((key, end)) = self.prefix_range_resolved(si2, &rr)
                        {
                            let start = profile_enabled().then(std::time::Instant::now);
                            let result = self.prefix_sum(key, end);
                            if let Some(t) = start { prof_record("SUM#prefix", t.elapsed().as_secs_f64()); }
                            return result;
                        }
                        let crit = if simple {
                            String::new()
                        } else {
                            match self.eval_node(&nodes[1], si).to_text() {
                                Ok(value) => value,
                                Err(error) => return FValue::Error(error),
                            }
                        };
                        let sum_spec = if id == FuncId::SumIf && nodes.len() >= 3 {
                            match self.resolve_sumif_amount(nodes, si) {
                                Some(sr) if !sr.is_3d() && !sr.is_ext() => {
                                    match self.sheet_index(sr.sheet.as_deref(), si) {
                                        Ok(sidx) => Some((sidx, sr.r1(), sr.c1(), sr.r2(), sr.c2())),
                                        Err(e) => return FValue::Error(e),
                                    }
                                }
                                _ => return self.call_generic_id(id, name.as_str(), nodes, si),
                            }
                        } else {
                            None
                        };
                        let range_tuple = (si2, rr.r1(), rr.c1(), rr.r2(), rr.c2());
                        let key = AggCacheKey {
                            range: range_tuple,
                            sum_range: sum_spec,
                            func: match id {
                                FuncId::Sum => 1u8,
                                FuncId::SumIf => 2,
                                FuncId::CountIf => 3,
                                FuncId::CountA => 4,
                                FuncId::Average => 5,
                                FuncId::Max => 6,
                                FuncId::Min => 7,
                                _ => 8,
                            },
                            crit,
                        };
                        if let Some(v) = self.agg_cache.get(&key) {
                            self.agg_hits += 1;
                            return v.clone();
                        }
                        self.agg_miss += 1;
                        let t0 = profile_enabled().then(std::time::Instant::now);
                        if simple
                            && let Some(streamed) = self.try_stream_single_col_agg(id, si2, rr.r1(), rr.c1(), rr.r2(), rr.c2())
                        {
                            if let Some(start) = t0 {
                                prof_record(&format!("{name}#agg"), start.elapsed().as_secs_f64());
                            }
                            if self.requested.is_none() {
                                self.agg_cache.insert(key, streamed.clone());
                            }
                            return streamed;
                        }
                        let out = match id {
                            FuncId::Sum
                            | FuncId::Count
                            | FuncId::CountA
                            | FuncId::Average
                            | FuncId::Max
                            | FuncId::Min => {
                                let (rows, cols, data) = self.cached_grid_coords(si2, rr.r1(), rr.c1(), rr.r2(), rr.c2());
                                let range = Arg::Range { values: data, rows, cols };
                                functions::call_id_in(self.ctx, id, name.as_str(), &[range])
                            }
                            FuncId::CountIf => {
                                let c = functions::Crit::parse(&key.crit);
                                let gk = self.grid_key_coords(si2, rr.r1(), rr.c1(), rr.r2(), rr.c2()).map(|mut g| {
                                    if g.2 == g.4 && (g.3 - g.1) >= 255 && !c.matches(&FValue::Empty) {
                                        let cm = self.col_max_row(si2, g.2 as u32);
                                        g.3 = aligned_col_end_row(g.1, cm, g.3);
                                    }
                                    g
                                });
                                if self.requested.is_none()
                                    && let Some(gk) = gk
                                    && let Some(ix) = self.count_indexes.lookup_ready(gk, &c)
                                {
                                    FValue::number(ix.count(&c) as f64)
                                } else if (c.num.is_none() || !c.indexable())
                                    && let Some(gk) = gk
                                    && let Some(streamed) = self.try_stream_single_col_countif(si2, gk, &c)
                                {
                                    streamed
                                } else {
                                    let (_, _, data) = self.cached_grid_coords(si2, rr.r1(), rr.c1(), rr.r2(), rr.c2());
                                    let idx_key = gk.unwrap_or(key.range);
                                    let index = self
                                        .requested
                                        .is_none()
                                        .then(|| self.count_indexes.get(idx_key, &data, &c))
                                        .flatten();
                                    let n = index.map_or_else(
                                        || data.iter().filter(|v| c.matches(v)).count(),
                                        |index| index.count(&c),
                                    );
                                    FValue::number(n as f64)
                                }
                            }
                            _ => {
                                // SUMIF: route through persistent SharedCondColumn + OrderedCriteria / sorted_nums / text_dict first!
                                if let Some(fast_sumif) =
                                    self.try_eval_sumif_cond_col(range_tuple, sum_spec, &key.crit)
                                {
                                    if let Some(start) = t0 {
                                        prof_record(&format!("{name}#agg"), start.elapsed().as_secs_f64());
                                    }
                                    if self.requested.is_none() {
                                        self.agg_cache.insert(key, fast_sumif.clone());
                                    }
                                    return fast_sumif;
                                }
                                let c = functions::Crit::parse(&key.crit);
                                let gk = self.grid_key_coords(si2, rr.r1(), rr.c1(), rr.r2(), rr.c2());
                                // Fast zero-materialization path for text criteria on pure-numeric columns:
                                if self.requested.is_none()
                                    && c.num.is_none()
                                    && c.indexable()
                                    && let Some(gk_val) = gk
                                {
                                    if self.count_indexes.lookup_ready(gk_val, &c).is_none() {
                                        let _ = self.try_stream_single_col_countif(si2, gk_val, &c);
                                    }
                                    if let Some(ix) = self.count_indexes.lookup_ready(gk_val, &c)
                                        && ix.count(&c) == 0
                                    {
                                        if let Some(start) = t0 {
                                            prof_record(&format!("{name}#agg"), start.elapsed().as_secs_f64());
                                        }
                                        self.agg_cache.insert(key, FValue::number(0.0));
                                        return FValue::number(0.0);
                                    }
                                }
                                let (_, cols, data) = self.cached_grid_coords(si2, rr.r1(), rr.c1(), rr.r2(), rr.c2());
                                let amounts = sum_spec.map(|(sidx, sr1, sc1, sr2, sc2)| {
                                    self.cached_grid_coords(sidx, sr1, sc1, sr2, sc2)
                                });
                                let mut t = 0.0;
                                let mut add = |i: usize| {
                                    let amount = match &amounts {
                                        Some((amount_rows, amount_cols, values)) => {
                                            let (row, col) = (i / cols, i % cols);
                                            if row >= *amount_rows || col >= *amount_cols { return Ok(()); }
                                            &values[row * amount_cols + col]
                                        }
                                        None => &data[i],
                                    };
                                    match amount {
                                        FValue::Number(n) => t += n,
                                        FValue::Error(e) => return Err(*e),
                                        _ => {}
                                    }
                                    Ok(())
                                };
                                let idx_key = gk.unwrap_or(key.range);
                                let index = self
                                    .requested
                                    .is_none()
                                    .then(|| self.count_indexes.get(idx_key, &data, &c))
                                    .flatten();
                                let sum = if let Some(mut rows) = index.and_then(|ix| ix.rows(&c)) {
                                    rows.try_for_each(&mut add)
                                } else {
                                    (0..data.len()).filter(|&i| c.matches(&data[i])).try_for_each(add)
                                };
                                sum.map(|()| FValue::number(t)).unwrap_or_else(FValue::Error)
                            }
                        };
                        if let Some(start) = t0 { prof_record(&format!("{name}#agg"), start.elapsed().as_secs_f64()); }
                        if self.requested.is_none() { self.agg_cache.insert(key, out.clone()); }
                        return out;
                    }
                }
                self.call_generic_id(id, name.as_str(), nodes, si)
            }
            _ => self.eval_func_generic(name, nodes, si),
        }
    }

    fn eval_func_generic(&mut self, name: &FuncName, nodes: &[Node], si: usize) -> FValue {
        let id = name.id();
        // 名字被 LET 绑定成函数时优先当函数调用
        let bound = if self.env_stack.is_empty() {
            None
        } else {
            self.env_stack
                .iter()
                .rev()
                .find_map(|m| m.get(name.as_str()).cloned())
        };
        if let Some(FValue::Lambda(lam)) = bound {
            let mut vals = Vec::with_capacity(nodes.len());
            for n in nodes.iter() {
                vals.push(self.eval_node(n, si));
            }
            return self.apply_lambda_val(&lam, vals, si);
        }
        if bound.is_none()
            && matches!(
                id,
                FuncId::SumIfs
                    | FuncId::CountIfs
                    | FuncId::AverageIf
                    | FuncId::AverageIfs
                    | FuncId::MinIfs
                    | FuncId::MaxIfs
            )
        {
            let t0 = profile_enabled().then(std::time::Instant::now);
            if let Some(fast) = self.try_eval_criteria_lazy(id, name.as_str(), nodes, si) {
                if let Some(start) = t0 {
                    prof_record(name.as_str(), start.elapsed().as_secs_f64());
                }
                return fast;
            }
        }
        let args = self.function_args(id, nodes, si);
        if self.requested.is_some() { return FValue::Error(ErrKind::Ref); }
        let t0 = profile_enabled().then(std::time::Instant::now);
        let out = if id.is_nondeterministic_volatile() {
            let cell = (self.cur_sheet, self.cur.0, self.cur.1);
            let count = self.volatile_counts.entry(cell).or_default();
            let seq = *count;
            *count += 1;
            let key = VolatileKey {
                cell,
                seq,
                func: id,
                args: args.iter().map(VolatileArgKey::from_arg).collect(),
            };
            if let Some(value) = self.volatile_values.get(&key) {
                value.clone()
            } else {
                let value = functions::call_id_in(self.ctx, id, name.as_str(), &args);
                self.volatile_values.insert(key, value.clone());
                value
            }
        } else {
            functions::call_by_id_in(self.ctx, id, name.as_str(), &args, &mut || {
                if bound.is_some() { FValue::Error(ErrKind::Value) }
                else { self.call_named_lambda(name.as_str(), si, false, &args) }
            })
        };
        if let Some(start) = t0 { prof_record(name.as_str(), start.elapsed().as_secs_f64()); }
        out
    }

    fn eval_index_fast(&mut self, rr: &ResolvedRange<'_>, nodes: &[Node], si: usize) -> FValue {
        let row_raw = match self.eval_node(&nodes[1], si).to_number() {
            Ok(v) => v,
            Err(e) => return FValue::Error(e),
        };
        if row_raw < 0.0 || !row_raw.is_finite() {
            return FValue::Error(ErrKind::Value);
        }
        let row = row_raw.trunc() as usize;
        let has_col = nodes.len() > 2;
        let col = if has_col {
            match self.eval_node(&nodes[2], si).to_number() {
                Ok(v) if v >= 0.0 && v.is_finite() => v.trunc() as usize,
                Ok(_) => return FValue::Error(ErrKind::Value),
                Err(e) => return FValue::Error(e),
            }
        } else {
            0
        };
        let r1 = rr.r1();
        let c1 = rr.c1();
        let r2 = rr.r2();
        let c2 = rr.c2();
        if r1 < 0 || c1 < 0 || r2 < r1 || c2 < c1 {
            return FValue::Error(ErrKind::Ref);
        }
        let raw_rows = (r2 - r1 + 1) as usize;
        let raw_cols = (c2 - c1 + 1) as usize;
        let book = rr.book.as_deref();
        let sheet = rr.sheet.as_deref();
        let read_single = |this: &mut Self, tr: i64, tc: i64| -> FValue {
            if book.is_some() {
                this.link_cell_at(book, sheet, tr, tc)
            } else {
                match this.sheet_index(sheet, si) {
                    Ok(idx) => this.cell_value(idx, tr as u32, tc as u32).scalar(),
                    Err(e) => FValue::Error(e),
                }
            }
        };
        let read_slice = |this: &mut Self, sr1: i64, sc1: i64, sr2: i64, sc2: i64| -> FValue {
            let res = if book.is_some() {
                this.link_range_coords(book, sheet, sr1, sc1, sr2, sc2)
            } else {
                match this.sheet_index(sheet, si) {
                    Ok(idx) => this.range_values_coords(idx, sr1, sc1, sr2, sc2),
                    Err(e) => Err(e),
                }
            };
            match res {
                Ok(Arg::Range { values, rows, cols }) => {
                    if rows <= 1 && cols <= 1 {
                        values.first().cloned().unwrap_or(FValue::Empty)
                    } else {
                        FValue::Array(std::sync::Arc::new(value::Array::from_shared(
                            rows, cols, values,
                        )))
                    }
                }
                Ok(Arg::Val(v)) => v,
                Err(e) => FValue::Error(e),
            }
        };

        // 1-row horizontal vector with 2 arguments: INDEX(A1:Z1, k) indexes column k-1
        if !has_col && raw_rows == 1 && raw_cols > 1 {
            if row == 0 {
                return match self.range_values_resolved(rr, si) {
                    Ok(Arg::Range { values, rows, cols }) => FValue::Array(std::sync::Arc::new(
                        value::Array::from_shared(rows, cols, values),
                    )),
                    Ok(Arg::Val(v)) => v,
                    Err(e) => FValue::Error(e),
                };
            }
            if row > raw_cols {
                return FValue::Error(ErrKind::Ref);
            }
            return read_single(self, r1, c1 + (row - 1) as i64);
        }

        // Entire column (row == 0) or entire area (row == 0 && col == 0)
        if row == 0 {
            if has_col && col == 0 {
                return read_slice(self, r1, c1, r2, c2);
            }
            let target_c = if has_col {
                if col > raw_cols {
                    return FValue::Error(ErrKind::Ref);
                }
                col - 1
            } else {
                0
            };
            if raw_rows == 1 {
                return read_single(self, r1, c1 + target_c as i64);
            }
            let tc = c1 + target_c as i64;
            return read_slice(self, r1, tc, r2, tc);
        }

        // Entire row (has_col && col == 0)
        if has_col && col == 0 {
            if row > raw_rows {
                return FValue::Error(ErrKind::Ref);
            }
            if raw_cols == 1 {
                return read_single(self, r1 + (row - 1) as i64, c1);
            }
            let tr = r1 + (row - 1) as i64;
            return read_slice(self, tr, c1, tr, c2);
        }

        // Standard O(1) single-cell INDEX(range, row, [col]) — zero range materialization!
        let target_c = if has_col { col - 1 } else { 0 };
        if row > raw_rows || target_c >= raw_cols {
            return FValue::Error(ErrKind::Ref);
        }
        read_single(self, r1 + (row - 1) as i64, c1 + target_c as i64)
    }

    fn eval_lookup_fast(
        &mut self,
        id: FuncId,
        key: FValue,
        lr: ResolvedRange<'_>,
        nodes: &[Node],
        si: usize,
    ) -> FValue {
        let is_ext = lr.is_ext();
        let has_wildcard = matches!(&key, FValue::Text(t) if functions::is_wildcard(t));
        let ext_target = if is_ext {
            let book = lr.book.as_deref().unwrap_or("");
            let Some(li) = crate::xlinks::resolve_book(self.wb, book) else {
                return FValue::Error(ErrKind::Ref);
            };
            if self.wb.links[li].cache.is_empty() {
                return FValue::Error(ErrKind::Ref);
            }
            let Some(slot) = crate::xlinks::sheet_slot(&self.wb.links[li], lr.sheet.as_deref()) else {
                return FValue::Error(ErrKind::Ref);
            };
            Some((li, slot))
        } else {
            None
        };
        let idx = if !is_ext {
            match self.sheet_index(lr.sheet.as_deref(), si) {
                Ok(i) => i,
                Err(e) => return FValue::Error(e),
            }
        } else {
            0
        };
        let (lr1, lc1, lr2, lc2) = (lr.r1(), lr.c1(), lr.r2(), lr.c2());

        if id == FuncId::VLookup || id == FuncId::HLookup {
            let vertical = id == FuncId::VLookup;
            let col_idx = match nodes.get(2) {
                Some(n) => match self.eval_node(n, si) {
                    FValue::Error(e) => return FValue::Error(e),
                    v => v.to_number().unwrap_or(0.0) as usize,
                },
                None => return FValue::Error(ErrKind::Value),
            };
            let approx = match nodes.get(3) {
                Some(n) => match self.eval_node(n, si) {
                    FValue::Error(e) => return FValue::Error(e),
                    v => v.to_bool().unwrap_or(true),
                },
                None => true,
            };
            let (max_r, max_c) = if let Some((li, slot)) = ext_target {
                self.ext_sheet_bounds(li, slot)
            } else {
                self.sheet_bounds(idx)
            };
            let (r1, c1) = (lr1, lc1);
            let mut br = lr2;
            let mut bc = lc2;
            if r1 < 0 || c1 < 0 {
                return FValue::Error(ErrKind::Ref);
            }
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
            let (sr1, sc1, mut sr2, sc2) = if vertical {
                let mut eff_sr2 = lr2;
                if (r1 == 0 && eff_sr2 == 1_048_575)
                    || (eff_sr2 - r1 + 1).saturating_mul(lc2 - c1 + 1) > 200_000
                {
                    eff_sr2 = eff_sr2.min(max_r as i64).max(r1);
                }
                (r1, c1, eff_sr2, c1)
            } else {
                (r1, c1, r1, lc2)
            };
            if vertical
                && ext_target.is_none()
                && !matches!(key, FValue::Empty)
                && (sr2 - sr1) >= 256
                && c1 >= 0
            {
                let cm = self.col_max_row(idx, c1 as u32);
                sr2 = aligned_col_end_row(sr1, cm, sr2);
            }
            let best = if approx {
                if ext_target.is_some() {
                    let Ok(Arg::Range { values, rows: s_rows, cols: s_cols }) =
                        self.link_range_coords(lr.book.as_deref(), lr.sheet.as_deref(), sr1, sc1, sr2, sc2)
                    else {
                        return FValue::Error(ErrKind::Ref);
                    };
                    let n = if vertical { s_rows } else { s_cols };
                    if n == 0 {
                        return FValue::Error(ErrKind::Na);
                    }
                    functions::xlookup_search(&values, n, 1, None, &key, -1, 2).ok().flatten()
                } else {
                    let (s_rows, s_cols, data) = self.cached_grid_coords(idx, sr1, sc1, sr2, sc2);
                    let n = if vertical { s_rows } else { s_cols };
                    if n == 0 {
                        return FValue::Error(ErrKind::Na);
                    }
                    self.approximate_match((idx, sr1, sc1, sr2, sc2), &data, n, 1, &key, true)
                }
            } else if has_wildcard {
                let data_arc = if ext_target.is_some() {
                    match self.link_range_coords(lr.book.as_deref(), lr.sheet.as_deref(), sr1, sc1, sr2, sc2) {
                        Ok(Arg::Range { values, .. }) => values,
                        _ => return FValue::Error(ErrKind::Ref),
                    }
                } else {
                    self.cached_grid_coords(idx, sr1, sc1, sr2, sc2).2
                };
                functions::xlookup_search(&data_arc, data_arc.len(), 1, None, &key, 2, 1)
                    .ok()
                    .flatten()
            } else {
                let lk = lookup_cache::LookupKey::from_fvalue(&key);
                if let Some((li, slot)) = ext_target {
                    self.ext_lookup_hash_coords(li, slot, sr1, sc1, sr2, sc2, vertical).get_first(&lk)
                } else {
                    self.lookup_hash_coords(idx, sr1, sc1, sr2, sc2, vertical).get_first(&lk)
                }
            };
            return match best {
                Some(i) => {
                    let (tr, tc) = if vertical {
                        (r1 + i as i64, c1 + col_idx as i64 - 1)
                    } else {
                        (r1 + col_idx as i64 - 1, c1 + i as i64)
                    };
                    if let Some((li, slot)) = ext_target {
                        match self.wb.links[li].cache.get(&(slot, tr as u32, tc as u32)) {
                            Some(v) => to_fvalue(v),
                            None => FValue::Empty,
                        }
                    } else {
                        self.cell_value(idx, tr as u32, tc as u32).scalar()
                    }
                }
                None => FValue::Error(ErrKind::Na),
            };
        }

        if id == FuncId::Match || id == FuncId::XMatch {
            let mode = match nodes.get(2) {
                Some(n) if !matches!(n, Node::Empty) => match self.eval_node(n, si).to_number() {
                    Ok(v) => v,
                    Err(e) => return FValue::Error(e),
                },
                None | Some(Node::Empty) if id == FuncId::Match => 1.0,
                _ => 0.0,
            };
            let search_mode = if id == FuncId::XMatch {
                match nodes.get(3) {
                    Some(n) if !matches!(n, Node::Empty) => match self.eval_node(n, si).to_number() {
                        Ok(v) => v.trunc() as i64,
                        Err(e) => return FValue::Error(e),
                    },
                    _ => 1,
                }
            } else {
                1
            };
            let raw_rows = (lr2 - lr1 + 1).max(0) as usize;
            let raw_cols = (lc2 - lc1 + 1).max(0) as usize;
            if raw_rows == 0 || raw_cols == 0 {
                return FValue::Error(ErrKind::Ref);
            }
            let vertical = !(raw_rows == 1 && raw_cols > 1);
            let mut eff_lr2 = lr2;
            if ext_target.is_none() {
                if (lr1 == 0 && eff_lr2 == 1_048_575)
                    || (eff_lr2 - lr1 + 1).saturating_mul(lc2 - lc1 + 1) > 200_000
                {
                    let (max_r, _) = self.sheet_bounds(idx);
                    eff_lr2 = eff_lr2.min(max_r as i64).max(lr1);
                }
                if vertical
                    && lc1 == lc2
                    && lc1 >= 0
                    && !matches!(key, FValue::Empty)
                    && (eff_lr2 - lr1) >= 256
                {
                    let cm = self.col_max_row(idx, lc1 as u32);
                    eff_lr2 = aligned_col_end_row(lr1, cm, eff_lr2);
                }
            }
            if mode == 0.0 && (search_mode == 1 || search_mode == -1) && !(id == FuncId::Match && has_wildcard) {
                let lk = lookup_cache::LookupKey::from_fvalue(&key);
                let hit = if let Some((li, slot)) = ext_target {
                    let ix = self.ext_lookup_hash_coords(li, slot, lr1, lc1, lr2, lc2, vertical);
                    if search_mode == -1 { ix.get_last(&lk) } else { ix.get_first(&lk) }
                } else {
                    if self.grid_key_coords(idx, lr1, lc1, eff_lr2, lc2).is_none() {
                        return FValue::Error(ErrKind::Ref);
                    }
                    let ix = self.lookup_hash_coords(idx, lr1, lc1, eff_lr2, lc2, vertical);
                    if search_mode == -1 { ix.get_last(&lk) } else { ix.get_first(&lk) }
                };
                return hit
                    .map(|i| FValue::number((i + 1) as f64))
                    .unwrap_or(FValue::Error(ErrKind::Na));
            }
            let (rows, cols, data) = if ext_target.is_some() {
                match self.link_range_coords(lr.book.as_deref(), lr.sheet.as_deref(), lr1, lc1, lr2, lc2) {
                    Ok(Arg::Range { values, rows, cols }) => (rows, cols, values),
                    _ => return FValue::Error(ErrKind::Ref),
                }
            } else {
                self.cached_grid_coords(idx, lr1, lc1, eff_lr2, lc2)
            };
            if rows == 0 || cols == 0 {
                return FValue::Error(ErrKind::Ref);
            }
            let n = if vertical { rows } else { cols };
            let stride = if vertical { cols } else { 1 };
            if id == FuncId::Match {
                if mode == 0.0 {
                    return functions::xlookup_search(&data, n, stride, None, &key, 2, 1)
                        .ok()
                        .flatten()
                        .map(|i| FValue::number((i + 1) as f64))
                        .unwrap_or(FValue::Error(ErrKind::Na));
                }
                let hit = if ext_target.is_none() {
                    self.approximate_match((idx, lr1, lc1, eff_lr2, lc2), &data, n, stride, &key, mode > 0.0)
                } else {
                    functions::xlookup_search(
                        &data,
                        n,
                        stride,
                        None,
                        &key,
                        if mode > 0.0 { -1 } else { 1 },
                        if mode > 0.0 { 2 } else { -2 },
                    )
                    .ok()
                    .flatten()
                };
                return hit
                    .map(|i| FValue::number((i + 1) as f64))
                    .unwrap_or(FValue::Error(ErrKind::Na));
            }
            let mm = mode.trunc() as i64;
            let pos = if search_mode.abs() == 2 && ext_target.is_none() {
                self.get_or_build_approx_positions((idx, lr1, lc1, eff_lr2, lc2), &data, n, stride, &key)
            } else {
                None
            };
            return match functions::xlookup_search(&data, n, stride, pos.as_deref().map(Vec::as_slice), &key, mm, search_mode) {
                Ok(Some(i)) => FValue::number((i + 1) as f64),
                Ok(None) => FValue::Error(ErrKind::Na),
                Err(e) => FValue::Error(e),
            };
        }

        // ============ XLOOKUP(lookup_value, lookup_array, return_array, [if_not_found], [match_mode], [search_mode])
        if nodes.len() < 3 {
            return FValue::Error(ErrKind::Value);
        }
        let match_mode = match nodes.get(4) {
            Some(n) if !matches!(n, Node::Empty) => match self.eval_node(n, si).to_number() {
                Ok(v) => v.trunc() as i64,
                Err(e) => return FValue::Error(e),
            },
            _ => 0,
        };
        let search_mode = match nodes.get(5) {
            Some(n) if !matches!(n, Node::Empty) => match self.eval_node(n, si).to_number() {
                Ok(v) => v.trunc() as i64,
                Err(e) => return FValue::Error(e),
            },
            _ => 1,
        };
        if !matches!(match_mode, -1..=2)
            || !matches!(search_mode, -2 | -1 | 1 | 2)
            || (match_mode == 2 && search_mode.abs() == 2)
        {
            return FValue::Error(ErrKind::Value);
        }
        let raw_rows = (lr2 - lr1 + 1).max(0) as usize;
        let raw_cols = (lc2 - lc1 + 1).max(0) as usize;
        if raw_rows == 0 || raw_cols == 0 {
            return FValue::Error(ErrKind::Ref);
        }
        let vertical = !(raw_rows == 1 && raw_cols > 1);
        let mut eff_lr2 = lr2;
        if ext_target.is_none() {
            if (lr1 == 0 && eff_lr2 == 1_048_575)
                || (eff_lr2 - lr1 + 1).saturating_mul(lc2 - lc1 + 1) > 200_000
            {
                let (max_r, _) = self.sheet_bounds(idx);
                eff_lr2 = eff_lr2.min(max_r as i64).max(lr1);
            }
            if vertical
                && lc1 == lc2
                && lc1 >= 0
                && !matches!(key, FValue::Empty)
                && (eff_lr2 - lr1) >= 256
            {
                let cm = self.col_max_row(idx, lc1 as u32);
                eff_lr2 = aligned_col_end_row(lr1, cm, eff_lr2);
            }
        }

        let hit = if match_mode == 0 && (search_mode == 1 || search_mode == -1) {
            let lk = lookup_cache::LookupKey::from_fvalue(&key);
            if let Some((li, slot)) = ext_target {
                let ix = self.ext_lookup_hash_coords(li, slot, lr1, lc1, lr2, lc2, vertical);
                if search_mode == -1 { ix.get_last(&lk) } else { ix.get_first(&lk) }
            } else if self.grid_key_coords(idx, lr1, lc1, eff_lr2, lc2).is_none() {
                None
            } else {
                let ix = self.lookup_hash_coords(idx, lr1, lc1, eff_lr2, lc2, vertical);
                if search_mode == -1 { ix.get_last(&lk) } else { ix.get_first(&lk) }
            }
        } else {
            let (rows, cols, data) = if ext_target.is_some() {
                match self.link_range_coords(lr.book.as_deref(), lr.sheet.as_deref(), lr1, lc1, lr2, lc2) {
                    Ok(Arg::Range { values, rows, cols }) => (rows, cols, values),
                    _ => return FValue::Error(ErrKind::Ref),
                }
            } else {
                self.cached_grid_coords(idx, lr1, lc1, eff_lr2, lc2)
            };
            if rows == 0 || cols == 0 {
                None
            } else {
                let n = if vertical { rows } else { cols };
                let stride = if vertical { cols } else { 1 };
                let pos = if search_mode.abs() == 2 && ext_target.is_none() {
                    self.get_or_build_approx_positions((idx, lr1, lc1, eff_lr2, lc2), &data, n, stride, &key)
                } else {
                    None
                };
                match functions::xlookup_search(
                    &data,
                    n,
                    stride,
                    pos.as_deref().map(Vec::as_slice),
                    &key,
                    match_mode,
                    search_mode,
                ) {
                    Ok(h) => h,
                    Err(e) => return FValue::Error(e),
                }
            }
        };

        let Some(i) = hit else {
            return match nodes.get(3) {
                Some(n) if !matches!(n, Node::Empty) => {
                    let fallback = self.eval_node(n, si);
                    if matches!(fallback, FValue::Empty) {
                        FValue::Error(ErrKind::Na)
                    } else {
                        fallback
                    }
                }
                _ => FValue::Error(ErrKind::Na),
            };
        };

        if let Some(ret_r) = self.resolve_range_node(&nodes[2], si) {
            let rr1 = ret_r.r1();
            let rc1 = ret_r.c1();
            let rr2 = ret_r.r2();
            let rc2 = ret_r.c2();
            if rr1 < 0 || rc1 < 0 || rr2 < rr1 || rc2 < rc1 {
                return FValue::Error(ErrKind::Ref);
            }
            let ret_ext = ret_r.is_ext();
            if vertical {
                let tr = rr1 + i as i64;
                if tr > rr2 {
                    return FValue::Error(ErrKind::Ref);
                }
                if rc1 == rc2 {
                    if ret_ext {
                        return self.link_cell_at(ret_r.book.as_deref(), ret_r.sheet.as_deref(), tr, rc1);
                    }
                    let ridx = match self.sheet_index(ret_r.sheet.as_deref(), si) {
                        Ok(s) => s,
                        Err(e) => return FValue::Error(e),
                    };
                    return self.cell_value(ridx, tr as u32, rc1 as u32).scalar();
                }
                let res = if ret_ext {
                    self.link_range_coords(ret_r.book.as_deref(), ret_r.sheet.as_deref(), tr, rc1, tr, rc2)
                } else {
                    match self.sheet_index(ret_r.sheet.as_deref(), si) {
                        Ok(ridx) => self.range_values_coords(ridx, tr, rc1, tr, rc2),
                        Err(e) => Err(e),
                    }
                };
                return match res {
                    Ok(Arg::Range { values, rows, cols }) => {
                        if rows <= 1 && cols <= 1 {
                            values.first().cloned().unwrap_or(FValue::Empty)
                        } else {
                            FValue::Array(std::sync::Arc::new(value::Array::from_shared(
                                rows, cols, values,
                            )))
                        }
                    }
                    Ok(Arg::Val(v)) => v,
                    Err(e) => FValue::Error(e),
                };
            } else {
                let tc = rc1 + i as i64;
                if tc > rc2 {
                    return FValue::Error(ErrKind::Ref);
                }
                if rr1 == rr2 {
                    if ret_ext {
                        return self.link_cell_at(ret_r.book.as_deref(), ret_r.sheet.as_deref(), rr1, tc);
                    }
                    let ridx = match self.sheet_index(ret_r.sheet.as_deref(), si) {
                        Ok(s) => s,
                        Err(e) => return FValue::Error(e),
                    };
                    return self.cell_value(ridx, rr1 as u32, tc as u32).scalar();
                }
                let res = if ret_ext {
                    self.link_range_coords(ret_r.book.as_deref(), ret_r.sheet.as_deref(), rr1, tc, rr2, tc)
                } else {
                    match self.sheet_index(ret_r.sheet.as_deref(), si) {
                        Ok(ridx) => self.range_values_coords(ridx, rr1, tc, rr2, tc),
                        Err(e) => Err(e),
                    }
                };
                return match res {
                    Ok(Arg::Range { values, rows, cols }) => {
                        if rows <= 1 && cols <= 1 {
                            values.first().cloned().unwrap_or(FValue::Empty)
                        } else {
                            FValue::Array(std::sync::Arc::new(value::Array::from_shared(
                                rows, cols, values,
                            )))
                        }
                    }
                    Ok(Arg::Val(v)) => v,
                    Err(e) => FValue::Error(e),
                };
            }
        }

        // Dynamic/calculated return_array expression
        let ret = self.operand_block(&nodes[2], si);
        let (rows, cols) = functions::range_dims(&ret);
        if vertical {
            if cols <= 1 {
                functions::range_get(&ret, i, 0)
            } else {
                let row_data = (0..cols).map(|c| functions::range_get(&ret, i, c)).collect();
                FValue::Array(std::sync::Arc::new(value::Array::new(1, cols, row_data)))
            }
        } else if rows <= 1 {
            functions::range_get(&ret, 0, i)
        } else {
            let col_data = (0..rows).map(|r| functions::range_get(&ret, r, i)).collect();
            FValue::Array(std::sync::Arc::new(value::Array::new(rows, 1, col_data)))
        }
    }

    fn stream_rect_into_agg(
        &mut self,
        id: FuncId,
        si: usize,
        r1: u32,
        c1: u32,
        r2: u32,
        c2: u32,
        sum: &mut f64,
        num_count: &mut usize,
        non_empty: &mut usize,
        min_v: &mut f64,
        max_v: &mut f64,
        earliest_err: &mut Option<((u32, u32), ErrKind)>,
        reads: &mut usize,
    ) -> bool {
        let Some(sheet) = self.wb.sheets.get(si) else {
            return false;
        };
        let ignore_err = matches!(id, FuncId::Count | FuncId::CountA);
        let rows_per_col = (r2 - r1 + 1) as usize;
        for col in c1..=c2 {
            let pages = sheet.cf_pages((r1, col, r2, col));
            for p in &pages {
                let crate::store::ColSlab::Mixed(cs) = p.cells.as_ref() else {
                    continue;
                };
                for ro in p.start..p.end {
                    let cell = &cs[ro];
                    if cell.formula.is_some() {
                        let abs_row = p.page * crate::store::PAGE_ROWS + ro as u32;
                        let k = (si, abs_row, col);
                        if self.has_memo(k) {
                            continue;
                        }
                        if self.parallel_values.is_some_and(|pv| pv.is_ready(k)) {
                            continue;
                        }
                        let out_of_scope_valid =
                            self.outside_scope(k) && !callable_cache_candidate(&cell.value);
                        if !out_of_scope_valid {
                            return false;
                        }
                    }
                }
            }
            for (pi, p) in pages.iter().enumerate() {
                if (pi & 31) == 31 && self.check_abort() {
                    return false;
                }
                let page_base = p.page * crate::store::PAGE_ROWS;
                match p.cells.as_ref() {
                    crate::store::ColSlab::Numbers(ns) => {
                        let span = p.end - p.start;
                        if id == FuncId::CountA {
                            *non_empty +=
                                crate::store::NumberSlab::present_count_in(&ns.present, p.start, span);
                        } else if id == FuncId::Count {
                            *num_count +=
                                crate::store::NumberSlab::present_count_in(&ns.present, p.start, span);
                        } else if crate::store::NumberSlab::all_present_in(&ns.present, p.start, span)
                        {
                            *non_empty += span;
                            *num_count += span;
                            let slice = &ns.values[p.start..p.end];
                            match id {
                                FuncId::Sum | FuncId::Average => {
                                    *sum += sum_f64_8way(slice);
                                }
                                FuncId::Max => {
                                    *max_v = (*max_v).max(max_f64_4way(slice));
                                }
                                _ => {
                                    *min_v = (*min_v).min(min_f64_4way(slice));
                                }
                            }
                        } else {
                            for ro in p.start..p.end {
                                if (ns.present[ro / 64] & (1u64 << (ro % 64))) != 0 {
                                    let n = ns.values[ro];
                                    *non_empty += 1;
                                    *num_count += 1;
                                    *sum += n;
                                    *min_v = (*min_v).min(n);
                                    *max_v = (*max_v).max(n);
                                }
                            }
                        }
                    }
                    crate::store::ColSlab::Values(vs) => {
                        for (off, val) in vs[p.start..p.end].iter().enumerate() {
                            match val {
                                crate::Value::Empty => {}
                                crate::Value::Number(n) => {
                                    *non_empty += 1;
                                    *num_count += 1;
                                    *sum += *n;
                                    *min_v = (*min_v).min(*n);
                                    *max_v = (*max_v).max(*n);
                                }
                                crate::Value::Error(e) => {
                                    if id == FuncId::CountA {
                                        *non_empty += 1;
                                    } else if !ignore_err {
                                        let pos = (page_base + (p.start + off) as u32, col);
                                        if earliest_err.is_none_or(|(prev, _)| pos < prev) {
                                            *earliest_err = Some((pos, *e));
                                        }
                                    }
                                }
                                _ => {
                                    *non_empty += 1;
                                }
                            }
                        }
                    }
                    crate::store::ColSlab::Mixed(cs) => {
                        for ro in p.start..p.end {
                            let cell = &cs[ro];
                            if cell.is_blank() {
                                continue;
                            }
                            let abs_row = page_base + ro as u32;
                            if cell.formula.is_none() {
                                match &cell.value {
                                    crate::Value::Empty => {}
                                    crate::Value::Number(n) => {
                                        *non_empty += 1;
                                        *num_count += 1;
                                        *sum += *n;
                                        *min_v = (*min_v).min(*n);
                                        *max_v = (*max_v).max(*n);
                                    }
                                    crate::Value::Error(e) => {
                                        if id == FuncId::CountA {
                                            *non_empty += 1;
                                        } else if !ignore_err {
                                            let pos = (abs_row, col);
                                            if earliest_err.is_none_or(|(prev, _)| pos < prev) {
                                                *earliest_err = Some((pos, *e));
                                            }
                                        }
                                    }
                                    _ => {
                                        *non_empty += 1;
                                    }
                                }
                            } else {
                                let k = (si, abs_row, col);
                                let fv = if let Some(m) = self.get_memo(k) {
                                    m.scalar()
                                } else if let Some(pv) = self.parallel_values
                                    && let Some(val) = pv.get(k)
                                {
                                    val.scalar()
                                } else {
                                    to_fvalue(&cell.value)
                                };
                                match fv {
                                    FValue::Empty => {}
                                    FValue::Number(n) => {
                                        *non_empty += 1;
                                        *num_count += 1;
                                        *sum += n;
                                        *min_v = (*min_v).min(n);
                                        *max_v = (*max_v).max(n);
                                    }
                                    FValue::Error(e) => {
                                        if id == FuncId::CountA {
                                            *non_empty += 1;
                                        } else if !ignore_err {
                                            let pos = (abs_row, col);
                                            if earliest_err.is_none_or(|(prev, _)| pos < prev) {
                                                *earliest_err = Some((pos, e));
                                            }
                                        }
                                    }
                                    _ => {
                                        *non_empty += 1;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            *reads += rows_per_col;
        }
        true
    }

    fn finish_streamed_agg(
        id: FuncId,
        sum: f64,
        num_count: usize,
        non_empty: usize,
        min_v: f64,
        max_v: f64,
        earliest_err: Option<((u32, u32), ErrKind)>,
    ) -> FValue {
        if let Some((_, e)) = earliest_err {
            return FValue::Error(e);
        }
        match id {
            FuncId::Sum => FValue::number(sum),
            FuncId::Count => FValue::number(num_count as f64),
            FuncId::CountA => FValue::number(non_empty as f64),
            FuncId::Average => {
                if num_count == 0 {
                    FValue::Error(ErrKind::Div0)
                } else {
                    FValue::number(sum / num_count as f64)
                }
            }
            FuncId::Max => FValue::number(if num_count > 0 { max_v } else { 0.0 }),
            _ => FValue::number(if num_count > 0 { min_v } else { 0.0 }),
        }
    }

    fn try_stream_single_col_agg(
        &mut self,
        id: FuncId,
        si: usize,
        ar: i64,
        ac: i64,
        br: i64,
        bc: i64,
    ) -> Option<FValue> {
        if self.requested.is_some() {
            return None;
        }
        let gk = self.grid_key_coords(si, ar, ac, br, bc)?;
        let (_, r1_i, c1_i, r2_i, c2_i) = gk;
        if r1_i < 0 || c1_i < 0 || r2_i < r1_i || c2_i < c1_i {
            return None;
        }
        let rows = (r2_i - r1_i + 1) as usize;
        let cols = (c2_i - c1_i + 1) as usize;
        let area = rows.saturating_mul(cols);
        if area < 64
            || (cols == 1
                && (self.range_cache.contains_key(&gk)
                    || self.shared.is_some_and(|s| s.get_range(gk).is_some())))
        {
            return None;
        }
        let mut sum = 0.0f64;
        let mut num_count = 0usize;
        let mut non_empty = 0usize;
        let mut min_v = f64::INFINITY;
        let mut max_v = f64::NEG_INFINITY;
        let mut earliest_err = None;
        let mut reads = 0usize;
        if !self.stream_rect_into_agg(
            id,
            si,
            r1_i as u32,
            c1_i as u32,
            r2_i as u32,
            c2_i as u32,
            &mut sum,
            &mut num_count,
            &mut non_empty,
            &mut min_v,
            &mut max_v,
            &mut earliest_err,
            &mut reads,
        ) {
            return None;
        }
        self.range_reads += reads;
        Some(Self::finish_streamed_agg(
            id,
            sum,
            num_count,
            non_empty,
            min_v,
            max_v,
            earliest_err,
        ))
    }

    fn try_stream_multi_arg_agg(
        &mut self,
        id: FuncId,
        nodes: &[Node],
        si: usize,
    ) -> Option<FValue> {
        if self.requested.is_some() {
            return None;
        }
        let mut sum = 0.0f64;
        let mut num_count = 0usize;
        let mut non_empty = 0usize;
        let mut min_v = f64::INFINITY;
        let mut max_v = f64::NEG_INFINITY;
        let mut reads = 0usize;
        let ignore_err = matches!(id, FuncId::Count | FuncId::CountA);
        let mut saw_large_range = false;

        for node in nodes {
            if let Some(rr) = self.resolve_range_node(node, si) {
                if rr.is_3d() || rr.is_ext() {
                    return None;
                }
                let Ok(si2) = self.sheet_index(rr.sheet.as_deref(), si) else {
                    return Some(FValue::Error(ErrKind::Ref));
                };
                let gk = self.grid_key_coords(si2, rr.r1(), rr.c1(), rr.r2(), rr.c2())?;
                let (_, r1_i, c1_i, r2_i, c2_i) = gk;
                if r1_i < 0 || c1_i < 0 || r2_i < r1_i || c2_i < c1_i {
                    return None;
                }
                let area = ((r2_i - r1_i + 1) as usize).saturating_mul((c2_i - c1_i + 1) as usize);
                saw_large_range |= area >= 64;
                let mut arg_err = None;
                if !self.stream_rect_into_agg(
                    id,
                    si2,
                    r1_i as u32,
                    c1_i as u32,
                    r2_i as u32,
                    c2_i as u32,
                    &mut sum,
                    &mut num_count,
                    &mut non_empty,
                    &mut min_v,
                    &mut max_v,
                    &mut arg_err,
                    &mut reads,
                ) {
                    return None;
                }
                if let Some((_, e)) = arg_err {
                    self.range_reads += reads;
                    return Some(FValue::Error(e));
                }
            } else {
                // Only accept scalar literal/expression arguments when at least one range arg exists;
                // bail to generic on arrays/lambdas so semantics stay 100% identical.
                if matches!(node, Node::Empty) {
                    continue;
                }
                let v = self.eval_node(node, si);
                if self.requested.is_some() {
                    return None;
                }
                if matches!(v, FValue::Array(_) | FValue::Lambda(_)) {
                    return None;
                }
                match v {
                    FValue::Empty => {}
                    FValue::Number(n) => {
                        non_empty += 1;
                        num_count += 1;
                        sum += n;
                        min_v = min_v.min(n);
                        max_v = max_v.max(n);
                    }
                    FValue::Bool(_) => {
                        non_empty += 1;
                        if id == FuncId::Count {
                            num_count += 1;
                        }
                    }
                    FValue::Text(_) => {
                        non_empty += 1;
                    }
                    FValue::Error(e) => {
                        if id == FuncId::CountA {
                            non_empty += 1;
                        } else if !ignore_err {
                            self.range_reads += reads;
                            return Some(FValue::Error(e));
                        }
                    }
                    FValue::Array(_) | FValue::Lambda(_) => unreachable!(),
                }
            }
        }
        if !saw_large_range {
            return None;
        }
        self.range_reads += reads;
        Some(Self::finish_streamed_agg(
            id, sum, num_count, non_empty, min_v, max_v, None,
        ))
    }

    fn try_stream_sumproduct(&mut self, nodes: &[Node], si: usize) -> Option<FValue> {
        if self.requested.is_some() {
            return None;
        }
        let mut cols_spec: Vec<(usize, u32, u32, u32)> = Vec::with_capacity(nodes.len());
        let mut rows_len: Option<usize> = None;
        for node in nodes {
            let rr = self.resolve_range_node(node, si)?;
            if rr.is_3d() || rr.is_ext() {
                return None;
            }
            let si2 = self.sheet_index(rr.sheet.as_deref(), si).ok()?;
            let gk = self.grid_key_coords(si2, rr.r1(), rr.c1(), rr.r2(), rr.c2())?;
            let (_, r1_i, c1_i, r2_i, c2_i) = gk;
            if c1_i != c2_i || r1_i < 0 || c1_i < 0 || r2_i < r1_i {
                return None;
            }
            let len = (r2_i - r1_i + 1) as usize;
            if let Some(prev) = rows_len {
                if prev != len {
                    return None;
                }
            } else {
                if len < 128 {
                    return None;
                }
                rows_len = Some(len);
            }
            cols_spec.push((si2, r1_i as u32, r2_i as u32, c1_i as u32));
        }
        let len = rows_len?;
        let (_, base_r1, base_r2, _) = cols_spec[0];
        if cols_spec.iter().any(|&(_, r1, r2, _)| r1 != base_r1 || r2 != base_r2) {
            return None;
        }
        // Verify no unready formulas in any column
        for &(si2, r1, r2, col) in &cols_spec {
            let sheet = self.wb.sheets.get(si2)?;
            for p in sheet.cf_pages((r1, col, r2, col)) {
                if let crate::store::ColSlab::Mixed(cs) = p.cells.as_ref() {
                    for ro in p.start..p.end {
                        let cell = &cs[ro];
                        if cell.formula.is_some() {
                            let k = (si2, p.page * crate::store::PAGE_ROWS + ro as u32, col);
                            if !self.has_memo(k)
                                && !self.parallel_values.is_some_and(|pv| pv.is_ready(k))
                                && !(self.outside_scope(k) && !callable_cache_candidate(&cell.value))
                            {
                                return None;
                            }
                        }
                    }
                }
            }
        }
        let p_start = base_r1 / crate::store::PAGE_ROWS;
        let p_end = base_r2 / crate::store::PAGE_ROWS;
        let mut total = 0.0f64;
        for pi in p_start..=p_end {
            if ((pi - p_start) & 31) == 31 && self.check_abort() {
                return None;
            }
            let page_base = pi * crate::store::PAGE_ROWS;
            let s_off = base_r1.saturating_sub(page_base) as usize;
            let e_off = ((base_r2 - page_base) as usize + 1).min(crate::store::PAGE_ROWS as usize);
            let span = e_off - s_off;
            if cols_spec.len() == 2 {
                let (s0, _, _, c0) = cols_spec[0];
                let (s1, _, _, c1) = cols_spec[1];
                let sl0 = self.wb.sheets[s0].cf_pages((page_base + s_off as u32, c0, page_base + e_off as u32 - 1, c0));
                let sl1 = self.wb.sheets[s1].cf_pages((page_base + s_off as u32, c1, page_base + e_off as u32 - 1, c1));
                let (Some(p0), Some(p1)) = (sl0.first(), sl1.first()) else {
                    continue;
                };
                if let (
                    crate::store::ColSlab::Numbers(n0),
                    crate::store::ColSlab::Numbers(n1),
                ) = (p0.cells.as_ref(), p1.cells.as_ref())
                    && p0.start == s_off
                    && p0.end == e_off
                    && p1.start == s_off
                    && p1.end == e_off
                    && crate::store::NumberSlab::all_present_in(&n0.present, s_off, span)
                    && crate::store::NumberSlab::all_present_in(&n1.present, s_off, span)
                {
                    total += dot_f64_8way(&n0.values[s_off..e_off], &n1.values[s_off..e_off]);
                    continue;
                }
            }
            for ro in s_off..e_off {
                let abs_r = page_base + ro as u32;
                let mut prod = 1.0f64;
                for &(si2, _, _, col) in &cols_spec {
                    let v_num = match self.wb.sheets[si2].get_view(abs_r, col) {
                        None => 0.0,
                        Some(crate::store::CellView::Number(n)) => n,
                        Some(crate::store::CellView::Value(v)) => {
                            to_fvalue(v).to_number().unwrap_or(0.0)
                        }
                        Some(crate::store::CellView::Cell(cell)) => {
                            if cell.formula.is_some() {
                                let k = (si2, abs_r, col);
                                let fv = if let Some(m) = self.get_memo(k) {
                                    m.scalar()
                                } else if let Some(pv) = self.parallel_values
                                    && let Some(val) = pv.get(k)
                                {
                                    val.scalar()
                                } else {
                                    to_fvalue(&cell.value)
                                };
                                fv.to_number().unwrap_or(0.0)
                            } else {
                                to_fvalue(&cell.value).to_number().unwrap_or(0.0)
                            }
                        }
                    };
                    if v_num == 0.0 {
                        prod = 0.0;
                        break;
                    }
                    prod *= v_num;
                }
                total += prod;
            }
        }
        self.range_reads += len * cols_spec.len();
        Some(FValue::number(total))
    }

    fn try_eval_sumif_cond_col(
        &mut self,
        range_tuple: RangeKey,
        sum_spec: Option<RangeKey>,
        crit_str: &str,
    ) -> Option<FValue> {
        if self.requested.is_some() {
            return None;
        }
        let c = functions::Crit::parse(crit_str);
        let mut cond_gk = self.grid_key_coords(
            range_tuple.0,
            range_tuple.1,
            range_tuple.2,
            range_tuple.3,
            range_tuple.4,
        )?;
        if cond_gk.2 != cond_gk.4 || (cond_gk.3 - cond_gk.1) < 255 {
            return None;
        }
        let mut val_gk = match sum_spec {
            Some(sr) => self.grid_key_coords(sr.0, sr.1, sr.2, sr.3, sr.4)?,
            None => cond_gk,
        };
        if val_gk.2 != val_gk.4 || (val_gk.3 - val_gk.1) != (cond_gk.3 - cond_gk.1) {
            return None;
        }
        if !c.matches(&FValue::Empty) {
            let cm = self.col_max_row(cond_gk.0, cond_gk.2 as u32);
            let new_cond_r2 = aligned_col_end_row(cond_gk.1, cm, cond_gk.3);
            if new_cond_r2 < cond_gk.3 && (new_cond_r2 - cond_gk.1) >= 255 {
                let shrunk = cond_gk.3 - new_cond_r2;
                cond_gk.3 = new_cond_r2;
                val_gk.3 -= shrunk;
            }
        }
        let cond_sc = self.shared_cond_col_internal(cond_gk)?;
        let val_sc = if val_gk == cond_gk {
            cond_sc.clone()
        } else {
            self.shared_cond_col_internal(val_gk)?
        };
        if val_sc.has_error || val_sc.has_non_finite {
            return None;
        }

        // 1. Numeric inequality (<, <=, >, >=, <>) via O(log N) OrderedCriteria prefix sum!
        if c.numeric_bounds().is_some()
            && !cond_sc.has_coerced_non_strict
            && !val_sc.has_coerced_non_strict
            && (!cond_sc.has_empty
                || c.matches(&FValue::Empty) == c.matches_coerced(&FValue::Empty))
        {
            let ckey = CriteriaKey {
                function: 0,
                ranges: vec![val_gk, cond_gk],
                criteria: vec![crit_str.to_string()],
            };
            let mut ordered = std::mem::take(&mut self.ordered_criteria);
            let shared_ref = self.shared;
            let cond_clone = cond_sc.clone();
            let val_clone = val_sc.clone();
            let res = ordered.lookup_lazy(
                &ckey,
                &mut |vr, cr| shared_ref.and_then(|s| s.get_ordered(vr, cr, false)),
                &mut |vr, cr, built| {
                    if let Some(s) = shared_ref {
                        s.put_ordered(vr, cr, false, built);
                    }
                },
                &mut |rk| rk == cond_gk || rk == val_gk,
                &mut |vr, cr, mm| {
                    if cr != cond_gk || vr != Some(val_gk) {
                        return None;
                    }
                    self.wb
                        .sheets
                        .get(cond_gk.0)
                        .and_then(|sh| {
                            sh.shared_lookup.get_or_build_ordered_persistent(
                                Some(val_gk),
                                cond_gk,
                                mm,
                                &cond_clone,
                                Some(&val_clone),
                            )
                        })
                        .or_else(|| {
                            conditional_index::BuiltOrdered::build_from_cond(
                                &cond_clone,
                                Some(&val_clone),
                                mm,
                            )
                            .map(std::sync::Arc::new)
                        })
                },
                &mut |_| None,
            );
            self.ordered_criteria = ordered;
            if let Some(ans) = res {
                return Some(ans);
            }
        }

        // 2. Exact numeric equality (`c.op == "="`, `c.num == Some(target)`) via O(log N + K) sorted_nums!
        if c.indexable()
            && let Some(target) = c.num
            && target.is_finite()
            && (!cond_sc.has_coerced_non_strict
                || target.fract() != 0.0
                || (target != 0.0 && target != 1.0))
        {
            // Note: if cond_sc has coerced booleans TRUE(1)/FALSE(0), SUMIF does NOT match Bool as 1/0,
            // so when `cond_sc.has_coerced_non_strict` is true we only take sorted_nums if we verify non-bool,
            // or simply require `!cond_sc.has_coerced_non_strict` for 100% strict safety!
            if !cond_sc.has_coerced_non_strict
                && let Some(sorted) = cond_sc.sorted_nums()
            {
                let lo = sorted.partition_point(|&(v, _)| v - target < -f64::EPSILON);
                let hi = sorted.partition_point(|&(v, _)| v - target <= f64::EPSILON);
                let check_non_empty = target.abs() <= f64::EPSILON && cond_sc.has_empty;
                let mut sum = 0.0f64;
                for &(_, row_off) in &sorted[lo..hi] {
                    let ro = row_off as usize;
                    if check_non_empty && !cond_sc.is_non_empty_at(ro) {
                        continue;
                    }
                    sum += val_sc.strict_number_at(ro);
                }
                return Some(FValue::number(sum));
            }
        }

        // 3. Exact non-empty text equality via O(1 + K) persistent text_dict!
        if c.indexable() && c.num.is_none() && !c.text.is_empty() {
            if !cond_sc.has_non_num && !cond_sc.has_coerced_non_strict {
                return Some(FValue::number(0.0));
            }
            if let Some(dict) = cond_sc.text_dict() {
                let Some(rows) = dict.get(c.text.as_str()) else {
                    return Some(FValue::number(0.0));
                };
                let mut sum = 0.0f64;
                for &row_off in rows.iter() {
                    sum += val_sc.strict_number_at(row_off as usize);
                }
                return Some(FValue::number(sum));
            }
        }

        None
    }

    /// Zero-materialization single-column page streaming for `COUNTIF` (and `SUMIF` text pre-index).
    /// Avoids allocating `Vec<FValue>` for 200k-row numeric columns when evaluating text criteria,
    /// while populating `count_indexes` so subsequent queries finish in O(1).
    fn try_stream_single_col_countif(
        &mut self,
        si: usize,
        gk: RangeKey,
        crit: &functions::Crit,
    ) -> Option<FValue> {
        if self.requested.is_some() {
            return None;
        }
        let (_, r1_i, c1_i, r2_i, c2_i) = gk;
        if c1_i != c2_i || r1_i < 0 || c1_i < 0 || r2_i < r1_i {
            return None;
        }
        let len = (r2_i - r1_i + 1) as usize;
        if len < 256
            || self.range_cache.contains_key(&gk)
            || self.shared.is_some_and(|s| s.get_range(gk).is_some())
        {
            return None;
        }
        let sheet = self.wb.sheets.get(si)?;
        let (r1, r2, col) = (r1_i as u32, r2_i as u32, c1_i as u32);
        let pages = sheet.cf_pages((r1, col, r2, col));
        let build_text_index = crit.indexable() && crit.num.is_none() && len <= 2_097_152;
        let skip_numbers_fast = crit.num.is_none() && crit.op == "=";
        let numbers_const_match = crit.num.is_none().then(|| crit.matches_number(0.0));

        // Ultra-fast single-thread O(pages) POPCNT path when ALL slabs are `ColSlab::Numbers`
        // and the criterion is a text/non-numeric criterion (`numbers_const_match.is_some()`):
        // counts 200k numeric rows via hardware POPCNT without loading a single f64 or spawning threads!
        if let Some(match_all_nums) = numbers_const_match
            && pages.iter().all(|p| p.cells.is_numbers())
        {
            let mut total_non_blank = 0usize;
            for p in &pages {
                if let crate::store::ColSlab::Numbers(ns) = p.cells.as_ref() {
                    total_non_blank += crate::store::NumberSlab::present_count_in(
                        &ns.present,
                        p.start,
                        p.end - p.start,
                    );
                }
            }
            let mut total_count = if match_all_nums { total_non_blank } else { 0 };
            let blank_rows = len - total_non_blank;
            if crit.matches(&FValue::Empty) {
                total_count += blank_rows;
            }
            if build_text_index {
                self.count_indexes
                    .insert_streamed_text(gk, 0, FxHashMap::default(), blank_rows);
            }
            self.range_reads += len;
            return Some(FValue::number(total_count as f64));
        }

        // Multi-core parallel page scan for large single-column scans (>= 16k rows) when no
        // per-Calc memo/parallel_values/scope lookups are needed (e.g. 200k numeric column scan).
        if pages.len() >= 64
            && self.memo.is_empty()
            && self.memo_overflow.is_empty()
            && self.batch_results.is_none()
            && self.parallel_values.is_none()
            && self.scope.is_none()
        {
            use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
            let workers = pool::max_workers().min(pages.len() / 32).clamp(1, 8);
            if workers >= 2 {
                struct WorkerOut {
                    count: usize,
                    non_blank_seen: usize,
                    empty_value_seen: usize,
                    text_entries: usize,
                    text_overflow: bool,
                    text_map: FxHashMap<String, Vec<usize>>,
                }
                let aborted = AtomicBool::new(false);
                let next_chunk = AtomicUsize::new(0);
                const PAGE_CHUNK: usize = 32;
                let num_chunks = pages.len().div_ceil(PAGE_CHUNK);
                let outputs: Vec<std::sync::Mutex<Option<WorkerOut>>> = (0..num_chunks)
                    .map(|_| std::sync::Mutex::new(None))
                    .collect();
                let pages_slice = pages.as_slice();

                let cancel_flag = self.cancel;
                let deadline_opt = self.deadline;
                pool::run_parallel(workers, |_| {
                    loop {
                        if aborted.load(Ordering::Relaxed) {
                            return;
                        }
                        if cancel_flag.is_some_and(|c| c.load(Ordering::Relaxed))
                            || deadline_opt.is_some_and(|d| std::time::Instant::now() >= d)
                        {
                            aborted.store(true, Ordering::Relaxed);
                            return;
                        }
                        let ci = next_chunk.fetch_add(1, Ordering::Relaxed);
                        if ci >= num_chunks {
                            break;
                        }
                        let ps = ci * PAGE_CHUNK;
                        let pe = (ps + PAGE_CHUNK).min(pages_slice.len());
                        let mut count = 0usize;
                        let mut non_blank_seen = 0usize;
                        let mut empty_value_seen = 0usize;
                        let mut text_map: FxHashMap<String, Vec<usize>> = FxHashMap::default();
                        let mut text_entries = 0usize;
                        let mut text_overflow = false;

                        for p in &pages_slice[ps..pe] {
                            let page_base = p.page * crate::store::PAGE_ROWS;
                            match p.cells.as_ref() {
                                crate::store::ColSlab::Numbers(ns) => {
                                    let span = p.end - p.start;
                                    if let Some(m) = numbers_const_match {
                                        let cnt = crate::store::NumberSlab::present_count_in(
                                            &ns.present,
                                            p.start,
                                            span,
                                        );
                                        non_blank_seen += cnt;
                                        if m {
                                            count += cnt;
                                        }
                                    } else if crate::store::NumberSlab::all_present_in(
                                        &ns.present,
                                        p.start,
                                        span,
                                    ) {
                                        non_blank_seen += span;
                                        for &n in &ns.values[p.start..p.end] {
                                            if crit.matches_number(n) {
                                                count += 1;
                                            }
                                        }
                                    } else {
                                        for ro in p.start..p.end {
                                            if (ns.present[ro / 64] & (1u64 << (ro % 64))) != 0 {
                                                non_blank_seen += 1;
                                                if crit.matches_number(ns.values[ro]) {
                                                    count += 1;
                                                }
                                            }
                                        }
                                    }
                                }
                                crate::store::ColSlab::Values(vs) => {
                                    for ro in p.start..p.end {
                                        let val = &vs[ro];
                                        match val {
                                            crate::Value::Empty => continue,
                                            crate::Value::Number(_) if skip_numbers_fast => {
                                                non_blank_seen += 1;
                                                continue;
                                            }
                                            _ => {}
                                        }
                                        non_blank_seen += 1;
                                        if crit.matches_value(val) {
                                            count += 1;
                                        }
                                        if build_text_index && !text_overflow {
                                            match val {
                                                crate::Value::Number(_) | crate::Value::Error(_) => {}
                                                crate::Value::Empty => empty_value_seen += 1,
                                                crate::Value::Text(s) => {
                                                    let row_off = (page_base + ro as u32 - r1) as usize;
                                                    let key_s = if s.is_ascii() {
                                                        s.to_ascii_lowercase()
                                                    } else {
                                                        s.to_string()
                                                    };
                                                    text_map.entry(key_s).or_default().push(row_off);
                                                    text_entries += 1;
                                                    if text_entries > 2_097_152 {
                                                        text_overflow = true;
                                                    }
                                                }
                                                crate::Value::Bool(b) => {
                                                    let row_off = (page_base + ro as u32 - r1) as usize;
                                                    text_map
                                                        .entry(if *b { "true".to_string() } else { "false".to_string() })
                                                        .or_default()
                                                        .push(row_off);
                                                    text_entries += 1;
                                                }
                                            }
                                        }
                                    }
                                }
                                crate::store::ColSlab::Mixed(cs) => {
                                    for ro in p.start..p.end {
                                        let cell = &cs[ro];
                                        if cell.formula.is_none() {
                                            match &cell.value {
                                                crate::Value::Empty => continue,
                                                crate::Value::Number(_) if skip_numbers_fast => {
                                                    non_blank_seen += 1;
                                                    continue;
                                                }
                                                _ => {}
                                            }
                                            non_blank_seen += 1;
                                            if crit.matches_value(&cell.value) {
                                                count += 1;
                                            }
                                            if build_text_index && !text_overflow {
                                                match &cell.value {
                                                    crate::Value::Number(_) | crate::Value::Error(_) => {}
                                                    crate::Value::Empty => empty_value_seen += 1,
                                                    crate::Value::Text(s) => {
                                                        let row_off = (page_base + ro as u32 - r1) as usize;
                                                        let key_s = if s.is_ascii() {
                                                            s.to_ascii_lowercase()
                                                        } else {
                                                            s.to_string()
                                                        };
                                                        text_map.entry(key_s).or_default().push(row_off);
                                                        text_entries += 1;
                                                        if text_entries > 2_097_152 {
                                                            text_overflow = true;
                                                        }
                                                    }
                                                    crate::Value::Bool(b) => {
                                                        let row_off = (page_base + ro as u32 - r1) as usize;
                                                        text_map
                                                            .entry(if *b { "true".to_string() } else { "false".to_string() })
                                                            .or_default()
                                                            .push(row_off);
                                                        text_entries += 1;
                                                    }
                                                }
                                            }
                                        } else {
                                            // Formula cell with no memo/scope -> abort parallel stream and fall back
                                            aborted.store(true, Ordering::Relaxed);
                                            return;
                                        }
                                    }
                                }
                            }
                        }

                        *outputs[ci].lock().unwrap() = Some(WorkerOut {
                            count,
                            non_blank_seen,
                            empty_value_seen,
                            text_entries,
                            text_overflow,
                            text_map,
                        });
                    }
                });

                if aborted.load(Ordering::Relaxed) {
                    let _ = self.check_abort();
                    return None;
                }
                let mut total_count = 0usize;
                let mut total_non_blank = 0usize;
                let mut total_empty_val = 0usize;
                let mut total_text_entries = 0usize;
                let mut any_overflow = false;
                let mut merged_text: FxHashMap<String, Vec<usize>> = FxHashMap::default();
                for slot in outputs {
                    let Some(w) = slot.into_inner().unwrap() else {
                        return None;
                    };
                    total_count += w.count;
                    total_non_blank += w.non_blank_seen;
                    total_empty_val += w.empty_value_seen;
                    total_text_entries += w.text_entries;
                    any_overflow |= w.text_overflow || total_text_entries > 2_097_152;
                    if build_text_index && !any_overflow && !w.text_map.is_empty() {
                        for (k, mut rows_v) in w.text_map {
                            match merged_text.entry(k) {
                                std::collections::hash_map::Entry::Vacant(v) => {
                                    v.insert(rows_v);
                                }
                                std::collections::hash_map::Entry::Occupied(mut o) => {
                                    o.get_mut().append(&mut rows_v);
                                }
                            }
                        }
                    }
                }
                let blank_rows = len - total_non_blank;
                if crit.matches(&FValue::Empty) {
                    total_count += blank_rows;
                }
                if build_text_index && !any_overflow {
                    self.count_indexes.insert_streamed_text(
                        gk,
                        total_text_entries,
                        merged_text,
                        blank_rows + total_empty_val,
                    );
                }
                self.range_reads += len;
                return Some(FValue::number(total_count as f64));
            }
        }

        let mut count = 0usize;
        let mut non_blank_seen = 0usize;
        let mut empty_value_seen = 0usize;
        let mut text_map: FxHashMap<String, Vec<usize>> = FxHashMap::default();
        let mut text_entries = 0usize;
        let mut text_overflow = false;

        for (pi, p) in pages.iter().enumerate() {
            if (pi & 31) == 31 && self.check_abort() {
                return None;
            }
            let page_base = p.page * crate::store::PAGE_ROWS;
            match p.cells.as_ref() {
                crate::store::ColSlab::Numbers(ns) => {
                    let span = p.end - p.start;
                    if let Some(m) = numbers_const_match {
                        let cnt = crate::store::NumberSlab::present_count_in(
                            &ns.present,
                            p.start,
                            span,
                        );
                        non_blank_seen += cnt;
                        if m {
                            count += cnt;
                        }
                    } else if crate::store::NumberSlab::all_present_in(&ns.present, p.start, span) {
                        non_blank_seen += span;
                        for &n in &ns.values[p.start..p.end] {
                            if crit.matches_number(n) {
                                count += 1;
                            }
                        }
                    } else {
                        for ro in p.start..p.end {
                            if (ns.present[ro / 64] & (1u64 << (ro % 64))) != 0 {
                                non_blank_seen += 1;
                                if crit.matches_number(ns.values[ro]) {
                                    count += 1;
                                }
                            }
                        }
                    }
                }
                crate::store::ColSlab::Values(vs) => {
                    for ro in p.start..p.end {
                        let val = &vs[ro];
                        if val.is_empty() {
                            continue;
                        }
                        non_blank_seen += 1;
                        if skip_numbers_fast && matches!(val, crate::Value::Number(_)) {
                            continue;
                        }
                        if crit.matches_value(val) {
                            count += 1;
                        }
                        if build_text_index && !text_overflow {
                            match val {
                                crate::Value::Number(_) | crate::Value::Error(_) => {}
                                crate::Value::Empty => empty_value_seen += 1,
                                crate::Value::Text(s) => {
                                    let row_off = (page_base + ro as u32 - r1) as usize;
                                    let key_s = if s.is_ascii() {
                                        s.to_ascii_lowercase()
                                    } else {
                                        s.to_string()
                                    };
                                    text_map.entry(key_s).or_default().push(row_off);
                                    text_entries += 1;
                                    if text_entries > 2_097_152 {
                                        text_overflow = true;
                                    }
                                }
                                crate::Value::Bool(b) => {
                                    let row_off = (page_base + ro as u32 - r1) as usize;
                                    text_map
                                        .entry(if *b { "true".to_string() } else { "false".to_string() })
                                        .or_default()
                                        .push(row_off);
                                    text_entries += 1;
                                }
                            }
                        }
                    }
                }
                crate::store::ColSlab::Mixed(cs) => {
                    for ro in p.start..p.end {
                        let cell = &cs[ro];
                        if cell.is_blank() {
                            continue;
                        }
                        non_blank_seen += 1;
                        if cell.formula.is_none() {
                            if skip_numbers_fast && matches!(cell.value, crate::Value::Number(_)) {
                                continue;
                            }
                            if crit.matches_value(&cell.value) {
                                count += 1;
                            }
                            if build_text_index && !text_overflow {
                                match &cell.value {
                                    crate::Value::Number(_) | crate::Value::Error(_) => {}
                                    crate::Value::Empty => empty_value_seen += 1,
                                    crate::Value::Text(s) => {
                                        let row_off = (page_base + ro as u32 - r1) as usize;
                                        let key_s = if s.is_ascii() {
                                            s.to_ascii_lowercase()
                                        } else {
                                            s.to_string()
                                        };
                                        text_map.entry(key_s).or_default().push(row_off);
                                        text_entries += 1;
                                        if text_entries > 2_097_152 {
                                            text_overflow = true;
                                        }
                                    }
                                    crate::Value::Bool(b) => {
                                        let row_off = (page_base + ro as u32 - r1) as usize;
                                        text_map
                                            .entry(if *b { "true".to_string() } else { "false".to_string() })
                                            .or_default()
                                            .push(row_off);
                                        text_entries += 1;
                                    }
                                }
                            }
                        } else {
                            let abs_row = page_base + ro as u32;
                            let k = (si, abs_row, col);
                            let fv = if let Some(m) = self.get_memo(k) {
                                m.scalar()
                            } else if let Some(pv) = self.parallel_values
                                && let Some(val) = pv.get(k)
                            {
                                val.scalar()
                            } else if self.outside_scope(k) && !callable_cache_candidate(&cell.value) {
                                to_fvalue(&cell.value)
                            } else {
                                return None;
                            };
                            if crit.matches(&fv) {
                                count += 1;
                            }
                            if build_text_index && !text_overflow {
                                match fv {
                                    FValue::Number(_) | FValue::Error(_) | FValue::Array(_) | FValue::Lambda(_) => {}
                                    FValue::Empty => empty_value_seen += 1,
                                    FValue::Text(s) => {
                                        let row_off = (abs_row - r1) as usize;
                                        let key_s = if s.is_ascii() {
                                            s.to_ascii_lowercase()
                                        } else {
                                            s.to_string()
                                        };
                                        text_map.entry(key_s).or_default().push(row_off);
                                        text_entries += 1;
                                        if text_entries > 2_097_152 {
                                            text_overflow = true;
                                        }
                                    }
                                    FValue::Bool(b) => {
                                        let row_off = (abs_row - r1) as usize;
                                        text_map
                                            .entry(if b { "true".to_string() } else { "false".to_string() })
                                            .or_default()
                                            .push(row_off);
                                        text_entries += 1;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        let blank_rows = len - non_blank_seen;
        if crit.matches(&FValue::Empty) {
            count += blank_rows;
        }
        if build_text_index && !text_overflow {
            self.count_indexes.insert_streamed_text(
                gk,
                text_entries,
                text_map,
                blank_rows + empty_value_seen,
            );
        }
        self.range_reads += len;
        Some(FValue::number(count as f64))
    }

    fn shared_cond_col_internal(&mut self, rk: RangeKey) -> Option<std::sync::Arc<lookup_cache::SharedCondColumn>> {
        let (si_c, r1_c, c1_c, r2_c, c2_c) = rk;
        if c1_c != c2_c || (r2_c - r1_c + 1) < 256 || self.check_abort() {
            return None;
        }
        let sheet = self.wb.sheets.get(si_c)?;
        let col_u32 = c1_c as u32;
        let has_scope = self.scope.is_some() || self.fast_scope.is_some();
        let overlay_cb = |abs_row: u32| self.ready_dirty_value_at(si_c, abs_row, col_u32);
        let eval_scope = lookup_cache::LookupEvalScope {
            scope: self.scope,
            fast_scope: self.fast_scope,
            overlay: has_scope.then_some(&overlay_cb as &dyn Fn(u32) -> Option<crate::Value>),
            cancel: self.cancel,
            deadline: self.deadline,
        };
        let (sc, reads) = sheet
            .shared_lookup
            .get_or_build_cond_column(sheet, rk, &eval_scope)?;
        self.range_reads += reads;
        if self.check_abort() {
            return None;
        }
        Some(sc)
    }

    fn try_eval_criteria_lazy(&mut self, id: FuncId, name: &str, nodes: &[Node], si: usize) -> Option<FValue> {
        if self.check_abort() {
            return Some(FValue::Error(ErrKind::Value));
        }
        let function = match id {
            FuncId::SumIfs => 0u8,
            FuncId::CountIfs => 1,
            FuncId::AverageIf => 2,
            FuncId::AverageIfs => 3,
            FuncId::MinIfs => 4,
            FuncId::MaxIfs => 5,
            _ => return None,
        };
        let valid_arity = match function {
            1 => nodes.len() >= 2 && nodes.len().is_multiple_of(2),
            2 => (2..=3).contains(&nodes.len()),
            _ => nodes.len() >= 3 && nodes.len() % 2 == 1,
        };
        if !valid_arity {
            return None;
        }
        let mut key = CriteriaKey {
            function,
            ranges: Vec::with_capacity(nodes.len() / 2 + 1),
            criteria: Vec::with_capacity(nodes.len() / 2),
        };
        for (i, node) in nodes.iter().enumerate() {
            let is_crit = match function {
                1 => i % 2 == 1,
                2 => i == 1,
                _ => i > 0 && i.is_multiple_of(2),
            };
            if is_crit {
                let cv = self.eval_node(node, si).scalar();
                if self.requested.is_some() {
                    return Some(FValue::Error(ErrKind::Ref));
                }
                if self.aborted {
                    return Some(FValue::Error(ErrKind::Value));
                }
                key.criteria.push(cv.to_text().ok()?);
            } else {
                let rr = self.resolve_range_node(node, si)?;
                if rr.is_ext() || rr.is_3d() {
                    return None;
                }
                let sheet = self.sheet_index(rr.sheet.as_deref(), si).ok()?;
                let gk = self.grid_key_coords(sheet, rr.r1(), rr.c1(), rr.r2(), rr.c2())?;
                key.ranges.push(gk);
            }
        }
        // Per-column used-row trimming: if at least one condition column cannot match Empty,
        // no row beyond that column's last occupied row can ever satisfy all conditions!
        if key.ranges.iter().all(|r| r.2 == r.4 && r.2 >= 0) {
            let first_len = key.ranges.first().map(|r| r.3 - r.1).unwrap_or(0);
            if first_len >= 511 && key.ranges.iter().all(|r| r.3 - r.1 == first_len) {
                let cond_slice: &[RangeKey] = match function {
                    1 => &key.ranges,
                    2 => key.ranges.get(..1).unwrap_or(&[]),
                    _ => key.ranges.get(1..).unwrap_or(&[]),
                };
                if cond_slice.len() == key.criteria.len() {
                    let mut min_cap_off: Option<i64> = None;
                    for (crk, ctext) in cond_slice.iter().zip(key.criteria.iter()) {
                        let parsed_c = functions::Crit::parse(ctext);
                        if !parsed_c.matches_coerced(&FValue::Empty) {
                            let cm = self.col_max_row(crk.0, crk.2 as u32);
                            let cap_off = aligned_col_end_row(crk.1, cm, crk.3) - crk.1;
                            min_cap_off = Some(match min_cap_off {
                                Some(prev) => prev.min(cap_off),
                                None => cap_off,
                            });
                        }
                    }
                    if let Some(cap_off) = min_cap_off
                        && cap_off >= 255
                        && cap_off < first_len
                    {
                        for rk in &mut key.ranges {
                            rk.3 = rk.1 + cap_off;
                        }
                    }
                }
            }
        }
        if let Some(v) = self.criteria_cache.get(&key) {
            self.agg_hits += 1;
            return Some(v.clone());
        }
        if let Some(shared) = self.shared
            && let Some(v) = shared.get_criteria(&key)
        {
            if self.criteria_cache.len() >= 16_384 {
                self.criteria_cache.clear();
            }
            self.criteria_cache.insert(key, v.clone());
            self.agg_hits += 1;
            return Some(v);
        }

        // Pre-resolve single-column `SharedCondColumn`s once (supports O(1) FastScope + dirty overlay!)
        let all_single_cols_256 = key
            .ranges
            .iter()
            .all(|r| r.2 == r.4 && (r.3 - r.1 + 1) >= 256);
        let mut resolved_cond_cols: Vec<(RangeKey, std::sync::Arc<lookup_cache::SharedCondColumn>)> =
            Vec::new();
        if all_single_cols_256 {
            let mut ok = true;
            for &rk in &key.ranges {
                if let Some((_, sc)) = resolved_cond_cols.iter().find(|(k, _)| *k == rk) {
                    let sc_clone = sc.clone();
                    resolved_cond_cols.push((rk, sc_clone));
                } else if let Some(sc) = self.shared_cond_col_internal(rk) {
                    if let Some(s) = self.shared
                        && s.get_cond_col(rk).is_none()
                    {
                        s.put_cond_col(&conditional_index::ColumnIndex::from_shared(rk, sc.clone()));
                    }
                    resolved_cond_cols.push((rk, sc));
                } else {
                    ok = false;
                    break;
                }
            }
            if !ok {
                resolved_cond_cols.clear();
            }
        }
        if self.aborted {
            return Some(FValue::Error(ErrKind::Value));
        }

        // 1. OrderedCriteria (<, <=, >, >=, <>)
        let mut ans = {
            let mut ordered = std::mem::take(&mut self.ordered_criteria);
            let shared_ref = self.shared;
            let wb_ref = self.wb;
            let need_mm = matches!(key.function, 4 | 5);
            let pre_cols = &resolved_cond_cols;
            let res = ordered.lookup_lazy(
                &key,
                &mut |vr, cr| shared_ref.and_then(|s| s.get_ordered(vr, cr, need_mm)),
                &mut |vr, cr, built| {
                    if let Some(s) = shared_ref {
                        s.put_ordered(vr, cr, need_mm, built);
                    }
                },
                &mut |rk| {
                    rk.2 == rk.4
                        && wb_ref
                            .sheets
                            .get(rk.0)
                            .is_some_and(|sh| sh.shared_lookup.has_cond_column(rk))
                },
                &mut |vr, cr, mm| {
                    let _sf = shared_ref.map(|s| {
                        s.ordered_cond_mu.lock().unwrap_or_else(|e| e.into_inner())
                    });
                    if let Some(s) = shared_ref
                        && let Some(hit) = s.get_ordered(vr, cr, mm)
                    {
                        return Some(hit);
                    }
                    let cond_sc = pre_cols.iter().find(|(k, _)| *k == cr).map(|(_, c)| c)?;
                    let val_sc = match vr {
                        Some(v) => Some(pre_cols.iter().find(|(k, _)| *k == v).map(|(_, c)| c)?),
                        None => None,
                    };
                    wb_ref
                        .sheets
                        .get(cr.0)
                        .and_then(|sh| {
                            sh.shared_lookup.get_or_build_ordered_persistent(
                                vr,
                                cr,
                                mm,
                                cond_sc,
                                val_sc,
                            )
                        })
                        .or_else(|| {
                            conditional_index::BuiltOrdered::build_from_cond(
                                cond_sc,
                                val_sc.map(std::sync::Arc::as_ref),
                                mm,
                            )
                            .map(std::sync::Arc::new)
                        })
                },
                &mut |rk| {
                    let len = ((rk.3 - rk.1 + 1) as usize).saturating_mul((rk.4 - rk.2 + 1) as usize);
                    self.materialize_range(rk, len).ok()
                },
            );
            self.ordered_criteria = ordered;
            res
        };
        if self.requested.is_some() {
            return Some(FValue::Error(ErrKind::Ref));
        }
        if self.aborted {
            return Some(FValue::Error(ErrKind::Value));
        }

        // 2. GroupedCriteria (multi-column exact equality group-by hash table, persisted across recalcs!)
        if ans.is_none() {
            let group_slot = usize::from(key.function == 1);
            let mut grouped = std::mem::take(&mut self.grouped_criteria[group_slot]);
            let shared_ref = self.shared;
            let wb_ref = self.wb;
            let cancel_ref = self.cancel;
            let deadline_ref = self.deadline;
            let pre_cols = &resolved_cond_cols;
            let res = grouped.lookup_lazy(
                &key,
                &mut |vr, cr, nm| {
                    if let Some(hit) = shared_ref.and_then(|s| s.get_grouped(vr, cr, nm)) {
                        return Some(hit);
                    }
                    if !pre_cols.is_empty() {
                        let mut c_scs = Vec::with_capacity(cr.len());
                        for &rk in cr {
                            c_scs.push(pre_cols.iter().find(|(k, _)| *k == rk)?.1.clone());
                        }
                        let v_sc = match vr {
                            Some(v) => Some(pre_cols.iter().find(|(k, _)| *k == v)?.1.clone()),
                            None => None,
                        };
                        let sh_idx = cr.first()?.0;
                        if let Some(hit) = wb_ref.sheets.get(sh_idx).and_then(|sh| {
                            sh.shared_lookup
                                .get_grouped_persistent(vr, cr, nm, &c_scs, v_sc.as_ref())
                        }) {
                            if let Some(s) = shared_ref {
                                s.put_grouped(vr, cr.to_vec(), nm.to_vec(), hit.clone());
                            }
                            return Some(hit);
                        }
                    }
                    None
                },
                &mut |vr, cr, nm, built| {
                    if let Some(s) = shared_ref {
                        s.put_grouped(vr, cr, nm, built);
                    }
                },
                &mut |vr, cr, nm, limit| {
                    if pre_cols.is_empty() {
                        return None;
                    }
                    let mut c_scs = Vec::with_capacity(cr.len());
                    for &rk in cr {
                        c_scs.push(pre_cols.iter().find(|(k, _)| *k == rk)?.1.clone());
                    }
                    let v_sc = match vr {
                        Some(v) => Some(pre_cols.iter().find(|(k, _)| *k == v)?.1.clone()),
                        None => None,
                    };
                    let built = std::sync::Arc::new(functions::BuiltGroups::build_from_cond_cols(
                        &c_scs,
                        v_sc.as_deref(),
                        nm,
                        limit,
                        cancel_ref,
                        deadline_ref,
                    )?);
                    if let Some(&first_cr) = cr.first()
                        && let Some(sh) = wb_ref.sheets.get(first_cr.0)
                    {
                        sh.shared_lookup.put_grouped_persistent(
                            vr,
                            cr.to_vec(),
                            nm.to_vec(),
                            c_scs,
                            v_sc,
                            built.clone(),
                        );
                    }
                    Some(built)
                },
                &mut |rk| {
                    let len = ((rk.3 - rk.1 + 1) as usize).saturating_mul((rk.4 - rk.2 + 1) as usize);
                    self.materialize_range(rk, len).ok()
                },
            );
            self.grouped_criteria[group_slot] = grouped;
            ans = res;
        }
        if self.requested.is_some() {
            return Some(FValue::Error(ErrKind::Ref));
        }
        if self.aborted {
            return Some(FValue::Error(ErrKind::Value));
        }

        // 3. MatchIndexes (column index + persistent 256-row page cache + high-selectivity sparse row probe)
        if ans.is_none() {
            let mut match_ix = std::mem::take(&mut self.match_indexes);
            let shared_ref = self.shared;
            let pre_cols = &resolved_cond_cols;
            let res = match_ix.lookup_lazy(
                &key,
                &mut |rk| {
                    if let Some(s) = shared_ref
                        && let Some(col) = s.get_cond_col(rk)
                    {
                        return Some(col);
                    }
                    if let Some((_, sc)) = pre_cols.iter().find(|(k, _)| *k == rk) {
                        let ci = conditional_index::ColumnIndex::from_shared(rk, sc.clone());
                        if let Some(s) = shared_ref {
                            s.put_cond_col(&ci);
                        }
                        return Some(ci);
                    }
                    None
                },
                &mut |col| {
                    if let Some(s) = shared_ref {
                        s.put_cond_col(col);
                    }
                },
                self,
            );
            self.match_indexes = match_ix;
            ans = res;
        }
        if self.requested.is_some() {
            return Some(FValue::Error(ErrKind::Ref));
        }
        if self.aborted {
            return Some(FValue::Error(ErrKind::Value));
        }

        // 4. Fallback for < 256-row ranges: materialize only this query's columns and cache in criteria_cache
        let out = match ans {
            Some(v) => v,
            None => {
                let args = self.function_args(id, nodes, si);
                if self.requested.is_some() {
                    return Some(FValue::Error(ErrKind::Ref));
                }
                if self.aborted {
                    return Some(FValue::Error(ErrKind::Value));
                }
                functions::call_id_in(self.ctx, id, name, &args)
            }
        };
        if self.requested.is_none() && !self.aborted {
            if let Some(s) = self.shared {
                s.put_criteria(key.clone(), out.clone());
            }
            if self.criteria_cache.len() >= 16_384 {
                self.criteria_cache.clear();
            }
            self.criteria_cache.insert(key, out.clone());
            self.agg_miss += 1;
        }
        Some(out)
    }

    fn cache_get(&mut self, key: RangeKey) -> Option<std::sync::Arc<Vec<FValue>>> {
        let (values, last) = self.range_cache.get_mut(&key)?;
        self.cache_order.remove(&(*last, key));
        self.cache_clock += 1;
        *last = self.cache_clock;
        self.cache_order.insert((*last, key));
        Some(values.clone())
    }

    fn cache_put(&mut self, key: RangeKey, vals: std::sync::Arc<Vec<FValue>>) -> std::sync::Arc<Vec<FValue>> {
        const LIMIT: usize = 4_000_000;
        if self.requested.is_some() || vals.len() > LIMIT { return vals; }
        if let Some(existing) = self.cache_get(key) { return existing; }
        while self.cache_vals + vals.len() > LIMIT {
            let Some((_, oldest)) = self.cache_order.pop_first() else { break };
            self.cache_vals -= self.range_cache.remove(&oldest).unwrap().0.len();
        }
        self.cache_clock += 1;
        self.cache_vals += vals.len();
        self.cache_order.insert((self.cache_clock, key));
        self.range_cache.insert(key, (vals.clone(), self.cache_clock));
        vals
    }

    fn prefix_range_resolved(&mut self, si: usize, rr: &ResolvedRange<'_>) -> Option<(PrefixKey, i64)> {
        if rr.a_row < 0 || rr.a_col < 0 { return None; }
        let (vertical, start, end, anchored) = if rr.a_col == rr.b_col && rr.a_row <= rr.b_row {
            (true, rr.a_row, rr.b_row, rr.a_row_abs && !rr.b_row_abs)
        } else if rr.a_row == rr.b_row && rr.a_col <= rr.b_col {
            (false, rr.a_col, rr.b_col, rr.a_col_abs && !rr.b_col_abs)
        } else {
            return None;
        };
        let key = (si, rr.a_row, rr.a_col, vertical);
        if !anchored && !self.sum_prefixes.contains_key(&key) {
            if self.sum_ends.len() >= 1024 { self.sum_ends.clear(); }
            let previous = self.sum_ends.insert(key, end);
            if previous.is_none_or(|last| last == end) { return None; }
        }
        let whole_axis = if vertical { 1_048_575 } else { 16_383 };
        if (start == 0 && end == whole_axis) || end - start >= 200_000 {
            let (max_row, max_col) = self.sheet_bounds(si);
            if (vertical && rr.a_col > i64::from(max_col)) || (!vertical && rr.a_row > i64::from(max_row)) {
                return None;
            }
            return Some((key, end.min(i64::from(if vertical { max_row } else { max_col }))));
        }
        Some((key, end))
    }

    fn prefix_sum(&mut self, key: PrefixKey, end: i64) -> FValue {
        let (si, row, col, vertical) = key;
        let start = if vertical { row } else { col };
        if end < start { return FValue::number(0.0); }
        let range = if vertical { (si, row, col, end, col) } else { (si, row, col, row, end) };
        let length = (end - start + 1) as usize;
        let mut prefix = self.sum_prefixes.remove(&key).unwrap_or_default();
        self.prefix_values -= prefix.len();
        let mut sum = prefix.last().copied().unwrap_or(Ok(0.0));
        let mut cells = RangeRemainder { range, offsets: prefix.len()..length };
        while let Some((si,r,c)) = cells.next() {
            self.range_reads += 1;
            let value = self.cell_value(si, r, c).scalar();
            if self.requested.is_some() {
                self.range_remainder = Some(cells);
                return FValue::Error(ErrKind::Ref);
            }
            if let Ok(n) = sum {
                sum = match value {
                    FValue::Number(v) => Ok(n + v), FValue::Error(e) => Err(e), _ => Ok(n),
                };
            }
            prefix.push(sum);
        }
        let result = prefix[length - 1];
        if self.prefix_values + prefix.len() > 1_048_576 {
            self.sum_prefixes.clear();
            self.prefix_values = 0;
        }
        if let Some(nested) = self.sum_prefixes.insert(key, prefix) {
            self.prefix_values -= nested.len();
        }
        self.prefix_values += self.sum_prefixes[&key].len();
        result.map(FValue::number).unwrap_or_else(FValue::Error)
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

    /// 单列精确非空末行（通过 Store `page_bits` + `prev_occupied_before` O(1) 位图获取并缓存）
    fn col_max_row(&mut self, si: usize, col: u32) -> u32 {
        if let Some(&r) = self.col_bounds.get(&(si, col)) {
            return r;
        }
        let (sheet_max_r, _) = self.sheet_bounds(si);
        let r = self
            .wb
            .sheets
            .get(si)
            .and_then(|sheet| sheet.next_occupied_in_col(col, sheet_max_r.saturating_add(1), false))
            .unwrap_or(0);
        self.col_bounds.insert((si, col), r);
        r
    }

    fn get_or_build_approx_positions(
        &mut self,
        range: RangeKey,
        data: &[FValue],
        n: usize,
        stride: usize,
        key: &FValue,
    ) -> Option<std::sync::Arc<Vec<usize>>> {
        const MAX_POSITIONS: usize = 1_048_576;
        let kind = functions::LookupKind::of(key)?;
        let index = (range, stride, kind);
        if let Some(pos) = self.approx_positions.get(&index) {
            return pos.clone();
        }
        if let Some(shared) = self.shared
            && let Some(pos) = shared.get_approx(&index)
        {
            return pos;
        }
        let pos = kind.positions(data, n, stride).map(std::sync::Arc::new);
        let count = pos.as_ref().map_or(0, |v| v.len());
        if count <= MAX_POSITIONS && self.requested.is_none() {
            if let Some(shared) = self.shared {
                shared.put_approx(index, pos.clone());
            }
            if self.position_count + count > MAX_POSITIONS || self.approx_positions.len() >= 4096 {
                self.approx_positions.clear();
                self.position_count = 0;
            }
            self.position_count += count;
            self.approx_positions.insert(index, pos.clone());
        }
        pos
    }

    fn approximate_match(
        &mut self, range: RangeKey, data: &[FValue], n: usize, stride: usize, key: &FValue, ascending: bool,
    ) -> Option<usize> {
        if functions::LookupKind::of(key).is_none() {
            return None;
        }
        let pos = self.get_or_build_approx_positions(range, data, n, stride, key);
        functions::search_sorted(data, n, stride, pos.as_deref().map(Vec::as_slice), key, ascending)
    }

    /// 精确匹配索引：零格式化 LookupKey → 首/末行下标（支持 256 行页级增量更新）
    fn lookup_hash_coords(
        &mut self,
        si: usize,
        ar: i64,
        ac: i64,
        br: i64,
        bc: i64,
        vertical: bool,
    ) -> lookup_cache::Index {
        let Some(key) = self.grid_key_coords(si, ar, ac, br, bc) else { return Default::default(); };
        let local = (key, vertical);
        if let Some(ix) = self.lookup_index.get(&local) {
            return ix.clone();
        }
        if let Some(shared) = self.shared
            && let Some(ix) = shared.get_lookup(local)
        {
            self.lookup_index.insert(local, ix.clone());
            return ix;
        }
        let sheet = &self.wb.sheets[si];
        let span = if vertical { key.3 - key.1 } else { key.4 - key.2 };
        if vertical && key.2 == key.4 && span >= 255 {
            let col_u32 = key.2 as u32;
            let has_scope = self.scope.is_some() || self.fast_scope.is_some();
            let overlay_cb = |abs_row: u32| self.ready_dirty_value_at(si, abs_row, col_u32);
            let eval_scope = lookup_cache::LookupEvalScope {
                scope: self.scope,
                fast_scope: self.fast_scope,
                overlay: has_scope
                    .then_some(&overlay_cb as &dyn Fn(u32) -> Option<crate::Value>),
                cancel: self.cancel,
                deadline: self.deadline,
            };
            if let Some((ix, reads)) =
                sheet
                    .shared_lookup
                    .get_or_build_vertical(sheet, key, &eval_scope)
            {
                self.range_reads += reads;
                if !self.check_abort() {
                    if let Some(shared) = self.shared {
                        shared.put_lookup(local, &ix);
                    }
                    self.lookup_index.insert(local, ix.clone());
                }
                return ix;
            }
        }
        if !vertical && span >= 255 && let Some(ix) = sheet.shared_lookup.get(sheet, local) {
            if let Some(shared) = self.shared {
                shared.put_lookup(local, &ix);
            }
            self.lookup_index.insert(local, ix.clone());
            return ix;
        }
        let (rows, cols, data) = self.cached_grid_coords(si, ar, ac, br, bc);
        let count = if vertical { rows } else { cols };
        let mut map = lookup_cache::LookupMap::with_capacity(count);
        for i in 0..count {
            let v = if vertical {
                data.get(i * cols.max(1))
            } else {
                data.get(i)
            };
            if let Some(v) = v {
                map.insert_next(lookup_cache::LookupKey::from_fvalue(v), i as u32);
            }
        }
        let index = std::sync::Arc::new(map);
        if self.requested.is_none() && !self.aborted {
            if !vertical && count >= 256 {
                let sheet = &self.wb.sheets[si];
                let (_, r1, c1, _, _) = key;
                let rect = (
                    r1 as u32,
                    c1 as u32,
                    r1 as u32,
                    c1 as u32 + cols.saturating_sub(1) as u32,
                );
                if !sheet.iter_rect(rect).any(|(_, c)| c.formula.is_some()) {
                    sheet.shared_lookup.insert(sheet, local, index.clone());
                }
            }
            if let Some(shared) = self.shared {
                shared.put_lookup(local, &index);
            }
            self.lookup_index.insert(local, index.clone());
        }
        index
    }

    fn ext_lookup_hash_coords(
        &mut self,
        li: usize,
        slot: usize,
        ar: i64,
        ac: i64,
        br: i64,
        bc: i64,
        vertical: bool,
    ) -> lookup_cache::Index {
        let mut r1 = ar.min(br).max(0) as u32;
        let mut c1 = ac.min(bc).max(0) as u32;
        let mut r2 = ar.max(br).max(0) as u32;
        let mut c2 = ac.max(bc).max(0) as u32;
        let full_cols = c1 == 0 && c2 == 16_383;
        let full_rows = r1 == 0 && r2 == 1_048_575;
        let huge = (r2 as u64 - r1 as u64 + 1) * (c2 as u64 - c1 as u64 + 1) > 200_000;
        if full_cols || full_rows || huge {
            let (max_r, max_c) = self.ext_sheet_bounds(li, slot);
            if full_rows || huge {
                r2 = r2.min(max_r);
            }
            if full_cols || huge {
                c2 = c2.min(max_c);
            }
            if r1 > r2 || c1 > c2 {
                r1 = 0;
                c1 = 0;
                r2 = 0;
                c2 = 0;
            }
        }
        let ekey = (li, slot, r1 as i64, c1 as i64, r2 as i64, c2 as i64, vertical);
        if let Some(ix) = self.ext_lookup_index.get(&ekey) {
            return ix.clone();
        }
        if let Some(shared) = self.shared
            && let Some(ix) = shared.get_ext_lookup(&ekey)
        {
            self.ext_lookup_index.insert(ekey, ix.clone());
            return ix;
        }
        let link = &self.wb.links[li];
        let mut map = lookup_cache::LookupMap::default();
        let mut first_blank: Option<u32> = None;
        let mut last_blank: Option<u32> = None;
        if vertical {
            let mut next_expected = r1;
            for (&(_, r, c), v) in link.cache.range((slot, r1, c1)..=(slot, r2, c1)) {
                if c != c1 {
                    continue;
                }
                if r > next_expected {
                    let fb = next_expected - r1;
                    let lb = r - 1 - r1;
                    first_blank.get_or_insert(fb);
                    last_blank = Some(lb);
                }
                if v.is_empty() {
                    let off = r - r1;
                    first_blank.get_or_insert(off);
                    last_blank = Some(off);
                } else {
                    map.insert_next(lookup_cache::LookupKey::from_value(v), r - r1);
                }
                next_expected = r.saturating_add(1);
            }
            if next_expected <= r2 {
                first_blank.get_or_insert(next_expected - r1);
                last_blank = Some(r2 - r1);
            }
        } else {
            for c in c1..=c2 {
                let off = c - c1;
                match link.cache.get(&(slot, r1, c)) {
                    Some(v) if !v.is_empty() => {
                        map.insert_next(lookup_cache::LookupKey::from_value(v), off);
                    }
                    _ => {
                        first_blank.get_or_insert(off);
                        last_blank = Some(off);
                    }
                }
            }
        }
        if let (Some(fb), Some(lb)) = (first_blank, last_blank) {
            map.insert_next(lookup_cache::LookupKey::Empty, fb);
            if lb != fb {
                map.insert_next(lookup_cache::LookupKey::Empty, lb);
            }
        }
        let ix = std::sync::Arc::new(map);
        if let Some(shared) = self.shared {
            shared.put_ext_lookup(ekey, &ix);
        }
        self.ext_lookup_index.insert(ekey, ix.clone());
        ix
    }

    /// 同一轮重算里，同一块区域只物化一次
    fn cached_grid_coords(
        &mut self,
        si: usize,
        ar: i64,
        ac: i64,
        br: i64,
        bc: i64,
    ) -> (usize, usize, std::sync::Arc<Vec<FValue>>) {
        let Some(key) = self.grid_key_coords(si, ar, ac, br, bc) else { return (0, 0, Default::default()); };
        let (_, r1, c1, r2, c2) = key;
        let rows = (r2 - r1 + 1) as usize;
        let cols = (c2 - c1 + 1) as usize;
        match self.materialize_range(key, rows * cols) {
            Ok(values) => (rows, cols, values),
            Err(_) => (0, 0, Default::default()),
        }
    }

    fn grid_key_coords(&mut self, si: usize, ar: i64, ac: i64, br: i64, bc: i64) -> Option<RangeKey> {
        let (max_r, max_c) = self.sheet_bounds(si);
        let r1 = ar.min(br);
        let c1 = ac.min(bc);
        let mut r2 = ar.max(br);
        let mut c2 = ac.max(bc);
        let full_cols = c1 == 0 && c2 == 16_383;
        let full_rows = r1 == 0 && r2 == 1_048_575;
        if full_cols {
            c2 = c2.min(max_c as i64);
        }
        if full_rows {
            r2 = r2.min(max_r as i64);
        }
        let area = (r2 - r1 + 1).saturating_mul(c2 - c1 + 1);
        if area > 200_000 {
            r2 = r2.min(max_r as i64);
            c2 = c2.min(max_c as i64);
        }
        (r2 >= r1 && c2 >= c1).then_some((si, r1, c1, r2, c2))
    }

    fn visible_summary_values(
        &self, arg: Arg, origin: (u32, u32), si: usize, skip_manual: bool, skip_subtotals: bool,
    ) -> Arg {
        let Arg::Range { values, rows, cols } = arg else { return arg };
        let sheet = &self.wb.sheets[si];
        if !skip_subtotals && sheet.hidden_rows.is_empty() && (!skip_manual || sheet.hidden_rows_user.is_empty()) {
            return Arg::Range { values, rows, cols };
        }
        let mut out: Option<Vec<FValue>> = None;
        let mut kept_rows = 0;
        for i in 0..rows {
            let row = origin.0 + i as u32;
            let hidden = sheet.hidden_rows.contains(&row) || (skip_manual && sheet.hidden_rows_user.contains(&row));
            if !hidden { kept_rows += 1; }
            for j in 0..cols {
                let col = origin.1 + j as u32;
                let index = i * cols + j;
                if hidden || (skip_subtotals && sheet.formula_of(row, col)
                    .is_some_and(|src| contains_subtotal(self.wb, si, src, row, col))) {
                    // Preserve shared materialized ranges until a value is actually omitted.
                    out.get_or_insert_with(|| values[..index].to_vec());
                } else if let Some(out) = &mut out {
                    out.push(values[index].clone());
                }
            }
        }
        match out {
            Some(out) if skip_subtotals => Arg::Range { rows: out.len(), cols: 1, values: out.into() },
            Some(out) => Arg::Range { rows: kept_rows, cols, values: out.into() },
            None => Arg::Range { rows, cols, values },
        }
    }

    fn function_args(&mut self, id: FuncId, nodes: &[Node], si: usize) -> Vec<Arg> {
        let mut args: Vec<Arg> = Vec::with_capacity(nodes.len());
        let amounts = if id == FuncId::SumIf { self.sumif_amount_node_shifted(nodes) } else { None };
        for (i, n) in nodes.iter().enumerate() {
            let n = if i == 2 { amounts.as_ref().unwrap_or(n) } else { n };
            // Every SUBTOTAL excludes filtered rows. Codes 101..111 also exclude manual hiding.
            let visibility = match id {
                FuncId::Subtotal if i > 0 => Some(args[0].first().to_number().unwrap_or(0.0) >= 100.0),
                FuncId::Aggregate if i > 1 && matches!(args[1].first().to_number(), Ok(5.0 | 6.0)) => Some(true),
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
                            let origin = (
                                self.r_row(a).min(self.r_row(b)).max(0) as u32,
                                self.r_col(a).min(self.r_col(b)).max(0) as u32,
                            );
                            arg = self.visible_summary_values(arg, origin, source, skip_manual, id == FuncId::Subtotal);
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

    fn call_generic_id(&mut self, id: FuncId, name: &str, nodes: &[Node], si: usize) -> FValue {
        let args = self.function_args(id, nodes, si);
        functions::call_id_in(self.ctx, id, name, &args)
    }

    #[cfg(test)]
    fn call_generic(&mut self, name: &str, nodes: &[Node], si: usize) -> FValue {
        self.call_generic_id(FuncId::from_upper(name), name, nodes, si)
    }

    fn grid_of(&mut self, n: &Node, si: usize) -> (usize, usize, std::sync::Arc<Vec<FValue>>) {
        match n {
            Node::Ref(re) => {
                if re.sheet2.is_some() || re.book.is_some() {
                    return match self.eval_node(n, si) {
                        FValue::Array(a) => (a.rows, a.cols, a.data.clone()),
                        FValue::Error(e) => (1, 1, vec![FValue::Error(e)].into()),
                        v => (1, 1, vec![v].into()),
                    };
                }
                match self.sheet_index(re.sheet.as_deref(), si) {
                    Ok(idx) => {
                        let r = self.r_row(re).max(0) as u32;
                        let c = self.r_col(re).max(0) as u32;
                        let v = self.cell_value(idx, r, c).scalar();
                        (1, 1, vec![v].into())
                    }
                    Err(_) => (0, 0, vec![].into()),
                }
            }
            Node::Spill(re) => {
                if re.book.is_some() {
                    return (1, 1, vec![FValue::Error(ErrKind::Ref)].into());
                }
                match self.sheet_index(re.sheet.as_deref(), si) {
                    Ok(idx) => {
                        let r = self.r_row(re).max(0) as u32;
                        let c = self.r_col(re).max(0) as u32;
                        match self.spill_value(idx, r, c) {
                            FValue::Array(a) => (a.rows, a.cols, a.data.clone()),
                            FValue::Error(e) => (1, 1, vec![FValue::Error(e)].into()),
                            _ => (1, 1, vec![FValue::Error(ErrKind::Ref)].into()),
                        }
                    }
                    Err(_) => (1, 1, vec![FValue::Error(ErrKind::Ref)].into()),
                }
            }
            Node::Range(a, b) => {
                if a.sheet2.is_some() || b.sheet2.is_some() || a.book.is_some() || b.book.is_some() {
                    return match self.eval_node(n, si) {
                        FValue::Array(a) => (a.rows, a.cols, a.data.clone()),
                        FValue::Error(e) => (0, 0, vec![FValue::Error(e)].into()),
                        _ => (0, 0, vec![].into()),
                    };
                }
                match self.range_values(a, b, si) {
                    Ok(Arg::Range { values, rows, cols }) => (rows, cols, values),
                    _ => (0, 0, vec![].into()),
                }
            }
            _ => match self.eval_node(n, si) {
                FValue::Array(a) => (a.rows, a.cols, a.data.clone()),
                FValue::Error(e) => (0, 0, vec![FValue::Error(e)].into()),
                v => (1, 1, vec![v].into()),
            },
        }
    }

    /// 应用一等公民 lambda（`<= 4` 参数使用内联栈帧，零 HashMap 堆分配）
    fn apply_lambda_val(
        &mut self,
        lam: &crate::formula::value::LambdaVal,
        vals: Vec<FValue>,
        si: usize,
    ) -> FValue {
        let mut frame = value::EnvFrame::with_capacity(lam.params.len());
        let mut vals_it = vals.into_iter();
        for p in lam.params.iter() {
            let v = vals_it.next().unwrap_or(FValue::Empty);
            frame.insert_arc(p.clone(), v);
        }
        let base = self.env_stack.len();
        for f in lam.env.iter() {
            self.env_stack.push(f.clone());
        }
        self.env_stack.push(frame);
        let prev_off = self.ref_offset;
        self.ref_offset = (0, 0);
        let out = self.eval_node(&lam.body, si);
        self.ref_offset = prev_off;
        self.env_stack.truncate(base);
        out
    }

    /// 应用 LAMBDA：把实参绑定到形参后求值函数体
    fn apply_lambda(&mut self, lam: &Node, vals: Vec<FValue>, si: usize) -> FValue {
        if let Node::Func(n, parts) = lam {
            if n.id() == FuncId::Lambda && !parts.is_empty() {
                let params = &parts[..parts.len() - 1];
                let body = &parts[parts.len() - 1];
                let mut frame = value::EnvFrame::with_capacity(params.len());
                let mut vals_it = vals.into_iter();
                for p in params {
                    let v = vals_it.next().unwrap_or(FValue::Empty);
                    if let Node::Name(pn) = p {
                        frame.insert(pn.as_str(), v);
                    }
                }
                self.env_stack.push(frame);
                let out = self.eval_node(body, si);
                self.env_stack.pop();
                return out;
            }
        }
        FValue::Error(ErrKind::Value)
    }

    /// 二元运算的操作数一律当成二维块：区域/数组保留 rows×cols，标量是 1×1。
    /// 旧实现直接摊平成 Vec，长度相等就按 1×n 输出，纵向数组会被写成横的。
    fn operand_block(&mut self, n: &Node, si: usize) -> Arg {
        if let Node::Range(a, b) = n {
            if a.sheet2.is_none() && b.sheet2.is_none() {
                if let Ok(range @ Arg::Range { .. }) = self.range_values(a, b, si) {
                    return range;
                }
            }
        }
        Arg::Val(self.eval_node(n, si))
    }

    fn eval_numeric_unary(&mut self, n: &Node, si: usize, op: impl Fn(f64) -> f64) -> FValue {
        let arg = self.operand_block(n, si);
        let (rows, cols) = functions::range_dims(&arg);
        if arg.slice().is_empty() { return FValue::Error(ErrKind::Value); }
        let apply = |v: &FValue| v.to_number().map(|x| FValue::number(op(x))).unwrap_or_else(FValue::Error);
        if rows == 1 && cols == 1 {
            return arg.slice().first().map(apply).unwrap_or(FValue::Error(ErrKind::Value));
        }
        let out = arg.slice().iter().map(apply).collect();
        FValue::Array(std::sync::Arc::new(value::Array::new(rows, cols, out)))
    }

    fn eval_binary(&mut self, op: BinOp, l: &Node, r: &Node, si: usize) -> FValue {
        let left = self.operand_block(l, si);
        let right = self.operand_block(r, si);
        let (lr, lc) = functions::range_dims(&left);
        let (rr, rc) = functions::range_dims(&right);
        let (lv, rv) = (left.slice(), right.slice());
        if lv.is_empty() || rv.is_empty() { return FValue::Error(ErrKind::Value); }
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
        FValue::Array(std::sync::Arc::new(value::Array::new(rows, cols, out)))
    }
}

impl conditional_index::CriteriaDataSource for Calc<'_> {
    #[inline]
    fn load_col(&mut self, rk: RangeKey) -> Option<std::sync::Arc<Vec<FValue>>> {
        let len = ((rk.3 - rk.1 + 1) as usize).saturating_mul((rk.4 - rk.2 + 1) as usize);
        self.materialize_range(rk, len).ok()
    }

    #[inline]
    fn shared_cond_col(&mut self, rk: RangeKey) -> Option<std::sync::Arc<lookup_cache::SharedCondColumn>> {
        self.shared_cond_col_internal(rk)
    }

    #[inline]
    fn probe_cell(&mut self, rk: RangeKey, row_offset: usize) -> Option<FValue> {
        let (si_p, r1_p, c1_p, r2_p, c2_p) = rk;
        let cols_p = (c2_p - c1_p + 1).max(1) as usize;
        let r = r1_p + (row_off_div(row_offset, cols_p)) as i64;
        let c = c1_p + (row_offset % cols_p) as i64;
        if r > r2_p || c > c2_p || r < 0 || c < 0 {
            return None;
        }
        let v = self.cell_value(si_p, r as u32, c as u32).scalar();
        if self.requested.is_some() {
            None
        } else {
            Some(v)
        }
    }
}

#[inline]
fn aligned_col_end_row(r1: i64, max_r: u32, orig_r2: i64) -> i64 {
    i64::from(max_r).max(r1 + 255).min(orig_r2)
}

#[inline]
fn row_off_div(off: usize, cols: usize) -> usize {
    off / cols
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
            let mut out = a;
            out.push_str(&b);
            FValue::Text(out.into())
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
        V::Text(s) => FValue::Text(std::sync::Arc::clone(s)),
        V::Bool(b) => FValue::Bool(*b),
        V::Error(e) => FValue::Error(*e),
    }
}

pub fn from_fvalue(v: &FValue) -> crate::model::Value {
    use crate::model::Value as V;
    match v {
        FValue::Empty => V::Empty,
        FValue::Number(n) => V::Number(*n),
        FValue::Text(s) => V::Text(std::sync::Arc::clone(s)),
        FValue::Bool(b) => V::Bool(*b),
        FValue::Error(e) => V::Error(*e),
        FValue::Array(a) => from_fvalue(&a.get(0, 0)),
        // 函数值落盘没有对应类型，Excel 里显示 #CALC!
        FValue::Lambda(_) => V::Text("#CALC!".into()),
    }
}

fn callable_cache_candidate(value: &crate::Value) -> bool {
    matches!(value, crate::Value::Text(text) if text.as_ref() == "#CALC!")
}

thread_local! {
    static FN_PROF: std::cell::RefCell<HashMap<String, (u64, f64)>> =
        std::cell::RefCell::new(HashMap::new());
}

fn profile_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("SC_PROFILE").is_some())
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
    let rows = crate::expand_whole_rows_cow(src);
    let cols = crate::expand_whole_cols_cow(&rows);
    parse_normalized(&cols)
}

pub(crate) fn parse_normalized(src: &str) -> Result<Node, ErrKind> {
    let toks = lex(src)?;
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

/// Early file caches can contain a text error written by an older application.
pub fn is_cached_spill_error(value: &crate::Value) -> bool {
    matches!(value, crate::Value::Error(ErrKind::Spill))
        || matches!(value, crate::Value::Text(text) if text.as_ref() == "#SPILL!")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SpillProbe {
    Footprint(u32, u32),
    NotSpillAnchor,
    Unresolved,
}

/// Probe a deterministic formula cell carrying `#SPILL!` so `Deps` can register a
/// localized `spill_watch` rectangle instead of forcing `requires_full_recalc()`.
pub(crate) fn probe_blocked_spill_shape(wb: &Workbook, key: deps::CellKey) -> SpillProbe {
    let (si, r, c) = key;
    let Some(f) = wb
        .sheets
        .get(si)
        .and_then(|s| s.formula_of(r, c))
    else {
        return SpillProbe::NotSpillAnchor;
    };
    let raw = f.strip_prefix('=').unwrap_or(f);
    let Some(rects) = deps::parallel_inputs(wb, key, raw) else {
        return SpillProbe::Unresolved;
    };
    for rect in &rects {
        let Some(sh) = wb.sheets.get(rect.sheet) else {
            return SpillProbe::Unresolved;
        };
        let r1 = rect.r1.max(0) as u32;
        let c1 = rect.c1.max(0) as u32;
        let r2 = rect.r2.max(0) as u32;
        let c2 = rect.c2.max(0) as u32;
        if u64::from(r2.saturating_sub(r1) + 1) * u64::from(c2.saturating_sub(c1) + 1) > 65_536 {
            return SpillProbe::Unresolved;
        }
        if sh
            .iter_rect((r1, c1, r2, c2))
            .any(|(_, cell)| cell.formula.is_some() && cell.value == crate::Value::Empty)
        {
            return SpillProbe::Unresolved;
        }
    }
    let has_tables = wb.sheets.iter().any(|s| !s.tables.is_empty());
    let expanded = if has_tables && raw.contains('[') {
        crate::table::expand_structured_at(wb, si, Some(r), Some(c), raw)
    } else {
        None
    };
    let body = expanded.as_deref().unwrap_or(raw);
    let mut anchor_scope = HashSet::with_capacity(1);
    anchor_scope.insert(key);
    let ctx = functions::EvalCtx::for_workbook(wb.date_system);
    let mut calc = Calc::new_worker(wb, ctx, Some(&anchor_scope), None);
    calc.cur_sheet = si;
    calc.cur = (r, c);
    let val = match compiled::with_shifted(body, r, c, |node, dr, dc| calc.eval_node_shifted(node, si, dr, dc)) {
        Ok(v) => v,
        Err(_) => return SpillProbe::NotSpillAnchor,
    };
    match val {
        FValue::Array(a) if a.rows > 1 || a.cols > 1 => {
            if u64::from(a.rows as u32) * u64::from(a.cols as u32) <= 65_536 {
                SpillProbe::Footprint(a.rows as u32, a.cols as u32)
            } else {
                SpillProbe::Unresolved
            }
        }
        _ => SpillProbe::NotSpillAnchor,
    }
}

fn array_blocked(wb: &Workbook, si: usize, r: u32, c: u32, a: &value::Array, ignore_old_spills: bool) -> bool {
    if a.rows == 0 || a.cols == 0 {
        return false;
    }
    if r as u64 + a.rows as u64 > 1_048_576 || c as u64 + a.cols as u64 > 16_384 {
        return true;
    }
    if a.rows == 1 && a.cols == 1 {
        return false;
    }
    let sheet = &wb.sheets[si];
    let r2 = r + a.rows as u32 - 1;
    let c2 = c + a.cols as u32 - 1;
    let anchor = CellRef::new(r, c);
    sheet.iter_rect((r, c, r2, c2)).any(|(key, cell)| {
        key != anchor
            && !cell.is_blank()
            && !(ignore_old_spills && sheet.spilled.contains(&key))
    })
}

/// 把求值结果写回工作表（处理动态数组溢出与 #SPILL!）
/// 调用方在写入数组前清空旧溢出区域，已被本批次写入的格子也属于冲突。
pub fn write_back(wb: &mut Workbook, results: Vec<(usize, u32, u32, FValue)>) {
    for (si, r, c, v) in results {
            match v {
                FValue::Array(a) => {
                    // 检查溢出区域是否被别的数据占着
                    if array_blocked(wb, si, r, c, &a, false) {
                        // Track the blocked anchor even when no tail cells exist.
                        // A later edit may clear the obstruction anywhere in its footprint.
                        wb.sheets[si].spill_shapes.insert(CellRef::new(r, c), (0, 0));
                        wb.sheets[si].set_formula_cache(r, c, from_fvalue(&FValue::Error(ErrKind::Spill)));
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
                                wb.sheets[si].set_formula_cache(key.row, key.col, val);
                            } else {
                                wb.sheets[si].set(key.row, key.col, Cell { value: val, formula: None });
                                wb.sheets[si].spilled.insert(key);
                            }
                        }
                    }
                }
                other => {
                    let anchor = CellRef::new(r, c);
                    if matches!(other, FValue::Error(ErrKind::Spill))
                        && !matches!(
                            probe_blocked_spill_shape(wb, (si, r, c)),
                            SpillProbe::NotSpillAnchor
                        )
                    {
                        wb.sheets[si].spill_shapes.insert(anchor, (0, 0));
                    } else if !wb.sheets[si].spill_shapes.is_empty() {
                        wb.sheets[si].spill_shapes.remove(&anchor);
                    }
                    wb.sheets[si].set_formula_cache(r, c, from_fvalue(&other));
                }
            }
        }
}

/// 手动模式：把受影响的表记入待计算集合，供“计算工作表”逐表清理。
pub fn mark_pending(wb: &mut Workbook, deps: &deps::Deps, seeds: &[deps::CellKey]) {
    let mut sheets: std::collections::BTreeSet<usize> =
        seeds.iter().map(|(si, _, _)| *si).collect();
    if !deps.is_empty() {
        for (si, _, _) in deps.edit_closure(seeds) {
            sheets.insert(si);
        }
    }
    wb.calc_pending_sheets.extend(sheets);
    wb.calc_pending = !wb.calc_pending_sheets.is_empty();
}

/// 增量重算：只算受影响的公式单元格，其余用缓存值。
/// 返回实际重算的格子数。
pub fn recalc_dirty(wb: &mut Workbook, deps: &deps::Deps, seeds: &[deps::CellKey]) -> usize {
    recalc_dirty_impl(wb, deps, seeds, None).unwrap_or_default()
}

/// A cancelled worker snapshot must be discarded; a completed snapshot is ready to publish.
pub fn recalc_dirty_cancelable(
    wb: &mut Workbook,
    deps: &deps::Deps,
    seeds: &[deps::CellKey],
    cancel: &std::sync::atomic::AtomicBool,
) -> bool {
    recalc_dirty_impl(wb, deps, seeds, Some(cancel)).is_some()
}

/// Existing materialized spills and probed `#SPILL!` anchors tracked in `deps.spill_watches`
/// are handled locally by `recalc_dirty` without whole-workbook fallback. Only legacy CSE
/// `sheet.arrays` mismatches or stale/untracked spill footprints require full stabilization.
fn edit_touches_spill(wb: &Workbook, deps: &deps::Deps, seeds: &[deps::CellKey], deps_ready: bool) -> bool {
    wb.sheets.iter().enumerate().any(|(si, sheet)| {
        sheet.arrays.iter().any(|&(anchor, (r1, c1, r2, c2))| {
            // A legacy one-cell CSE formula may legitimately return a scalar; there
            // are no spill tails to restore. An edit producing a new array is caught
            // by the dirty evaluator before writeback.
            if (anchor.row, anchor.col) == (r1, c1) && (r1, c1) == (r2, c2)
                && !sheet.spill_shapes.contains_key(&anchor)
                && sheet.get(r1, c1).is_some_and(|c| c.formula.is_some()) {
                return false;
            }
            anchor.row != r1 || anchor.col != c1 || r2 < r1 || c2 < c1
                || sheet.spill_shapes.get(&anchor) != Some(&(r2.saturating_sub(r1).saturating_add(1),
                    c2.saturating_sub(c1).saturating_add(1)))
        })
            || (sheet.spill_shapes.is_empty() && !sheet.spilled.is_empty())
            || (!sheet.spill_shapes.is_empty() && wb.calc_iterate)
            || {
                let mut untracked_count = 0usize;
                let mut sheet_seeds_count: Option<usize> = None;
                let mut hit = false;
                for (anchor, &(rows, cols)) in &sheet.spill_shapes {
                    let tracked = deps_ready && deps.tracks_spill((si, anchor.row, anchor.col));
                    if rows == 0 || cols == 0 {
                        if tracked {
                            continue;
                        }
                        if deps_ready
                            && matches!(
                                probe_blocked_spill_shape(wb, (si, anchor.row, anchor.col)),
                                SpillProbe::NotSpillAnchor
                            )
                        {
                            continue;
                        }
                        hit = true;
                        break;
                    }
                    if tracked {
                        continue;
                    }
                    untracked_count += 1;
                    let s_cnt = *sheet_seeds_count
                        .get_or_insert_with(|| seeds.iter().filter(|&&(s, _, _)| s == si).count());
                    if untracked_count.saturating_mul(s_cnt) > 100_000 {
                        hit = true;
                        break;
                    }
                    if seeds.iter().any(|&(s, r, c)| {
                        s == si
                            && r >= anchor.row
                            && r - anchor.row < rows
                            && c >= anchor.col
                            && c - anchor.col < cols
                    }) {
                        hit = true;
                        break;
                    }
                }
                hit
            }
    })
}

/// Fast check: returns `true` and commits in-place if all results are scalars (with no
/// disappearing `spill_shapes`) or dynamic arrays whose shape and tail values are unchanged.
fn write_dirty_results(wb: &mut Workbook, mut results: Vec<(usize, u32, u32, FValue)>) -> bool {
    let mut stable = true;
    results.retain(|(si, r, c, value)| {
        let sh = &wb.sheets[*si];
        let old = sh.spill_shapes.get(&CellRef::new(*r, *c));
        match value {
            FValue::Array(a) if a.rows > 0 && a.cols > 0 &&
                old.is_some_and(|&(h, w)| (h as usize, w as usize) == (a.rows, a.cols)) => {
                if (a.rows, a.cols) == (1, 1) { return true; }
                stable &= (0..a.rows).all(|y| (0..a.cols).all(|x| {
                    let key = CellRef::new(*r + y as u32, *c + x as u32);
                    (y == 0 && x == 0 || sh.spilled.contains(&key))
                        && sh.value(key.row, key.col) == from_fvalue(&a.get(y, x))
                }));
                false
            }
            FValue::Array(_) => { stable = false; false }
            _ if old.is_some() => { stable = false; false }
            _ => true,
        }
    });
    if stable { write_back(wb, results); }
    stable
}

/// Apply dirty calculation results locally, including expanding/shrinking/unblocking/blocking
/// dynamic array spills, and return every cell key whose value or `#` spill shape changed so
/// downstream readers can be converged incrementally via `deps.closure`.
/// Returns `Err(())` only if a legacy CSE `sheet.arrays` anchor changes shape.
fn apply_dirty_results_local(
    wb: &mut Workbook,
    deps: &deps::Deps,
    results: Vec<(usize, u32, u32, FValue)>,
) -> Result<Vec<deps::CellKey>, ()> {
    let any_spill_touch = results.iter().any(|(si, r, c, v)| {
        matches!(v, FValue::Array(_))
            || (!wb.sheets[*si].spill_shapes.is_empty()
                && wb.sheets[*si].spill_shapes.contains_key(&CellRef::new(*r, *c)))
    });
    if !any_spill_touch {
        write_back(wb, results);
        return Ok(Vec::new());
    }
    let array_sets: Vec<Option<FxHashSet<CellRef>>> = wb
        .sheets
        .iter()
        .map(|sh| {
            (sh.arrays.len() > 8).then(|| sh.arrays.iter().map(|&(a, _)| a).collect())
        })
        .collect();
    for (si, r, c, v) in &results {
        let anchor = CellRef::new(*r, *c);
        let is_cse = match &array_sets[*si] {
            Some(set) => set.contains(&anchor),
            None => wb.sheets[*si].arrays.iter().any(|&(a, _)| a == anchor),
        };
        if is_cse {
            let old = wb.sheets[*si].spill_shapes.get(&anchor).copied();
            let same = match (old, v) {
                (Some((h, w)), FValue::Array(a)) => (h as usize, w as usize) == (a.rows, a.cols),
                (None, other) if !matches!(other, FValue::Array(_)) => true,
                _ => false,
            };
            if !same {
                return Err(());
            }
        }
    }
    if write_dirty_results(wb, results.clone()) {
        return Ok(Vec::new());
    }

    let edited_before: Vec<bool> = wb.sheets.iter().map(|s| s.edited).collect();
    let mut before_cells: HashMap<deps::CellKey, crate::Value> = HashMap::new();
    let mut old_shapes: HashMap<deps::CellKey, Option<(u32, u32)>> = HashMap::new();
    let mut array_anchors: HashSet<deps::CellKey> = HashSet::new();

    // 1. Record old values of affected spill footprints and clear ONLY re-evaluated anchors' old tails
    for (si, r, c, v) in &results {
        let anchor = CellRef::new(*r, *c);
        let prev_shape = wb.sheets[*si].spill_shapes.remove(&anchor);
        old_shapes.insert((*si, *r, *c), prev_shape);
        if let Some((old_rows, old_cols)) = prev_shape {
            for dy in 0..old_rows {
                for dx in 0..old_cols {
                    if dy == 0 && dx == 0 {
                        continue;
                    }
                    let tail = CellRef::new(*r + dy, *c + dx);
                    if wb.sheets[*si].spilled.remove(&tail) {
                        before_cells
                            .entry((*si, tail.row, tail.col))
                            .or_insert_with(|| wb.sheets[*si].value(tail.row, tail.col));
                        wb.sheets[*si].set(tail.row, tail.col, Cell::default());
                    }
                }
            }
        }
        if let FValue::Array(a) = v {
            array_anchors.insert((*si, *r, *c));
            if *r as u64 + a.rows as u64 <= 1_048_576 && *c as u64 + a.cols as u64 <= 16_384 {
                for dy in 0..a.rows as u32 {
                    for dx in 0..a.cols as u32 {
                        if dy == 0 && dx == 0 {
                            continue;
                        }
                        let tk = (*si, *r + dy, *c + dx);
                        before_cells
                            .entry(tk)
                            .or_insert_with(|| wb.sheets[*si].value(tk.1, tk.2));
                    }
                }
            }
        }
    }

    // 2. Materialize new scalars and dynamic arrays in deterministic row-major order
    write_back(wb, results);

    for (sheet, was) in wb.sheets.iter_mut().zip(&edited_before) {
        sheet.edited = *was;
    }

    // 3. Collect changed tail cells and any anchor whose `#` spill shape or collision status changed
    let mut changed: Vec<deps::CellKey> = before_cells
        .into_iter()
        .filter_map(|(tk, old_val)| (wb.sheets[tk.0].value(tk.1, tk.2) != old_val).then_some(tk))
        .collect();

    for (akey, prev_shape) in old_shapes {
        let now_shape = wb.sheets[akey.0]
            .spill_shapes
            .get(&CellRef::new(akey.1, akey.2))
            .copied();
        if array_anchors.contains(&akey) && now_shape == Some((0, 0)) {
            changed.push(akey);
        } else if deps.has_spill_op_refs() && prev_shape != now_shape {
            changed.push(akey);
        }
    }
    changed.sort_unstable();
    changed.dedup();
    Ok(changed)
}

fn recalc_dirty_impl(
    wb: &mut Workbook,
    deps: &deps::Deps,
    seeds: &[deps::CellKey],
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Option<usize> {
    let cancelled = || cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed));
    if cancelled() { return None; }
    if wb.calc_manual {
        mark_pending(wb, deps, seeds);
        return Some(0);
    }
    if profile_enabled() {
        prof_reset();
    }
    if seeds.is_empty() {
        return Some(0);
    }
    for &(si, r, c) in seeds {
        if let Some(sheet) = wb.sheets.get_mut(si) {
            sheet.spilled.remove(&CellRef::new(r, c));
        }
    }
    let dirty_vec = if deps.requires_full_recalc() || edit_touches_spill(wb, deps, seeds, true) {
        None
    } else {
        Some(deps.edit_closure_vec_for_wb(wb, seeds, cancel)?)
    };
    let Some(mut order) = dirty_vec else {
        let count = wb.sheets.iter().map(|s| s.formula_cells().count()).sum();
        return recalc_all_force_impl(wb, cancel, None).then_some(count);
    };
    // 修改的公式也必须计算，不能只算它的下游。
    for &key in seeds {
        if wb
            .sheets
            .get(key.0)
            .and_then(|s| s.formula_arc_of(key.1, key.2))
            .is_some()
        {
            order.push(key);
        }
    }
    if order.is_empty() {
        wb.mark_recalc_synced();
        return Some(0);
    }
    if !order.is_sorted() {
        order.sort_unstable();
    }
    order.dedup();
    let prev_now = functions::ensure_now_snapshot_in(wb.date_system);
    let eval_ctx = functions::EvalCtx::for_workbook(wb.date_system);

    // Ultra-fast path for width-1 sequential chains (`chain 1M`): skips building both 32 MB
    // `HashSet<CellKey>` AND `Plan::build` CSR structures completely!
    if !wb.calc_iterate
        && !profile_enabled()
        && order.len() >= 256
        && let Some(chain_batch) =
            dependency_batch::try_eval_tight_forward_chain(wb, &order, true, cancel, None)
        && chain_batch.pending.is_empty()
    {
        let total_evaluated = order.len();
        let Ok(changed_tails) = apply_dirty_results_local(wb, deps, chain_batch.values) else {
            functions::set_now_snapshot(prev_now);
            let count = wb.sheets.iter().map(|s| s.formula_cells().count()).sum();
            return recalc_all_force_impl(wb, cancel, None).then_some(count);
        };
        if changed_tails.is_empty() {
            functions::set_now_snapshot(prev_now);
            wb.mark_recalc_synced();
            return Some(total_evaluated);
        }
        // Rare spill-tail follow-up after a chain: continue from `changed_tails`
        let mut dirty = deps.closure_excluding_watches(&order, &order.iter().copied().collect());
        dirty.extend(deps.closure(&changed_tails));
        dirty.extend(deps.dynamic_cells_touching(&changed_tails));
        if dirty.is_empty() {
            functions::set_now_snapshot(prev_now);
            wb.mark_recalc_synced();
            return Some(total_evaluated);
        }
        order = dirty.iter().copied().collect();
        order.sort_unstable();
    }

    let mut dirty: HashSet<deps::CellKey> = order.iter().copied().collect();

    if wb.calc_iterate {
        let passes = wb.calc_max_iterations.max(1) as usize;
        let mut prev_nums: HashMap<deps::CellKey, f64> = HashMap::new();
        let mut last_count = 0usize;
        for pass in 0..passes {
            if cancelled() {
                functions::set_now_snapshot(prev_now);
                return None;
            }
            let results = {
                let mut calc = Calc::new(wb);
                calc.scope = Some(&dirty);
                let mut results = Vec::with_capacity(order.len());
                for &(si, r, c) in &order {
                    if cancelled() {
                        drop(calc);
                        functions::set_now_snapshot(prev_now);
                        return None;
                    }
                    results.push((si, r, c, calc.cell_value(si, r, c)));
                }
                results
            };
            if !write_dirty_results(wb, results) {
                functions::set_now_snapshot(prev_now);
                let count = wb.sheets.iter().map(|s| s.formula_cells().count()).sum();
                return recalc_all_force_impl(wb, cancel, None).then_some(count);
            }
            last_count = order.len();
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
        functions::set_now_snapshot(prev_now);
        return Some(last_count);
    }

    let mut total_evaluated = 0usize;
    let mut volatile_values: FxHashMap<VolatileKey, FValue> = FxHashMap::default();
    for hop in 0..8usize {
        let shared_caches = SharedCalcCaches::default();
        let parallel = if profile_enabled() {
            None
        } else {
            dependency_batch::dirty(wb, &order, &dirty, &shared_caches, eval_ctx, cancel)
        };
        if cancelled() {
            functions::set_now_snapshot(prev_now);
            return None;
        }
        let fast_scope = (dirty.len() >= 16).then(|| FastScope::from_std(&dirty));
        let results = if let Some(mut batch) = parallel {
            if !batch.pending.is_empty() {
                let mut pending_updates = Vec::with_capacity(batch.pending.len());
                {
                    let mut calc = Calc::new(wb);
                    calc.scope = Some(&dirty);
                    calc.fast_scope = fast_scope.as_ref();
                    calc.cancel = cancel;
                    calc.deps_watch = Some(deps);
                    calc.shared = Some(&shared_caches);
                    calc.batch_results = Some(BatchResults {
                        values: &batch.values,
                        pending: &batch.pending,
                    });
                    calc.volatile_values = std::mem::take(&mut volatile_values);
                    for &slot in &batch.pending {
                        if cancelled() || calc.aborted {
                            drop(calc);
                            functions::set_now_snapshot(prev_now);
                            return None;
                        }
                        let (si, r, c) = order[slot];
                        let v = calc.cell_value(si, r, c);
                        if calc.aborted {
                            drop(calc);
                            functions::set_now_snapshot(prev_now);
                            return None;
                        }
                        pending_updates.push((slot, v));
                    }
                    volatile_values = std::mem::take(&mut calc.volatile_values);
                }
                for (slot, v) in pending_updates {
                    batch.values[slot].3 = v;
                }
            }
            batch.values
        } else {
            let mut calc = Calc::new(wb);
            calc.scope = Some(&dirty);
            calc.fast_scope = fast_scope.as_ref();
            calc.cancel = cancel;
            calc.deps_watch = Some(deps);
            calc.shared = Some(&shared_caches);
            calc.volatile_values = std::mem::take(&mut volatile_values);
            let mut results = Vec::with_capacity(order.len());
            for &(si, r, c) in &order {
                if cancelled() || calc.aborted {
                    drop(calc);
                    functions::set_now_snapshot(prev_now);
                    return None;
                }
                let v = calc.cell_value(si, r, c);
                if calc.aborted {
                    drop(calc);
                    functions::set_now_snapshot(prev_now);
                    return None;
                }
                results.push((si, r, c, v));
            }
            volatile_values = std::mem::take(&mut calc.volatile_values);
            results
        };
        total_evaluated += order.len();
        let Ok(changed_tails) = apply_dirty_results_local(wb, deps, results) else {
            functions::set_now_snapshot(prev_now);
            let count = wb.sheets.iter().map(|s| s.formula_cells().count()).sum();
            return recalc_all_force_impl(wb, cancel, None).then_some(count);
        };
        if changed_tails.is_empty() {
            break;
        }
        let mut next_dirty = deps.closure_excluding_watches(&changed_tails, &dirty);
        next_dirty.extend(deps.dynamic_cells_touching(&changed_tails));
        if next_dirty.is_empty() {
            break;
        }
        if hop + 1 == 8 {
            functions::set_now_snapshot(prev_now);
            let count = wb.sheets.iter().map(|s| s.formula_cells().count()).sum();
            return recalc_all_force_impl(wb, cancel, None).then_some(count);
        }
        dirty = next_dirty;
        order = dirty.iter().copied().collect();
        order.sort_unstable();
    }
    if profile_enabled() {
        for (n, c, t) in prof_dump().into_iter().take(6) {
            eprintln!("  [edit] {n:<12} {c:>7} 次  {t:>7.3}s");
        }
    }
    functions::set_now_snapshot(prev_now);
    wb.mark_recalc_synced();
    Some(total_evaluated)
}

fn large_spill(&(rows, cols): &(u32, u32)) -> bool {
    u64::from(rows) * u64::from(cols) > 4096
}

const INTERACTIVE_FRAME_BUDGET: std::time::Duration = std::time::Duration::from_millis(8);

fn eval_formula_seeds_until(
    wb: &mut Workbook,
    seeds: &[deps::CellKey],
    deadline: Option<std::time::Instant>,
    deps_watch: Option<&deps::Deps>,
) -> Result<bool, ()> {
    if wb.calc_manual || seeds.is_empty() {
        return Ok(true);
    }
    // Both the edit broadcaster and the interactive planner use this path.
    // Avoid evaluating a known large anchor once here and then again on the worker.
    if seeds.iter().any(|&(si, r, c)| {
        wb.sheets
            .get(si)
            .and_then(|sh| sh.spill_shapes.get(&CellRef::new(r, c)))
            .is_some_and(large_spill)
    }) {
        return Ok(false);
    }
    let mut formula_seeds: Vec<deps::CellKey> = seeds
        .iter()
        .copied()
        .filter(|&(si, r, c)| {
            wb.sheets
                .get(si)
                .and_then(|s| s.formula_arc_of(r, c))
                .is_some()
        })
        .collect();
    if formula_seeds.is_empty() {
        return Ok(true);
    }
    formula_seeds.sort_unstable();
    formula_seeds.dedup();
    let scope: HashSet<deps::CellKey> = formula_seeds.iter().copied().collect();
    let fast_scope = (scope.len() >= 16).then(|| FastScope::from_std(&scope));
    let mut results = Vec::with_capacity(formula_seeds.len());
    {
        let mut calc = Calc::new(wb);
        calc.scope = Some(&scope);
        calc.fast_scope = fast_scope.as_ref();
        calc.deadline = deadline;
        calc.deps_watch = deps_watch;
        for &(si, r, c) in &formula_seeds {
            if calc.check_abort() {
                return Err(());
            }
            let v = calc.cell_value(si, r, c);
            if calc.aborted {
                return Err(());
            }
            results.push((si, r, c, v));
        }
    }
    Ok(write_dirty_results(wb, results))
}

/// 仅即时计算 `seeds` 中本身带有公式的单元格（供提交编辑时在广播前毫秒级算出当前格结果）。
/// 若遇到需整表物化的动态数组溢出返回 `false`，普通标量公式写回后返回 `true`。
pub fn eval_formula_seeds_immediate(wb: &mut Workbook, seeds: &[deps::CellKey]) -> bool {
    eval_formula_seeds_until(wb, seeds, None, None).unwrap_or(false)
}

/// 带帧预算的即时公式种子求值（供 UI 主线程提交编辑时防止长区间公式阻塞渲染帧）：
/// 超时时不会把残缺中断值写回工作表，而是安全返回 `false` 交给后台线程完成。
pub fn eval_formula_seeds_budgeted(
    wb: &mut Workbook,
    seeds: &[deps::CellKey],
    budget: std::time::Duration,
) -> bool {
    let deadline = std::time::Instant::now() + budget;
    eval_formula_seeds_until(wb, seeds, Some(deadline), None).unwrap_or(false)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InteractiveRecalc {
    Complete,
    BackgroundDirty,
    BackgroundFull,
}

pub fn recalc_interactive(
    wb: &mut Workbook,
    deps: &deps::Deps,
    seeds: &[deps::CellKey],
    deps_ready: bool,
) -> bool {
    recalc_interactive_plan(wb, deps, seeds, deps_ready) == InteractiveRecalc::Complete
}

/// Determine the smallest safe follow-up job for an interactive edit within an 8ms UI frame budget.
pub fn recalc_interactive_plan(
    wb: &mut Workbook,
    deps: &deps::Deps,
    seeds: &[deps::CellKey],
    deps_ready: bool,
) -> InteractiveRecalc {
    use InteractiveRecalc::*;
    if wb.calc_manual {
        mark_pending(wb, deps, seeds);
        return Complete;
    }
    if seeds.is_empty() {
        return Complete;
    }
    let total_cells: usize = wb.sheets.iter().map(|s| s.len()).sum();
    // Blank spill tails consume calculation time without increasing Sheet::len().
    if deps_ready
        && total_cells <= 1_200
        && !wb.sheets.iter().any(|sh| sh.spill_shapes.values().any(large_spill))
    {
        recalc_dirty(wb, deps, seeds);
        return Complete;
    }
    let deadline = std::time::Instant::now() + INTERACTIVE_FRAME_BUDGET;
    let touches_spill = edit_touches_spill(wb, deps, seeds, deps_ready);
    for &(si, r, c) in seeds {
        if let Some(sheet) = wb.sheets.get_mut(si) {
            sheet.spilled.remove(&CellRef::new(r, c));
        }
    }
    let can_bg_dirty =
        deps_ready && !deps.requires_full_recalc() && !touches_spill && !wb.calc_iterate;
    match eval_formula_seeds_until(
        wb,
        seeds,
        Some(deadline),
        deps_ready.then_some(deps),
    ) {
        Ok(true) => {}
        Ok(false) => return BackgroundFull,
        Err(()) => return if can_bg_dirty { BackgroundDirty } else { BackgroundFull },
    }
    if !can_bg_dirty {
        return BackgroundFull;
    }
    let Some(dirty) = deps.edit_closure_limited(seeds, 96) else {
        return BackgroundDirty;
    };
    // Large materialized arrays can take longer than a frame to compare.
    let mut has_tail = false;
    for &(s, r, c) in &dirty {
        if let Some(shape) = wb
            .sheets
            .get(s)
            .and_then(|sh| sh.spill_shapes.get(&CellRef::new(r, c)))
        {
            if large_spill(shape) {
                return BackgroundDirty;
            }
            has_tail |= shape.0 > 1 || shape.1 > 1;
        }
    }
    if dirty.is_empty() {
        return Complete;
    }
    let mut order: Vec<deps::CellKey> = dirty.iter().copied().collect();
    order.sort_unstable();
    let fast_scope = (dirty.len() >= 16).then(|| FastScope::from_std(&dirty));
    let mut results = Vec::with_capacity(order.len());
    let mut timed_out = false;
    {
        let mut calc = Calc::new(wb);
        calc.scope = Some(&dirty);
        calc.fast_scope = fast_scope.as_ref();
        calc.deadline = Some(deadline);
        calc.deps_watch = Some(deps);
        for &(si, r, c) in &order {
            if calc.check_abort() {
                timed_out = true;
                break;
            }
            let v = calc.cell_value(si, r, c);
            if calc.aborted {
                timed_out = true;
                break;
            }
            results.push((si, r, c, v));
        }
    }
    if timed_out && has_tail {
        return BackgroundDirty;
    }
    if write_dirty_results(wb, results.clone()) {
        return if timed_out { BackgroundDirty } else { Complete };
    }
    if timed_out || std::time::Instant::now() >= deadline {
        return BackgroundDirty;
    }
    // Check if any new array in `results` exceeds the interactive spill limit or legacy CSE
    let array_sets: Vec<Option<FxHashSet<CellRef>>> = wb
        .sheets
        .iter()
        .map(|sh| (sh.arrays.len() > 8).then(|| sh.arrays.iter().map(|&(a, _)| a).collect()))
        .collect();
    let mut prospective = Vec::new();
    for (si, r, c, v) in &results {
        let anchor = CellRef::new(*r, *c);
        let is_cse = match &array_sets[*si] {
            Some(set) => set.contains(&anchor),
            None => wb.sheets[*si].arrays.iter().any(|&(a, _)| a == anchor),
        };
        if is_cse {
            return BackgroundFull;
        }
        if let Some(&(oh, ow)) = wb.sheets[*si].spill_shapes.get(&anchor) {
            for dy in 0..oh {
                for dx in 0..ow {
                    prospective.push((*si, *r + dy, *c + dx));
                }
            }
        }
        if let FValue::Array(a) = v {
            if large_spill(&(a.rows as u32, a.cols as u32)) {
                return BackgroundDirty;
            }
            for dy in 0..a.rows as u32 {
                for dx in 0..a.cols as u32 {
                    prospective.push((*si, *r + dy, *c + dx));
                }
            }
        }
    }
    if deps.edit_closure_limited(&prospective, 96).is_none()
        || std::time::Instant::now() >= deadline
    {
        return BackgroundDirty;
    }
    let Ok(changed_tails) = apply_dirty_results_local(wb, deps, results) else {
        return BackgroundFull;
    };
    if !changed_tails.is_empty() {
        let mut next_dirty = deps.closure_excluding_watches(&changed_tails, &dirty);
        next_dirty.extend(deps.dynamic_cells_touching(&changed_tails));
        if !next_dirty.is_empty() {
            if next_dirty.len() > 96 || std::time::Instant::now() >= deadline {
                return BackgroundFull;
            }
            let mut next_order: Vec<deps::CellKey> = next_dirty.iter().copied().collect();
            next_order.sort_unstable();
            let mut follow_results = Vec::with_capacity(next_order.len());
            {
                let mut calc = Calc::new(wb);
                calc.scope = Some(&next_dirty);
                calc.deadline = Some(deadline);
                calc.deps_watch = Some(deps);
                for &(si, r, c) in &next_order {
                    if calc.check_abort() {
                        return BackgroundFull;
                    }
                    let v = calc.cell_value(si, r, c);
                    if calc.aborted {
                        return BackgroundFull;
                    }
                    follow_results.push((si, r, c, v));
                }
            }
            let Ok(more_changed) = apply_dirty_results_local(wb, deps, follow_results) else {
                return BackgroundFull;
            };
            if !more_changed.is_empty() {
                return BackgroundFull;
            }
        }
    }
    wb.mark_recalc_synced();
    Complete
}

fn clear_spills(wb: &mut Workbook, selected: Option<&[bool]>) {
    for (_,sheet) in wb.sheets.iter_mut().enumerate().filter(|(i,_)| selected.is_none_or(|s| s[*i])) {
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

fn spill_values(wb: &Workbook, selected: Option<&[bool]>) -> HashMap<deps::CellKey, crate::Value> {
    wb.sheets.iter().enumerate().filter(|(i,_)| selected.is_none_or(|s| s[*i])).flat_map(|(si, sheet)| {
        sheet.spilled.iter().map(move |key| ((si, key.row, key.col), sheet.value(key.row, key.col)))
    }).collect()
}

/// 全量计算后物化数组，再计算读取溢出格的公式，直到溢出内容稳定。
/// 非收敛的溢出循环显式记为循环引用，避免把尚未稳定的数字当成结果。
pub fn recalc_all(wb: &mut Workbook) {
    if wb.calc_manual {
        wb.calc_pending_sheets = wb.sheets.iter().enumerate()
            .filter(|(_, s)| s.has_formulas())
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
/// 返回 `false` 表示被取消，并标 `calc_pending`。调用方应丢弃取消的工作快照：
/// 多遍计算可能已经物化了前一遍的溢出区或迭代值。
pub fn recalc_all_force_impl(
    wb: &mut Workbook,
    cancel: Option<&std::sync::atomic::AtomicBool>,
    progress: Option<(&std::sync::atomic::AtomicUsize, &std::sync::atomic::AtomicUsize)>,
) -> bool {
    recalc_selected(wb,None,cancel,progress)
}

fn recalc_selected(
    wb: &mut Workbook, selected: Option<&[bool]>,
    cancel: Option<&std::sync::atomic::AtomicBool>,
    progress: Option<(&std::sync::atomic::AtomicUsize, &std::sync::atomic::AtomicUsize)>,
) -> bool {
    use std::sync::atomic::Ordering;
    if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
        wb.calc_pending = true;
        if let Some((done, total)) = progress {
            done.store(0, Ordering::Relaxed);
            total.store(wb.sheets.iter().map(|s| s.formula_cells().count()).sum(), Ordering::Relaxed);
        }
        return false;
    }
    if profile_enabled() { prof_reset(); }
    let prev_now = functions::ensure_now_snapshot_in(wb.date_system);
    let eval_ctx = functions::EvalCtx::for_workbook(wb.date_system);
    // 重算不是用户编辑：记下 edited，写回后再恢复。否则未改过的表
    // （尤其是动态数组表）会被当成 dirty，保存时整表重写，_xlfn 前缀
    // 和溢出缓存都会丢，Excel 打不开/结果不对。
    let edited_before: Vec<bool> = wb.sheets.iter().map(|s| s.edited).collect();
    let mark_cancelled = |wb: &mut Workbook| {
        wb.calc_pending = true;
        for (sheet, was) in wb.sheets.iter_mut().zip(&edited_before) { sheet.edited = *was; }
        functions::set_now_snapshot(prev_now);
        false
    };
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
    for (_,sheet) in wb.sheets.iter_mut().enumerate().filter(|(i,_)| selected.is_none_or(|s| s[*i])) {
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
    clear_spills(wb,selected);

    mark!("清溢出");
    // Recalculation changes values and spill tails, not formula text. Preserve row-major
    // order for intersecting spills while skipping pages that cannot contain formulas.
    let mut formulas: Vec<_> = wb.sheets.iter().enumerate().filter(|(i,_)| selected.is_none_or(|s| s[*i])).flat_map(|(si, sh)|
        sh.formula_cells().map(move |(k, _)| (si, k.row, k.col))).collect();
    formulas.sort_unstable();
    let scope: Option<HashSet<_>> = selected.map(|_| formulas.iter().copied().collect());
    let formula_count = formulas.len();
    // 无环的溢出依赖链每遍至少推进一个公式；再多一遍确认稳定。
    // 迭代计算：环上按上一轮值反复推进，最多 calc_max_iterations 遍（Excel 默认 100）
    let passes = if wb.calc_iterate {
        (wb.calc_max_iterations.max(2)) as usize
    } else {
        formula_count.saturating_add(1).max(2)
    };
    let force_two = std::env::var("SC_TWOPASS").is_ok();
    let mut volatile_values: FxHashMap<VolatileKey, FValue> = FxHashMap::default();
    let mut previous_arrays: HashMap<deps::CellKey, FValue> = HashMap::new();
    let mut cached_graph: Option<deps::Deps> = None;
    let mut pass_done = 0usize;
    // 迭代模式：记录环上格子每遍的数值，用来判收敛
    let mut prev_nums: HashMap<deps::CellKey, f64> = HashMap::new();
    for _pass in 0..passes {
        pass_done += 1;
        let before_spills = spill_values(wb,selected);
        mark!("收集公式");
        if let Some((done, total)) = progress {
            total.store(formulas.len(), Ordering::Relaxed);
            done.store(0, Ordering::Relaxed);
        }
        let shared_caches = SharedCalcCaches::default();
        let parallel = if !wb.calc_iterate && !profile_enabled() {
            let done = progress.map(|p| p.0);
            let independent = scalar_batch::evaluate_in(
                wb,
                &formulas,
                scope.as_ref(),
                Some(&shared_caches),
                eval_ctx,
                cancel,
                done,
            );
            dependency_batch::evaluate(
                wb,
                &formulas,
                independent,
                scope.as_ref(),
                &shared_caches,
                eval_ctx,
                cancel,
                done,
            )
        } else { None };
        if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            return mark_cancelled(wb);
        }
        let (mut results, pending) = match parallel {
            Some(batch) => (batch.values, Some(batch.pending)),
            None => (Vec::with_capacity(formulas.len()), None),
        };
        let remaining = pending.as_ref().map_or(formulas.len(), Vec::len);
        if remaining > 0 {
            let mut pending_updates: Vec<(usize, FValue)> = if pending.is_some() {
                Vec::with_capacity(remaining)
            } else {
                Vec::new()
            };
            let mut seq_results: Vec<(usize, u32, u32, FValue)> = if pending.is_none() {
                Vec::with_capacity(formulas.len())
            } else {
                Vec::new()
            };
            {
                let mut calc = Calc::new(wb);
                calc.scope = scope.as_ref();
                calc.cancel = cancel;
                calc.shared = Some(&shared_caches);
                if let Some(ref pend_vec) = pending {
                    // Sequential formulas may refer to any parallel batch result, even across sheets.
                    // Borrow the sorted parallel batch slice directly via O(log N) lookup instead of
                    // cloning up to 1M scalar values into `Calc::memo` FxHashMap!
                    calc.batch_results = Some(BatchResults {
                        values: &results,
                        pending: pend_vec,
                    });
                }
                calc.volatile_values = std::mem::take(&mut volatile_values);
                let profile = profile_enabled();
                let mut slow: Vec<(f64, usize, u32, u32, String)> = Vec::new();
                for i in 0..remaining {
                    let slot = pending.as_ref().map_or(i, |indices| indices[i]);
                    let (si, r, c) = formulas[slot];
                    if let Some(flag) = cancel {
                        // 每 256 个公式查一次，取消时立刻放弃本轮（尚未写回）
                        if i % 256 == 0 && flag.load(Ordering::Relaxed) {
                            drop(calc);
                            return mark_cancelled(wb);
                        }
                    }
                    if let Some((done, _)) = progress {
                        done.store(formulas.len() - remaining + i, Ordering::Relaxed);
                    }
                    let t0 = profile.then(std::time::Instant::now);
                    let v = calc.cell_value(si, r, c);
                    if calc.aborted {
                        drop(calc);
                        return mark_cancelled(wb);
                    }
                    if let Some(start) = t0 {
                        slow.push((
                            start.elapsed().as_secs_f64(),
                            si,
                            r,
                            c,
                            wb.sheets[si]
                                .get(r, c)
                                .and_then(|x| x.formula.as_deref().map(|f| f.to_string()))
                                .unwrap_or_default(),
                        ));
                    }
                    if pending.is_some() {
                        pending_updates.push((slot, v));
                    } else {
                        seq_results.push((si, r, c, v));
                    }
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
                        eprintln!(
                            "  {:>7.3}s  {}!R{}C{}  {}",
                            d,
                            si,
                            r + 1,
                            c + 1,
                            f.chars().take(90).collect::<String>().as_str()
                        );
                    }
                }
                volatile_values = std::mem::take(&mut calc.volatile_values);
            }
            if pending.is_some() {
                for (slot, v) in pending_updates {
                    results[slot].3 = v;
                }
            } else {
                results = seq_results;
            }
        }

        mark!("求值");
        let arrays: HashMap<deps::CellKey, FValue> = results.iter()
            .filter(|(_, _, _, v)| matches!(v, FValue::Array(_)))
            .map(|(si, r, c, v)| ((*si, *r, *c), v.clone())).collect();
        let final_results = (_pass + 1 == passes).then(|| results.clone());
        // A cancellation can arrive while worker results are collected into source order.
        if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
            return mark_cancelled(wb);
        }
        clear_spills(wb,selected);
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
        let after_spills = spill_values(wb,selected);
        // 普通模式：溢出块稳定即停。迭代模式不能在这里停——环上格子没有溢出，
        // 一停就只跑一遍，永远到不了不动点。
        if !wb.calc_iterate && before_spills == after_spills && (!force_two || _pass > 0) {
            break;
        }
        // Pass-0 溢出单遍局部收敛优化：若工作簿存在动态数组溢出但没有任何公式（或仅有极少数纯标量公式）
        // 引用了新物化的溢出尾格，直接跳过全簿 Pass 1 重算，避免 10 万条无关公式翻倍执行！
        if !wb.calc_iterate && !force_two && _pass == 0 && final_results.is_none() {
            let mut changed_after_wb: Vec<deps::CellKey> = before_spills
                .keys()
                .chain(after_spills.keys())
                .filter(|k| before_spills.get(*k) != after_spills.get(*k))
                .copied()
                .collect();
            for &akey in arrays.keys() {
                if wb.sheets[akey.0]
                    .spill_shapes
                    .get(&CellRef::new(akey.1, akey.2))
                    == Some(&(0, 0))
                {
                    changed_after_wb.push(akey);
                }
            }
            changed_after_wb.sort_unstable();
            changed_after_wb.dedup();
            if cached_graph.is_none()
                && !deps::any_formula_depends_on_sorted(wb, selected, &formulas, &changed_after_wb)
            {
                break;
            }
            let graph = cached_graph.get_or_insert_with(|| deps::Deps::build_filtered(wb, selected));
            let mut affected = graph.closure(&changed_after_wb);
            affected.extend(graph.dynamic_cells());
            if let Some(ref sc) = scope {
                affected.retain(|k| sc.contains(k));
            }
            if affected.is_empty() {
                break;
            }
            let touches_spill_anchor = affected.iter().any(|&(si, r, c)| {
                wb.sheets[si]
                    .spill_shapes
                    .contains_key(&CellRef::new(r, c))
            });
            if !touches_spill_anchor && affected.len() * 2 < formulas.len() {
                let mut sub_order: Vec<deps::CellKey> = affected.iter().copied().collect();
                sub_order.sort_unstable();
                let sub_shared = SharedCalcCaches::default();
                let sub_parallel = if profile_enabled() {
                    None
                } else {
                    dependency_batch::dirty(wb, &sub_order, &affected, &sub_shared, eval_ctx, cancel)
                };
                if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
                    return mark_cancelled(wb);
                }
                let mut produced_array = false;
                let sub_results = if let Some(mut batch) = sub_parallel {
                    if !batch.pending.is_empty() {
                        let mut pending_updates = Vec::with_capacity(batch.pending.len());
                        {
                            let mut calc = Calc::new(wb);
                            calc.scope = Some(&affected);
                            calc.cancel = cancel;
                            calc.shared = Some(&sub_shared);
                            calc.batch_results = Some(BatchResults {
                                values: &batch.values,
                                pending: &batch.pending,
                            });
                            calc.volatile_values = std::mem::take(&mut volatile_values);
                            for &slot in &batch.pending {
                                if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
                                    drop(calc);
                                    return mark_cancelled(wb);
                                }
                                let (si, r, c) = sub_order[slot];
                                let v = calc.cell_value(si, r, c);
                    if calc.aborted {
                        drop(calc);
                        return mark_cancelled(wb);
                    }
                                produced_array |= matches!(v, FValue::Array(_));
                                pending_updates.push((slot, v));
                            }
                            volatile_values = std::mem::take(&mut calc.volatile_values);
                        }
                        for (slot, v) in pending_updates {
                            batch.values[slot].3 = v;
                        }
                    }
                    batch.values
                } else {
                    let mut calc = Calc::new(wb);
                    calc.scope = Some(&affected);
                    calc.cancel = cancel;
                    calc.shared = Some(&sub_shared);
                    calc.volatile_values = std::mem::take(&mut volatile_values);
                    let mut sub_results = Vec::with_capacity(sub_order.len());
                    for &(si, r, c) in &sub_order {
                        if cancel.is_some_and(|flag| flag.load(Ordering::Relaxed)) {
                            drop(calc);
                            return mark_cancelled(wb);
                        }
                        let v = calc.cell_value(si, r, c);
                    if calc.aborted {
                        drop(calc);
                        return mark_cancelled(wb);
                    }
                        produced_array |= matches!(v, FValue::Array(_));
                        sub_results.push((si, r, c, v));
                    }
                    volatile_values = std::mem::take(&mut calc.volatile_values);
                    sub_results
                };
                if !produced_array {
                    write_back(wb, sub_results);
                    break;
                }
            }
        }
        if let Some(mut final_results) = final_results {
            let mut unstable: HashSet<deps::CellKey> = before_spills.keys().chain(after_spills.keys())
                .filter(|key| before_spills.get(key) != after_spills.get(key)).copied().collect();
            let changing_arrays: HashSet<deps::CellKey> = previous_arrays.keys().chain(arrays.keys())
                .filter(|key| previous_arrays.get(key) != arrays.get(key)).copied().collect();
            unstable.extend(changing_arrays.iter().copied());
            let graph = match cached_graph.as_mut() {
                Some(g) => {
                    g.refresh_spills(wb, changing_arrays.iter().copied());
                    g
                }
                None => cached_graph.insert(deps::Deps::build_filtered(wb, selected)),
            };
            // 无法静态解析的动态引用保守视为受影响，普通独立公式保留结果。
            unstable.extend(graph.dynamic_cells());
            let mut affected = graph.closure(&unstable.iter().copied().collect::<Vec<_>>());
            affected.extend(changing_arrays);
            affected.extend(graph.dynamic_cells());
            clear_spills(wb,selected);
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
    let _ = t_phase;
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
    if selected.is_none() {
        wb.mark_recalc_synced();
    }
    true
}

/// 求单个单元格的值（测试与 UI 用）
pub fn eval_cell(wb: &Workbook, si: usize, r: u32, c: u32) -> FValue {
    let mut calc = Calc::new(wb);
    calc.cell_value(si, r, c)
}

/// 直接求一段公式文本（测试用）
pub fn eval_formula(wb: &Workbook, si: usize, formula: &str) -> FValue {
    let raw = formula.strip_prefix('=').unwrap_or(formula);
    let expanded = crate::table::expand_structured(wb, si, raw);
    match compile_at(expanded.as_deref().unwrap_or(raw), 0, 0) {
        Ok(node) => {
            let mut calc = Calc::new(wb);
            calc.eval_node(&node, si)
        }
        Err(e) => FValue::Error(e),
    }
}

/// 与 `eval_formula` 相同，但返回区域物化实际读取的格数，供性能回归使用。
pub fn eval_formula_reads(wb: &Workbook, si: usize, formula: &str) -> (FValue, usize) {
    let raw = formula.strip_prefix('=').unwrap_or(formula);
    let expanded = crate::table::expand_structured(wb, si, raw);
    match compile_at(expanded.as_deref().unwrap_or(raw), 0, 0) {
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
/// 走 `formula_cells()` 稀疏页位图 + 多核 `recalc_selected`，不把其它表的无关易失函数拉进来。
pub fn recalc_sheet(wb: &mut Workbook, sheet: usize) -> usize {
    recalc_sheet_with_progress(wb, sheet, None, None).unwrap_or(0)
}

/// 可取消、带实时进度的单表重算（消除前置 `formula_cells().count()` 重复扫描）。
pub fn recalc_sheet_with_progress(
    wb: &mut Workbook,
    sheet: usize,
    cancel: Option<&std::sync::atomic::AtomicBool>,
    progress: Option<(&std::sync::atomic::AtomicUsize, &std::sync::atomic::AtomicUsize)>,
) -> Option<usize> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    if sheet >= wb.sheets.len() {
        return Some(0);
    }
    let local_done = AtomicUsize::new(0);
    let local_total = AtomicUsize::new(0);
    let eff_progress = Some(progress.unwrap_or((&local_done, &local_total)));
    let mut pending_sheets = std::mem::take(&mut wb.calc_pending_sheets);
    let mut mask = vec![false; wb.sheets.len()];
    mask[sheet] = true;
    let completed = recalc_selected(wb, Some(&mask), cancel, eff_progress);
    if completed {
        pending_sheets.remove(&sheet);
    } else {
        pending_sheets.insert(sheet);
    }
    wb.calc_pending_sheets = pending_sheets;
    wb.calc_pending = !wb.calc_pending_sheets.is_empty();
    completed.then(|| eff_progress.unwrap().1.load(Ordering::Relaxed))
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
