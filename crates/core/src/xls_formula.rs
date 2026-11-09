//! 把公式 AST 编译成 BIFF8 的 RPN 记号流（`rgce`）。
//!
//! 函数号不是猜的：拿 Excel 生成的样本逐条读出来的（见 `docs/native-rewrite.md`）。
//! 记号带"类"位：数值类 0x40、引用类 0x20，Excel 在普通公式里写的是数值类
//! （`ptgRef` 0x44、`ptgFunc` 0x41、`ptgFuncVar` 0x42）。

use crate::formula::{BinOp, Node, RefExpr};
use anyhow::{bail, Result};
use std::collections::HashMap;

/// 函数名 → BIFF8 iftab（Excel 97 起的函数号）
fn func_index(name: &str) -> Option<u16> {
    Some(match name {
        "COUNT" => 0,
        "IF" => 1,
        "ISNA" => 2,
        "ISERROR" => 3,
        "SUM" => 4,
        "AVERAGE" => 5,
        "MIN" => 6,
        "MAX" => 7,
        "ROW" => 8,
        "COLUMN" => 9,
        "NA" => 10,
        "SIN" => 15,
        "COS" => 16,
        "TAN" => 17,
        "ATAN" => 18,
        "PI" => 19,
        "SQRT" => 20,
        "EXP" => 21,
        "LN" => 22,
        "LOG10" => 23,
        "ABS" => 24,
        "INT" => 25,
        "SIGN" => 26,
        "ROUND" => 27,
        "LOOKUP" => 28,
        "INDEX" => 29,
        "REPT" => 30,
        "MID" => 31,
        "LEN" => 32,
        "VALUE" => 33,
        "TRUE" => 34,
        "FALSE" => 35,
        "AND" => 36,
        "OR" => 37,
        "NOT" => 38,
        "MOD" => 39,
        "TEXT" => 48,
        "MATCH" => 64,
        "DATE" => 65,
        "DAY" => 67,
        "MONTH" => 68,
        "YEAR" => 69,
        "WEEKDAY" => 70,
        "HOUR" => 71,
        "MINUTE" => 72,
        "SECOND" => 73,
        "NOW" => 74,
        "HLOOKUP" => 101,
        "VLOOKUP" => 102,
        "LOG" => 109,
        "LOWER" => 112,
        "UPPER" => 113,
        "LEFT" => 115,
        "RIGHT" => 116,
        "TRIM" => 118,
        "ISTEXT" => 127,
        "ISNUMBER" => 128,
        "ISBLANK" => 129,
        "N" => 131,
        "COUNTA" => 169,
        "TRUNC" => 197,
        "ROUNDUP" => 212,
        "ROUNDDOWN" => 213,
        "TODAY" => 221,
        "SUMPRODUCT" => 228,
        "IFERROR" => 255,
        "CONCATENATE" => 336,
        "POWER" => 337,
        "SUMIF" => 345,
        "COUNTIF" => 346,
        "MAXA" => 362,
        "MINA" => 363,
        _ => return None,
    })
}

/// 同一工作簿里的跨表引用要登记到 EXTERNSHEET，这里维护它的条目表。
#[derive(Default)]
pub struct Externs {
    /// (supbook 序号, 起表, 止表) → 去重后的 ixti
    pub entries: Vec<(u16, u16, u16)>,
    map: HashMap<(u16, u16, u16), u16>,
}

impl Externs {
    pub fn add(&mut self, supbook: u16, first: u16, last: u16) -> u16 {
        let key = (supbook, first, last);
        if let Some(i) = self.map.get(&key) {
            return *i;
        }
        let i = self.entries.len() as u16;
        self.entries.push(key);
        self.map.insert(key, i);
        i
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

pub struct Compiler<'a> {
    pub sheets: &'a [String],
    pub cur_sheet: usize,
    pub externs: &'a mut Externs,
}

impl Compiler<'_> {
    pub fn compile(&mut self, node: &Node) -> Result<Vec<u8>> {
        let mut out = Vec::new();
        self.emit(node, &mut out)?;
        Ok(out)
    }

