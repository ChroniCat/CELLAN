//! 把公式 AST 编译成 BIFF8 的 RPN 记号流（`rgce`）。
//!
//! 函数号是从 Excel 自己写出的样本里逐条读出来的（`work/make_probe2.ps1` →
//! `examples/fn_map.rs`），不是照记忆抄的。表里第二列是 iftab，第三列是「固定参数个数」
//! （0 表示可变参数，用 ptgFuncVar；非 0 且实参个数正好吻合时用 ptgFunc）。
//!
//! 三个 BIFF8 特有机制：
//! - 数组常量 `{1,2,3}`：ptgArray 只存行列数，值放在 FORMULA 记录尾部的 rgcb 里；
//! - 定义名称：ptgName 的索引是「第几条 NAME 记录」，1 起；
//! - 2007 之后才有的函数：先 ptgName 把 `_xlfn.名字` 压栈，再 ptgFuncVar(iftab=0xFF)。

use crate::Value;
use crate::formula::{BinOp, Node, RefExpr};
use anyhow::{bail, Result};
use std::collections::HashMap;

/// 名字 → (iftab, 固定参数个数)
fn func_spec(name: &str) -> Option<(u16, u8)> {
    const TABLE: &[(&str, u16, u8)] = &[
    ("ABS", 24, 1),
    ("ACOS", 99, 1),
    ("ACOSH", 233, 1),
    ("ADDRESS", 219, 0),
    ("AND", 36, 0),
    ("ASIN", 98, 1),
    ("ASINH", 232, 1),
    ("ATAN", 18, 1),
    ("ATAN2", 97, 2),
    ("ATANH", 234, 1),
    ("AVERAGE", 5, 0),
    ("AVERAGEA", 361, 0),
    ("CEILING", 288, 2),
    ("CELL", 125, 0),
    ("CHAR", 111, 1),
    ("CHOOSE", 100, 0),
    ("CLEAN", 162, 1),
    ("CODE", 121, 1),
    ("COLUMN", 9, 0),
    ("COLUMNS", 77, 1),
    ("COMBIN", 276, 2),
    ("CONCATENATE", 336, 0),
    ("CORREL", 307, 2),
    ("COS", 16, 1),
    ("COSH", 230, 1),
    ("COUNT", 0, 0),
    ("COUNTA", 169, 0),
    ("COUNTBLANK", 347, 1),
    ("COUNTIF", 346, 2),
    ("COVAR", 308, 2),
    ("DATE", 65, 3),
    ("DATEVALUE", 140, 1),
    ("DAY", 67, 1),
    ("DAYS360", 220, 0),
    ("DB", 247, 0),
    ("DCOUNT", 40, 3),
    ("DDB", 144, 0),
    ("DEGREES", 343, 1),
    ("DEVSQ", 318, 0),
    ("DGET", 235, 3),
    ("DMAX", 44, 3),
    ("DMIN", 43, 3),
    ("DOLLAR", 13, 0),
    ("DPRODUCT", 189, 3),
    ("DSTDEV", 45, 3),
    ("DSUM", 41, 3),
    ("DVAR", 47, 3),
    ("ERROR.TYPE", 261, 1),
    ("EVEN", 279, 1),
    ("EXACT", 117, 2),
    ("EXP", 21, 1),
    ("EXPONDIST", 280, 3),
    ("FACT", 184, 1),
    ("FALSE", 35, 0),
    ("FIND", 124, 0),
    ("FISHER", 283, 1),
    ("FLOOR", 285, 2),
    ("FORECAST", 309, 3),
    ("FV", 57, 0),
    ("GEOMEAN", 319, 0),
    ("GROWTH", 52, 0),
    ("HARMEAN", 320, 0),
    ("HLOOKUP", 101, 0),
    ("HOUR", 71, 1),
    ("HYPERLINK", 359, 0),
    ("IF", 1, 0),
    ("INDEX", 29, 0),
    ("INDIRECT", 148, 0),
    ("INFO", 244, 1),
    ("INT", 25, 1),
    ("INTERCEPT", 311, 2),
    ("IPMT", 167, 0),
    ("IRR", 62, 0),
    ("ISBLANK", 129, 1),
    ("ISERR", 126, 1),
    ("ISERROR", 3, 1),
    ("ISLOGICAL", 198, 1),
    ("ISNA", 2, 1),
    ("ISNONTEXT", 190, 1),
    ("ISNUMBER", 128, 1),
    ("ISREF", 105, 1),
    ("ISTEXT", 127, 1),
    ("LARGE", 325, 2),
    ("LEFT", 115, 0),
    ("LEN", 32, 1),
    ("LINEST", 49, 0),
    ("LN", 22, 1),
    ("LOG", 109, 0),
    ("LOG10", 23, 1),
    ("LOOKUP", 28, 0),
    ("LOWER", 112, 1),
    ("MATCH", 64, 0),
    ("MAX", 7, 0),
    ("MAXA", 362, 0),
    ("MEDIAN", 227, 0),
    ("MID", 31, 3),
    ("MIN", 6, 0),
    ("MINA", 363, 0),
    ("MINUTE", 72, 1),
    ("MIRR", 61, 3),
    ("MOD", 39, 2),
    ("MODE", 330, 0),
    ("MONTH", 68, 1),
    ("N", 131, 1),
    ("NA", 10, 0),
    ("NOT", 38, 1),
    ("NOW", 74, 0),
    ("NPER", 58, 0),
    ("NPV", 11, 0),
    ("ODD", 298, 1),
    ("OFFSET", 78, 0),
    ("OR", 37, 0),
    ("PEARSON", 312, 2),
    ("PERCENTILE", 328, 2),
    ("PERCENTRANK", 329, 0),
    ("PI", 19, 0),
    ("PMT", 59, 0),
    ("POWER", 337, 2),
    ("PPMT", 168, 0),
    ("PRODUCT", 183, 0),
    ("PROPER", 114, 1),
    ("PV", 56, 0),
    ("QUARTILE", 327, 2),
    ("RADIANS", 342, 1),
    ("RAND", 63, 0),
    ("RANK", 216, 0),
    ("RATE", 60, 0),
    ("REPLACE", 119, 4),
    ("REPT", 30, 2),
    ("RIGHT", 116, 0),
    ("ROMAN", 354, 0),
    ("ROUND", 27, 2),
    ("ROUNDDOWN", 213, 2),
    ("ROUNDUP", 212, 2),
    ("ROW", 8, 0),
    ("ROWS", 76, 1),
    ("SEARCH", 82, 0),
    ("SECOND", 73, 1),
    ("SIGN", 26, 1),
    ("SIN", 15, 1),
    ("SINH", 229, 1),
    ("SLOPE", 315, 2),
    ("SMALL", 326, 2),
    ("SQRT", 20, 1),
    ("STANDARDIZE", 297, 3),
    ("STDEV", 12, 0),
    ("STDEVA", 366, 0),
    ("STDEVP", 193, 0),
    ("SUBSTITUTE", 120, 0),
    ("SUBTOTAL", 344, 0),
    ("SUM", 4, 0),
    ("SUMIF", 345, 0),
    ("SUMPRODUCT", 228, 0),
    ("SUMSQ", 321, 0),
    ("SYD", 143, 4),
    ("T", 130, 1),
    ("TAN", 17, 1),
    ("TANH", 231, 1),
    ("TEXT", 48, 2),
    ("TIME", 66, 3),
    ("TODAY", 221, 0),
    ("TRANSPOSE", 83, 1),
    ("TRIM", 118, 1),
    ("TRIMMEAN", 331, 2),
    ("TRUE", 34, 0),
    ("TRUNC", 197, 0),
    ("TYPE", 86, 1),
    ("UPPER", 113, 1),
    ("VALUE", 33, 1),
    ("VAR", 46, 0),
    ("VARA", 367, 0),
    ("VARP", 194, 0),
    ("VLOOKUP", 102, 0),
    ("WEEKDAY", 70, 0),
    ("YEAR", 69, 1),
    ];
    TABLE
        .binary_search_by(|(n, _, _)| (*n).cmp(name))
        .ok()
        .map(|i| (TABLE[i].1, TABLE[i].2))
}

/// 外部工作簿的表清单（写进 SUPBOOK，Excel 靠它显示表名）
#[derive(Clone, Default)]
pub struct BookSheets {
    pub file: String,
    pub sheets: Vec<String>,
}

/// 全局簿级结构：SUPBOOK 序号、EXTERNSHEET 条目
#[derive(Default)]
pub struct Externs {
    /// ixti → (supbook, 起表, 止表)
    pub entries: Vec<(u16, u16, u16)>,
    map: HashMap<(u16, u16, u16), u16>,
    /// supbook 1.. 是外部簿（0 是本簿自引用）
    pub books: Vec<BookSheets>,
    /// 用到了 2007+ 的 _xlfn 函数
    pub addin: bool,
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

    /// 登记一本外部簿，返回它的 supbook 序号
    pub fn book(&mut self, file: &str, sheets: Vec<String>) -> u16 {
        if let Some(i) = self
            .books
            .iter()
            .position(|b| b.file.eq_ignore_ascii_case(file))
        {
            return i as u16 + 1;
        }
        self.books.push(BookSheets {
            file: file.to_string(),
            sheets,
        });
        self.books.len() as u16
    }

    /// 延迟解析 add-in SUPBOOK 哨兵值：编译完所有外部簿后在写 EXTERNSHEET 时统一映射到 `books.len() + 1`
    pub const ADDIN_SUPBOOK: u16 = u16::MAX;