    fn emit(&mut self, node: &Node, out: &mut Vec<u8>) -> Result<()> {
        match node {
            Node::Number(n) => {
                // 小整数用 ptgInt（1..=65535）更省字节，也是 Excel 的习惯
                if *n >= 0.0 && *n <= 65535.0 && n.fract() == 0.0 {
                    out.push(0x1E);
                    out.extend_from_slice(&(*n as u16).to_le_bytes());
                } else {
                    out.push(0x1F);
                    out.extend_from_slice(&n.to_le_bytes());
                }
            }
            Node::Text(t) => {
                let units: Vec<u16> = t.encode_utf16().collect();
                if units.len() > 255 {
                    bail!("字符串常量太长（>255 字符），.xls 写不了");
                }
                out.push(0x17);
                out.push(units.len() as u8);
                out.push(1); // UTF-16
                for u in units {
                    out.extend_from_slice(&u.to_le_bytes());
                }
            }
            Node::Bool(b) => {
                out.push(0x1D);
                out.push(if *b { 1 } else { 0 });
            }
            Node::Err(e) => {
                out.push(0x1C);
                out.push(err_code(*e));
            }
            Node::Empty => out.push(0x16), // ptgMissArg
            Node::Ref(r) => self.emit_ref(r, out)?,
            Node::Range(a, b) => self.emit_area(a, b, out)?,
            Node::Unary(neg, inner) => {
                self.emit(inner, out)?;
                out.push(if *neg { 0x13 } else { 0x12 });
            }
            Node::Percent(inner) => {
                self.emit(inner, out)?;
                out.push(0x14);
            }
            Node::Binary(op, a, b) => {
                self.emit(a, out)?;
                self.emit(b, out)?;
                out.push(binop_code(*op));
            }
            Node::Func(name, args) => {
                let upper = name.to_ascii_uppercase();
                let Some(idx) = func_index(&upper) else {
                    bail!("函数 {name} 暂不支持写入 .xls");
                };
                let mut argc = 0usize;
                for a in args {
                    self.emit(a, out)?;
                    argc += 1;
                }
                // 固定参数个数的函数用 ptgFunc，可变参数用 ptgFuncVar
                let fixed = matches!(
                    upper.as_str(),
                    "ABS" | "INT" | "SIGN" | "SQRT" | "EXP" | "LN" | "LOG10" | "SIN" | "COS"
                        | "TAN" | "ATAN" | "NOT" | "LEN" | "TRIM" | "UPPER" | "LOWER" | "VALUE"
                        | "TEXT" | "DATE" | "YEAR" | "MONTH" | "DAY" | "HOUR" | "MINUTE"
                        | "SECOND" | "WEEKDAY" | "ROUND" | "ROUNDUP" | "ROUNDDOWN" | "TRUNC"
                        | "REPT" | "LEFT" | "RIGHT" | "MID" | "MOD" | "N" | "ISBLANK"
                        | "ISNUMBER" | "ISTEXT" | "ISERROR" | "ISNA" | "PI" | "NOW" | "TODAY"
                        | "TRUE" | "FALSE" | "NA"
                );
                if fixed || upper == "PI" || upper == "NOW" || upper == "TODAY" || upper == "TRUE" || upper == "FALSE" {
                    out.push(0x41);
                    out.extend_from_slice(&idx.to_le_bytes());
                } else {
                    out.push(0x42);
                    out.push(argc as u8);
                    out.extend_from_slice(&idx.to_le_bytes());
                }
            }
            Node::Name(n) => bail!("名称 {n} 暂不支持写入 .xls"),
            Node::Array(..) => bail!("数组常量暂不支持写入 .xls"),
        }
        Ok(())
    }