    pub fn addin_index(&mut self) -> u16 {
        self.addin = true;
        Self::ADDIN_SUPBOOK
    }

    pub fn resolved_supbook(&self, sup: u16) -> u16 {
        if sup == Self::ADDIN_SUPBOOK {
            self.books.len() as u16 + 1
        } else {
            sup
        }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// 名称空间：先用户名称（1..n），再 _xlfn 函数名（n+1..）
#[derive(Default)]
pub struct NameSpace {
    /// (名字, 作用表, 定义 rgce)
    pub user: Vec<(String, Option<usize>, Vec<u8>)>,
    /// 用到过但表里没有的函数，按出现顺序
    pub future: Vec<String>,
    defined: HashMap<String, u16>,
}

impl NameSpace {
    pub fn push_user(&mut self, name: String, scope: Option<usize>) -> u16 {
        let idx = self.user.len() as u16 + 1;
        self.defined.insert(name.to_lowercase(), idx);
        self.user.push((name, scope, Vec::new()));
        idx
    }

    pub fn set_rgce(&mut self, idx: u16, rgce: Vec<u8>) {
        if let Some(slot) = self.user.get_mut(idx as usize - 1) {
            slot.2 = rgce;
        }
    }

    pub fn lookup(&self, name: &str) -> Option<u16> {
        self.defined.get(&name.to_lowercase()).copied()
    }

    /// 未来函数：没有就登记一个 `_xlfn.名字`
    pub fn future_index(&mut self, name: &str) -> u16 {
        let tag = format!("_xlfn.{}", name.to_ascii_uppercase());
        let idx = match self
            .future
            .iter()
            .position(|n| n.eq_ignore_ascii_case(&tag))
        {
            Some(i) => i,
            None => {
                self.future.push(tag);
                self.future.len() - 1
            }
        };
        self.user.len() as u16 + idx as u16 + 1
    }
}

pub struct Compiler<'a> {
    pub sheets: &'a [String],
    pub cur_sheet: usize,
    pub externs: &'a mut Externs,
    pub names: &'a mut NameSpace,
    /// 外部簿表清单（来自 wb.links）
    pub books: &'a [BookSheets],
    /// 数组常量的值（FORMULA 记录尾部 rgcb）
    pub extra: Vec<u8>,
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
                out.push(1);
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
            Node::Empty => out.push(0x16),
            Node::Ref(r) => self.emit_ref(r, out)?,
            Node::Spill(_) => bail!("动态数组溢出引用 A1# 不能写入 .xls"),
            Node::Implicit(_) => bail!("隐式交集 @ 不能写入 .xls"),
            Node::Range(a, b) => self.emit_area(a, b, out)?,
            Node::Array(rows, cols, vals) => self.emit_array(*rows, *cols, vals, out)?,
            Node::ScopedName { .. } => bail!("作用域限定名称暂不支持写入 .xls"),
            Node::Name(n) => {
                let Some(idx) = self.names.lookup(n) else {
                    bail!("名称「{n}」没有定义，写不进 .xls");
                };
                out.push(0x23);
                out.extend_from_slice(&idx.to_le_bytes());
                out.extend_from_slice(&[0, 0]);
            }
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
                if let Some((idx, fixed)) = func_spec(&upper) {
                    for a in args {
                        self.emit(a, out)?;
                    }
                    if (fixed != 0 && args.len() as u8 == fixed)
                        || (fixed == 0 && args.is_empty() && is_const_func(&upper))
                    {
                        out.push(0x41);
                        out.extend_from_slice(&idx.to_le_bytes());
                    } else {
                        if args.len() > 250 {
                            bail!("函数 {name} 参数太多（{} 个），.xls 放不下", args.len());
                        }
                        out.push(0x42);
                        out.push(args.len() as u8);
                        out.extend_from_slice(&idx.to_le_bytes());
                    }
                } else {
                    // 2007+ 的函数：名字压栈 + ptgFuncVar(iftab=0xFF)
                    if args.len() > 250 {
                        bail!("函数 {name} 参数太多（{} 个），.xls 放不下", args.len());
                    }
                    let idx = self.names.future_index(&upper);
                    let addin = self.externs.addin_index();
                    self.externs.add(addin, 0xFFFE, 0xFFFE);
                    out.push(0x23);
                    out.extend_from_slice(&idx.to_le_bytes());
                    out.extend_from_slice(&[0, 0]);
                    for a in args {
                        self.emit(a, out)?;
                    }
                    out.push(0x42);
                    out.push(args.len() as u8 + 1);
                    out.extend_from_slice(&[0xFF, 0x00]);
                }
            }
        }
        Ok(())
    }

    /// 数组常量：token 里放行列数，值追加到 rgcb
    fn emit_array(
        &mut self,
        rows: usize,
        cols: usize,
        vals: &[Value],
        out: &mut Vec<u8>,
    ) -> Result<()> {
        if rows == 0 || cols == 0 || rows * cols > 0x40 {
            bail!("数组常量尺寸不支持（{rows}×{cols}）");
        }
        if vals.len() != rows * cols {
            bail!("数组常量元素个数不对（{rows}×{cols} 但给了 {} 个）", vals.len());
        }
        out.push(0x60);
        out.push((cols - 1) as u8);
        out.push((rows - 1) as u8);
        out.extend_from_slice(&[0, 0, 0, 0, 0]);
        self.extra.push((cols - 1) as u8);
        self.extra.push((rows - 1) as u8);
        self.extra.push(0);
        for v in vals {
            let (tag, payload): (u8, Vec<u8>) = match v {
                Value::Empty => (0x00, vec![0u8; 8]),
                Value::Number(n) => (0x01, n.to_le_bytes().to_vec()),
                Value::Bool(b) => {
                    let mut b8 = vec![0u8; 8];
                    b8[0] = if *b { 1 } else { 0 };
                    (0x04, b8)
                }
                Value::Error(e) => {
                    let mut b8 = vec![0u8; 8];
                    b8[0] = err_code(*e);
                    (0x10, b8)
                }
                Value::Text(_) => {
                    bail!("数组常量里的文本/嵌套数组暂时写不进 .xls")
                }
            };
            self.extra.push(tag);
            self.extra.extend_from_slice(&payload);
        }
        Ok(())
    }

    fn emit_ref(&mut self, r: &RefExpr, out: &mut Vec<u8>) -> Result<()> {
        let (row, col) = self.check_coords(r)?;
        if let (Some(first), Some(last)) = (r.sheet.as_deref(), r.sheet2.as_deref()) {
            let ixti = match r.book.as_deref() {
                Some(book) => self.book_ixti_span(book, first, last)?,
                None => self.ixti_span(first, last)?,
            };
            out.push(0x3A);
            out.extend_from_slice(&ixti.to_le_bytes());
            out.extend_from_slice(&row.to_le_bytes());
            out.extend_from_slice(&self.col_word(col, r).to_le_bytes());
            return Ok(());
        }
        if let Some(book) = r.book.clone() {
            let ixti = self.book_ixti(&book, r.sheet.as_deref())?;
            out.push(0x3A);
            out.extend_from_slice(&ixti.to_le_bytes());
            out.extend_from_slice(&row.to_le_bytes());
            out.extend_from_slice(&self.col_word(col, r).to_le_bytes());
            return Ok(());
        }
        match &r.sheet {
            None => {
                out.push(0x44);
                out.extend_from_slice(&row.to_le_bytes());
                out.extend_from_slice(&self.col_word(col, r).to_le_bytes());
            }
            Some(sh) => {
                let ixti = self.ixti(sh)?;
                out.push(0x3A);
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
        let book = a.book.clone().or_else(|| b.book.clone());
        let sheet = a.sheet.clone().or_else(|| b.sheet.clone());
        let end_sheet = b.sheet2.as_deref().or(a.sheet2.as_deref());
        if let (Some(first), Some(last)) = (sheet.as_deref(), end_sheet) {
            let ixti = match book.as_deref() {
                Some(bk) => self.book_ixti_span(bk, first, last)?,
                None => self.ixti_span(first, last)?,
            };
            out.push(0x3B);
            out.extend_from_slice(&ixti.to_le_bytes());
        } else if let Some(book) = book {
            let ixti = self.book_ixti(&book, sheet.as_deref())?;
            out.push(0x3B);
            out.extend_from_slice(&ixti.to_le_bytes());
        } else if let Some(sh) = sheet {
            let ixti = self.ixti(&sh)?;
            out.push(0x3B);
            out.extend_from_slice(&ixti.to_le_bytes());
        } else {
            out.push(0x45);
        }
        out.extend_from_slice(&r1.to_le_bytes());
        out.extend_from_slice(&r2.to_le_bytes());
        out.extend_from_slice(&self.col_word(c1, a).to_le_bytes());
        out.extend_from_slice(&self.col_word(c2, b).to_le_bytes());
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

    fn ixti_span(&mut self, first: &str, last: &str) -> Result<u16> {
        let find = |s: &str| -> Result<usize> {
            self.sheets
                .iter()
                .position(|x| x.eq_ignore_ascii_case(s))
                .ok_or_else(|| anyhow::anyhow!("引用了不存在的工作表 {s}"))
        };
        let mut a = find(first)?;
        let mut b = find(last)?;
        if a > b { std::mem::swap(&mut a, &mut b); }
        Ok(self.externs.add(0, a as u16, b as u16))
    }

    fn ixti(&mut self, sheet: &str) -> Result<u16> {
        let Some(i) = self
            .sheets
            .iter()
            .position(|s| s == sheet)
            .or_else(|| {
                self.sheets
                    .iter()
                    .position(|s| s.eq_ignore_ascii_case(sheet))
            })
        else {
            bail!("引用了不存在的工作表 {sheet}");
        };
        Ok(self.externs.add(0, i as u16, i as u16))
    }

    /// 跨簿三维引用：`[1]First:Third!A1` → 外部 supbook + 首尾表索引。
    fn book_ixti_span(&mut self, book: &str, first: &str, last: &str) -> Result<u16> {
        let idx: Option<usize> = if let Ok(n) = book.parse::<usize>() {
            Some(n.saturating_sub(1))
        } else {
            self.books.iter().position(|b| {
                b.file.eq_ignore_ascii_case(book)
                    || b.file.eq_ignore_ascii_case(&format!("{book}.xls"))
                    || b.file.eq_ignore_ascii_case(&format!("{book}.xlsx"))
            })
        };
        let Some(i) = idx else {
            bail!("跨工作簿引用 [{book}] 找不到对应的外部链接");
        };
        let entry = self.books.get(i).cloned().unwrap_or_default();
        let find = |s: &str| -> Result<usize> {
            entry
                .sheets
                .iter()
                .position(|x| x.eq_ignore_ascii_case(s))
                .ok_or_else(|| anyhow::anyhow!("外部工作簿 [{}] 找不到工作表 {s}", entry.file))
        };
        let mut a = find(first)?;
        let mut b = find(last)?;
        if a > b {
            std::mem::swap(&mut a, &mut b);
        }
        let sup = self.externs.book(&entry.file, entry.sheets.clone());
        Ok(self.externs.add(sup, a as u16, b as u16))
    }

    /// 跨簿引用：book 可能是 `[1]` 这样的序号，也可能是文件名
    fn book_ixti(&mut self, book: &str, sheet: Option<&str>) -> Result<u16> {
        let idx: Option<usize> = if let Ok(n) = book.parse::<usize>() {
            Some(n.saturating_sub(1))
        } else {
            self.books.iter().position(|b| {
                b.file.eq_ignore_ascii_case(book)
                    || b.file.eq_ignore_ascii_case(&format!("{book}.xls"))
                    || b.file.eq_ignore_ascii_case(&format!("{book}.xlsx"))
            })
        };
        let Some(i) = idx else {
            bail!("跨工作簿引用 [{book}] 找不到对应的外部链接");
        };
        let entry = self.books.get(i).cloned().unwrap_or_default();
        let sheet_no = sheet
            .and_then(|s| entry.sheets.iter().position(|x| x.eq_ignore_ascii_case(s)))
            .unwrap_or(0) as u16;
        let sup = self.externs.book(&entry.file, entry.sheets.clone());
        Ok(self.externs.add(sup, sheet_no, sheet_no))
    }
}

fn is_const_func(name: &str) -> bool {
    matches!(name, "PI" | "NOW" | "TODAY" | "TRUE" | "FALSE" | "NA" | "RAND")
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

/// NAME 记录（0x0018）的 body
pub fn name_record(name: &str, scope: Option<usize>, rgce: &[u8], future: bool) -> Vec<u8> {
    let mut b = Vec::new();
    // 未来函数名带隐藏+函数标记，Excel 自己写的是 0x000B
    b.extend_from_slice(&(if future { 0x000Bu16 } else { 0x0000 }).to_le_bytes());
    b.push(0); // chKey
    b.push(name.chars().count().min(255) as u8);
    b.extend_from_slice(&(rgce.len() as u16).to_le_bytes());
    b.extend_from_slice(&0u16.to_le_bytes()); // ixals
    b.extend_from_slice(&(scope.unwrap_or(0) as u16).to_le_bytes());
    // 四个 cch* + 1 字节标志：Excel 自己写的 NAME 头部是 15 字节（实测）
    b.extend_from_slice(&[0, 0, 0, 0, 0]);
    for c in name.chars().take(255) {
        b.push(c as u32 as u8); // 名称都是 ASCII
    }
    b.extend_from_slice(rgce);
    b
}

/// `_xlfn.xxx` 名称的定义：默认 #NAME?
pub fn future_rgce() -> Vec<u8> {
    vec![0x1C, 29]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::formula::parse;

    struct Fix {
        sheets: Vec<String>,
        externs: Externs,
        names: NameSpace,
        books: Vec<BookSheets>,
    }

    impl Fix {
        fn new() -> Self {
            Self {
                sheets: vec!["S".into(), "T".into()],
                externs: Externs::default(),
                names: NameSpace::default(),
                books: Vec::new(),
            }
        }
        fn go(&mut self, src: &str) -> Result<Vec<u8>> {
            let node = parse(src).unwrap();
            let mut c = Compiler {
                sheets: &self.sheets,
                cur_sheet: 0,
                externs: &mut self.externs,
                names: &mut self.names,
                books: &self.books,
                extra: Vec::new(),
            };
            c.compile(&node)
        }
    }

    #[test]
    fn arithmetic_becomes_rpn() {
        let mut f = Fix::new();
        let t = f.go("1+2*3").unwrap();
        assert_eq!(t, vec![0x1E, 1, 0, 0x1E, 2, 0, 0x1E, 3, 0, 0x05, 0x03]);
    }

    #[test]
    fn refs_carry_relative_flags() {
        let mut f = Fix::new();
        let t = f.go("A1").unwrap();
        assert_eq!(t[0], 0x44);
        let col = u16::from_le_bytes([t[3], t[4]]);
        assert_eq!(col & 0x3FFF, 0);
        assert_eq!(col & 0xC000, 0xC000);
    }

    #[test]
    fn absolute_ref_clears_flags() {
        let mut f = Fix::new();
        let t = f.go("$B$2").unwrap();
        let col = u16::from_le_bytes([t[3], t[4]]);
        assert_eq!(col & 0x3FFF, 1);
        assert_eq!(col & 0xC000, 0);
    }

    #[test]
    fn function_uses_verified_index() {
        let mut f = Fix::new();
        let t = f.go("SUM(A1:A2)").unwrap();
        assert_eq!(&t[t.len() - 4..], &[0x42, 1, 4, 0], "SUM 可变参数 iftab=4");
    }

    #[test]
    fn fixed_arity_function_uses_ptgfunc() {
        let mut f = Fix::new();
        let t = f.go("ABS(-1)").unwrap();
        assert_eq!(&t[t.len() - 3..], &[0x41, 24, 0], "ABS 走 ptgFunc");
    }

    #[test]
    fn future_function_gets_xlfn_name() {
        let mut f = Fix::new();
        let t = f.go("XLOOKUP(1,A1:A2,B1:B2)").unwrap();
        assert_eq!(t[0], 0x23, "先把 _xlfn.XLOOKUP 压栈");
        assert_eq!(&t[t.len() - 4..], &[0x42, 4, 0xFF, 0x00], "3 个参数 + 名字");
        assert_eq!(f.names.future, vec!["_xlfn.XLOOKUP"]);
        assert!(f.externs.addin);
    }

    #[test]
    fn array_constant_fills_extra() {
        let mut f = Fix::new();
        let node = parse("SUM({1,2;3,4})").unwrap();
        let mut c = Compiler {
            sheets: &f.sheets,
            cur_sheet: 0,
            externs: &mut f.externs,
            names: &mut f.names,
            books: &f.books,
            extra: Vec::new(),
        };
        let t = c.compile(&node).unwrap();
        assert_eq!(&t[..8], &[0x60, 1, 1, 0, 0, 0, 0, 0], "2×2 的 ptgArray");
        assert_eq!(c.extra.len(), 3 + 4 * 9, "3 字节头 + 4 个 9 字节值");
        assert_eq!(&c.extra[..3], &[1, 1, 0]);
    }

    #[test]
    fn cross_sheet_and_cross_book() {
        let mut f = Fix::new();
        let t = f.go("T!A1").unwrap();
        assert_eq!(t[0], 0x3A);
        assert_eq!(u16::from_le_bytes([t[1], t[2]]), 0);

        f.books.push(BookSheets {
            file: "book_b.xls".into(),
            sheets: vec!["Sheet1".into()],
        });
        let t = f.go("[book_b.xls]Sheet1!A1").unwrap();
        assert_eq!(t[0], 0x3A);
        assert_eq!(u16::from_le_bytes([t[1], t[2]]), 1, "第一条外部簿");
        assert_eq!(f.externs.books.len(), 1);

        let mut f2 = Fix::new();
        f2.books.push(BookSheets {
            file: "book_b.xls".into(),
            sheets: vec!["First".into(), "Second".into(), "Third".into()],
        });
        let t = f2.go("SUM([book_b.xls]First:Third!A1)").unwrap();
        assert_eq!(t[0], 0x3A);
        let ixti = u16::from_le_bytes([t[1], t[2]]) as usize;
        assert_eq!(f2.externs.entries[ixti], (1, 0, 2), "外部三维引用必须覆盖 First..Third");
        let t = f2.go("SUM([book_b.xls]First:Third!A1:B2)").unwrap();
        assert_eq!(t[0], 0x3B);
        let ixti = u16::from_le_bytes([t[1], t[2]]) as usize;
        assert_eq!(f2.externs.entries[ixti], (1, 0, 2), "外部三维区域同样覆盖 First..Third");
    }

    #[test]
    fn three_d_ref_emits_ref3d_and_span_ixti() {
        let mut f = Fix::new();
        f.sheets = vec!["First".into(), "Second".into(), "Third".into()];
        let t = f.go("SUM(First:Third!A1)").unwrap();
        // 0x3A = Ref3DPtg；不能用 0x5A（Excel 会显示 @ 并算 #REF!）。
        assert_eq!(t[0], 0x3A);
        let ixti = u16::from_le_bytes([t[1], t[2]]) as usize;
        assert_eq!(f.externs.entries[ixti], (0, 0, 2), "ixti 必须覆盖 First..Third");
        let t = f.go("SUM(First:Third!A1:B1)").unwrap();
        assert_eq!(t[0], 0x3B);
        let ixti = u16::from_le_bytes([t[1], t[2]]) as usize;
        assert_eq!(f.externs.entries[ixti], (0, 0, 2), "Area3D 同样保留首尾表");
    }

    #[test]
    fn defined_name_uses_ptgname() {
        let mut f = Fix::new();
        f.names.push_user("Total".into(), None);
        let t = f.go("Total*2").unwrap();
        assert_eq!(t[0], 0x23);
        assert_eq!(u16::from_le_bytes([t[1], t[2]]), 1, "第 1 条 NAME 记录");
    }

    #[test]
    fn name_record_layout() {
        let b = name_record("Total", None, &[0x1E, 1, 0], false);
        assert_eq!(u16::from_le_bytes([b[0], b[1]]), 0);
        assert_eq!(b[3], 5, "名字长度");
        assert_eq!(u16::from_le_bytes([b[4], b[5]]), 3, "cce");
        assert_eq!(&b[15..20], b"Total");
        assert_eq!(&b[20..], &[0x1E, 1, 0]);
    }
}