    /// 单个引用（会写 ptgRef / ptgRef3d 的完整内容）
    fn emit_ref(&mut self, r: &RefExpr, out: &mut Vec<u8>) -> Result<()> {
        let (row, col) = self.check_coords(r)?;
        if let Some(book) = &r.book {
            bail!("跨工作簿引用（[{book}]）暂不支持写入 .xls");
        }
        match &r.sheet {
            None => {
                out.push(0x44);
                out.extend_from_slice(&row.to_le_bytes());
                out.extend_from_slice(&self.col_word(col, r).to_le_bytes());
            }
            Some(sh) => {
                let ixti = self.ixti(sh)?;
                out.push(0x5A);
                out.extend_from_slice(&ixti.to_le_bytes());
                out.extend_from_slice(&row.to_le_bytes());
                out.extend_from_slice(&self.col_word(col, r).to_le_bytes());
            }
        }
        Ok(())
    }

    fn emit_area(&mut self, a: &RefExpr, b: &RefExpr, out: &mut Vec<u8>) -> Result<()> {
        let (r1, c1) = self.check_coords(a)?;
        let (r2, c2) = self.check_coords(b)?;
        if let Some(book) = a.book.as_ref().or(b.book.as_ref()) {
            bail!("跨工作簿引用（[{book}]）暂不支持写入 .xls");
        }
        match a.sheet.as_ref().or(b.sheet.as_ref()) {
            None => {
                out.push(0x45);
                out.extend_from_slice(&r1.to_le_bytes());
                out.extend_from_slice(&r2.to_le_bytes());
                out.extend_from_slice(&self.col_word(c1, a).to_le_bytes());
                out.extend_from_slice(&self.col_word(c2, b).to_le_bytes());
            }
            Some(sh) => {
                let ixti = self.ixti(sh)?;
                out.push(0x5B);
                out.extend_from_slice(&ixti.to_le_bytes());
                out.extend_from_slice(&r1.to_le_bytes());
                out.extend_from_slice(&r2.to_le_bytes());
                out.extend_from_slice(&self.col_word(c1, a).to_le_bytes());
                out.extend_from_slice(&self.col_word(c2, b).to_le_bytes());
            }
        }
        Ok(())
    }

    fn check_coords(&self, r: &RefExpr) -> Result<(u16, u16)> {
        if r.row < 0 || r.row > 65535 {
            bail!(".xls 最多 65536 行，公式引用了第 {} 行", r.row + 1);
        }
        if r.col < 0 || r.col > 255 {
            bail!(".xls 最多 256 列，公式引用了第 {} 列", r.col + 1);
        }
        Ok((r.row as u16, r.col as u16))
    }

    fn col_word(&self, col: u16, r: &RefExpr) -> u16 {
        let mut w = col & 0x3FFF;
        if !r.col_abs {
            w |= 0x4000;
        }
        if !r.row_abs {
            w |= 0x8000;
        }
        w
    }

    fn ixti(&mut self, sheet: &str) -> Result<u16> {
        let idx = self
            .sheets
            .iter()
            .position(|s| s == sheet)
            .or_else(|| {
                // 名字大小写不同也认
                self.sheets.iter().position(|s| s.eq_ignore_ascii_case(sheet))
            });
        let Some(i) = idx else {
            bail!("引用了不存在的工作表 {sheet}");
        };
        Ok(self.externs.add(0, i as u16, i as u16))
    }
}

fn binop_code(op: BinOp) -> u8 {
    match op {
        BinOp::Add => 0x03,
        BinOp::Sub => 0x04,
        BinOp::Mul => 0x05,
        BinOp::Div => 0x06,
        BinOp::Pow => 0x07,
        BinOp::Concat => 0x08,
        BinOp::Lt => 0x09,
        BinOp::Le => 0x0A,
        BinOp::Eq => 0x0B,
        BinOp::Ge => 0x0C,
        BinOp::Gt => 0x0D,
        BinOp::Ne => 0x0E,
    }
}

fn err_code(e: crate::formula::ErrKind) -> u8 {
    use crate::formula::ErrKind::*;
    match e {
        Null => 0,
        Div0 => 7,
        Value => 15,
        Ref => 23,
        Name => 29,
        Num => 36,
        Na => 42,
        Circular => 15,
        Spill => 15,
    }
}

/// 表名 → 公式文本里的引用写法（带引号）
pub fn quote_sheet(name: &str) -> String {
    let need = name.chars().any(|c| {
        c.is_whitespace() || matches!(c, '\'' | '!' | '[' | ']' | ':' | '\\' | '/' | '?' | '*')
            || c.is_ascii_digit()
    });
    if need {
        format!("'{}'", name.replace('\'', "''"))
    } else {
        name.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formula::parse;

    fn comp<'a>(sheets: &'a [String], ext: &'a mut Externs) -> Compiler<'a> {
        Compiler {
            sheets,
            cur_sheet: 0,
            externs: ext,
        }
    }

    #[test]
    fn arithmetic_becomes_rpn() {
        let sheets = vec!["S".to_string()];
        let mut ext = Externs::default();
        let node = parse("1+2*3").unwrap();
        let t = comp(&sheets, &mut ext).compile(&node).unwrap();
        // 1 2 3 * + ：乘法先压栈
        assert_eq!(t, vec![0x1E, 1, 0, 0x1E, 2, 0, 0x1E, 3, 0, 0x05, 0x03]);
    }

    #[test]
    fn refs_carry_relative_flags() {
        let sheets = vec!["S".to_string()];
        let mut ext = Externs::default();
        let node = parse("A1").unwrap();
        let t = comp(&sheets, &mut ext).compile(&node).unwrap();
        assert_eq!(t[0], 0x44, "ptgRef 的数值类");
        let col = u16::from_le_bytes([t[3], t[4]]);
        assert_eq!(col & 0x3FFF, 0, "列号");
        assert_eq!(col & 0xC000, 0xC000, "行列都是相对引用");
    }

    #[test]
    fn absolute_ref_clears_flags() {
        let sheets = vec!["S".to_string()];
        let mut ext = Externs::default();
        let node = parse("$B$2").unwrap();
        let t = comp(&sheets, &mut ext).compile(&node).unwrap();
        let col = u16::from_le_bytes([t[3], t[4]]);
        assert_eq!(col & 0x3FFF, 1);
        assert_eq!(col & 0xC000, 0, "$ 引用不带相对标志");
    }

    #[test]
    fn cross_sheet_ref_registers_extern() {
        let sheets = vec!["S".to_string(), "T".to_string()];
        let mut ext = Externs::default();
        let node = parse("T!A1").unwrap();
        let t = comp(&sheets, &mut ext).compile(&node).unwrap();
        assert_eq!(t[0], 0x5A, "ptgRef3d");
        assert_eq!(u16::from_le_bytes([t[1], t[2]]), 0, "ixti 从 0 开始");
        assert_eq!(ext.entries.len(), 1);
        assert_eq!(ext.entries[0], (0, 1, 1), "supbook 0 / 第 2 张表");
    }

    #[test]
    fn function_uses_verified_index() {
        let sheets = vec!["S".to_string()];
        let mut ext = Externs::default();
        let node = parse("SUM(A1:A2)").unwrap();
        let t = comp(&sheets, &mut ext).compile(&node).unwrap();
        // 末尾是 ptgFuncVar(0x42) argc=1 iftab=4(SUM)
        assert_eq!(&t[t.len() - 4..], &[0x42, 1, 4, 0]);
    }

    #[test]
    fn unknown_function_is_reported() {
        let sheets = vec!["S".to_string()];
        let mut ext = Externs::default();
        let node = parse("LAMBDA(x,x)").unwrap();
        let e = comp(&sheets, &mut ext).compile(&node).unwrap_err();
        assert!(format!("{e}").contains("暂不支持"), "错误要说清楚：{e}");
    }
}
