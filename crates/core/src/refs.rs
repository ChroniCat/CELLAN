//! 公式里的引用改写：插行删行之后，公式里的 A1、$B$2、A1:C5、A:A、1:3 要跟着动。
//!
//! 这不是简单的字符串替换——必须跳过字符串字面量（`"A1"` 不能动）、
//! 跳过函数名（`LOG10(` 不是单元格）、认出引号包着的表名（`'我的 表'!A1`）。
//!
//! 规则和 Excel 一致：**绝对引用也照样平移**（`$A$1` 插行后变 `$A$2`），
//! 只有引用落进被删掉的那一块才变成 `#REF!`。

use crate::model::col_label;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    Row,
    Col,
}

/// 一次平移
#[derive(Clone, Debug)]
pub struct Shift {
    pub axis: Axis,
    /// 起始下标（0 基）
    pub at: u32,
    pub n: u32,
    pub delete: bool,
    /// 被改的工作表名（用于判断带表名的引用要不要动）
    pub target_sheet: String,
    /// 公式所在的工作表名
    pub own_sheet: String,
    /// 平移模式（复制粘贴用）：(行偏移, 列偏移)，设了就忽略 at/n/delete
    pub translate: Option<(i64, i64)>,
}

impl Shift {
    /// 区间平移：删除时区间会「收缩」而不是整体报废（A:C 删掉 A 列 → A:B）
    pub fn map_range(&self, a: u32, b: u32) -> Option<(u32, u32)> {
        let (a, b) = if a <= b { (a, b) } else { (b, a) };
        if !self.delete {
            return Some((self.map(a)?, self.map(b)?));
        }
        let del_end = self.at.saturating_add(self.n); // 不含
        // 完全在被删区之前
        if b < self.at {
            return Some((a, b));
        }
        // 完全在被删区之后
        if a >= del_end {
            return Some((a - self.n, b - self.n));
        }
        // 被删掉之后剩下的最小/最大下标
        let new_a = if a < self.at { a } else { self.at };
        let new_b = if b >= del_end { b - self.n } else { self.at.saturating_sub(1) };
        if b < self.at || new_a > new_b {
            return None;
        }
        if new_a > new_b {
            return None;
        }
        Some((new_a, new_b))
    }

    /// 一个下标平移到哪；None 表示落进被删的区间
    pub fn map(&self, v: u32) -> Option<u32> {
        if self.delete {
            if v < self.at {
                Some(v)
            } else if v < self.at.saturating_add(self.n) {
                None
            } else {
                Some(v - self.n)
            }
        } else if v >= self.at {
            Some(v + self.n)
        } else {
            Some(v)
        }
    }
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'.'
}

fn col_index(letters: &[u8]) -> Option<u32> {
    let mut v: u32 = 0;
    for b in letters {
        let d = b.to_ascii_uppercase();
        if !d.is_ascii_uppercase() {
            return None;
        }
        v = v.checked_mul(26)?.checked_add((d - b'A') as u32 + 1)?;
        if v > 16_384 {
            return None;
        }
    }
    Some(v - 1)
}

fn row_index(digits: &[u8]) -> Option<u32> {
    let s = std::str::from_utf8(digits).ok()?;
    let v: u32 = s.parse().ok()?;
    if v == 0 || v > 1_048_576 {
        return None;
    }
    Some(v - 1)
}

struct Ref {
    /// 结束位置（不含）
    end: usize,
    /// 替换文本
    text: String,
}

/// 解析一个单元格引用或区域，返回替换后的文本（不需要改就返回 None）
fn parse_ref(b: &[u8], start: usize, sh: &Shift, qualified_sheet: Option<&str>) -> Option<Ref> {
    let mut i = start;
    let d1 = if b.get(i) == Some(&b'$') {
        i += 1;
        true
    } else {
        false
    };
    let ls = i;
    while i < b.len() && b[i].is_ascii_alphabetic() {
        i += 1;
    }
    let letters = &b[ls..i];
    if letters.is_empty() || letters.len() > 3 {
        return None;
    }
    let d2 = if b.get(i) == Some(&b'$') {
        i += 1;
        true
    } else {
        false
    };
    let ds = i;
    while i < b.len() && b[i].is_ascii_digit() {
        i += 1;
    }
    let digits = &b[ds..i];

    // 整列引用 A:C
    if digits.is_empty() {
        if b.get(i) == Some(&b':') {
            let mut j = i + 1;
            while j < b.len() && (b[j] == b'$') {
                j += 1;
            }
            let ls2 = j;
            while j < b.len() && b[j].is_ascii_alphabetic() {
                j += 1;
            }
            if j > ls2 && j - ls2 <= 3 {
                if let (Some(c1), Some(c2)) = (col_index(letters), col_index(&b[ls2..j])) {
                    if sh.axis != Axis::Col || !shift_applies(sh, qualified_sheet) {
                        return None;
                    }
                    let text = if sh.translate.is_some() {
                        match (apply(sh, 0, c1), apply(sh, 0, c2)) {
                            ((Some(_), Some(x)), (Some(_), Some(y))) => {
                                format!("{}:{}", col_label(x), col_label(y))
                            }
                            _ => "#REF!".to_string(),
                        }
                    } else {
                        match sh.map_range(c1, c2) {
                            Some((x, y)) => format!("{}:{}", col_label(x), col_label(y)),
                            None => "#REF!".to_string(),
                        }
                    };
                    return Some(Ref { end: j, text });
                }
            }
        }
        return None;
    }

    // 函数名（LOG10( 之类）不是引用
    if b.get(i) == Some(&b'(') {
        return None;
    }
    // 前面紧挨着字母/数字说明是更长标识符的一部分
    if start > 0 && is_ident_byte(b[start - 1]) {
        return None;
    }

    let c1 = col_index(letters)?;
    let r1 = row_index(digits)?;
    // 区域：A1:B2
    if b.get(i) == Some(&b':') {
        let mut j = i + 1;
        let d3 = if b.get(j) == Some(&b'$') {
            j += 1;
            true
        } else {
            false
        };
        let ls2 = j;
        while j < b.len() && b[j].is_ascii_alphabetic() {
            j += 1;
        }
        let letters2 = &b[ls2..j];
        let d4 = if b.get(j) == Some(&b'$') {
            j += 1;
            true
        } else {
            false
        };
        let ds2 = j;
        while j < b.len() && b[j].is_ascii_digit() {
            j += 1;
        }
        if !letters2.is_empty() && !b[ds2..j].is_empty() {
            if let (Some(c2), Some(r2)) = (col_index(letters2), row_index(&b[ds2..j])) {
                if b.get(j) == Some(&b'(') {
                    return None;
                }
                if !shift_applies(sh, qualified_sheet) {
                    return None;
                }
                let none2 = (None::<u32>, None::<u32>);
                let (p1, p2) = if sh.translate.is_some() {
                    (apply(sh, r1, c1), apply(sh, r2, c2))
                } else if sh.axis == Axis::Row {
                    // 已经 map 过了，不能再走 apply（否则平移两次）
                    match sh.map_range(r1, r2) {
                        Some((x, y)) => ((Some(x), Some(c1)), (Some(y), Some(c2))),
                        None => (none2, none2),
                    }
                } else {
                    match sh.map_range(c1, c2) {
                        Some((x, y)) => ((Some(r1), Some(x)), (Some(r2), Some(y))),
                        None => (none2, none2),
                    }
                };
                let (nr1, nc1) = p1;
                let (nr2, nc2) = p2;
                let text = match (nr1, nc1, nr2, nc2) {
                    (Some(a), Some(b1), Some(c), Some(d)) => format!(
                        "{}{}{}:{}{}{}",
                        if d1 { "$" } else { "" },
                        col_label(b1),
                        if d2 { format!("${}", a + 1) } else { format!("{}", a + 1) },
                        if d3 { "$" } else { "" },
                        col_label(d),
                        if d4 { format!("${}", c + 1) } else { format!("{}", c + 1) },
                    ),
                    _ => "#REF!".to_string(),
                };
                return Some(Ref { end: j, text });
            }
        }
        return None;
    }

    if !shift_applies(sh, qualified_sheet) {
        return None;
    }
    let (nr, nc) = apply(sh, r1, c1);
    let text = match (nr, nc) {
        (Some(r), Some(c)) => format!(
            "{}{}{}{}",
            if d1 { "$" } else { "" },
            col_label(c),
            if d2 { "$" } else { "" },
            r + 1
        ),
        _ => "#REF!".to_string(),
    };
    Some(Ref { end: i, text })
}

/// 平移或结构位移的统一出口
fn apply(sh: &Shift, r: u32, c: u32) -> (Option<u32>, Option<u32>) {
    if let Some((dr, dc)) = sh.translate {
        let nr = r as i64 + dr;
        let nc = c as i64 + dc;
        if nr < 0 || nc < 0 || nr >= 1_048_576 || nc >= 16_384 {
            return (None, None);
        }
        return (Some(nr as u32), Some(nc as u32));
    }
    match sh.axis {
        Axis::Row => (sh.map(r), Some(c)),
        Axis::Col => (Some(r), sh.map(c)),
    }
}

fn shift_applies(sh: &Shift, qualified: Option<&str>) -> bool {
    match qualified {
        Some(s) => s.eq_ignore_ascii_case(&sh.target_sheet),
        None => sh.own_sheet.eq_ignore_ascii_case(&sh.target_sheet),
    }
}

/// 复制粘贴时按 (dr, dc) 平移公式里的相对引用（绝对引用也平移，和处理插删一致）
pub fn translate(formula: &str, dr: i64, dc: i64, sheet: &str) -> String {
    let sh = Shift {
        axis: Axis::Row,
        at: 0,
        n: 0,
        delete: false,
        target_sheet: sheet.to_string(),
        own_sheet: sheet.to_string(),
        translate: Some((dr, dc)),
    };
    rewrite(formula, &sh)
}

/// 改写一条公式（不带前导 `=` 也可以）
pub fn rewrite(formula: &str, sh: &Shift) -> String {
    let b = formula.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(formula.len() + 8);
    let mut i = 0usize;
    let mut qual: Option<String> = None;
    while i < b.len() {
        let c = b[i];
        // 字符串字面量：原样搬，内部的 "" 是转义
        if c == b'"' {
            let start = i;
            i += 1;
            while i < b.len() {
                if b[i] == b'"' {
                    if b.get(i + 1) == Some(&b'"') {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            out.extend_from_slice(&b[start..i]);
            continue;
        }
        // 带引号的表名：'Sheet 1'!
        if c == b'\'' {
            let start = i;
            i += 1;
            let ns = i;
            while i < b.len() && b[i] != b'\'' {
                i += 1;
            }
            let name = &formula[ns..i];
            if i < b.len() {
                i += 1; // 收尾引号
            }
            out.extend_from_slice(&b[start..i]);
            // 后跟 ! 才算表名限定（顺手把这个 ! 吃掉，否则下面会被当成
            // 新的限定符起点，把刚记下的表名清掉）
            if b.get(i) == Some(&b'!') {
                qual = Some(name.replace("''", "'"));
                out.push(b'!');
                i += 1;
            }
            continue;
        }
        // 裸表名：Sheet2!A1 / 数据!A1（表名可以是中文）
        if c.is_ascii_alphabetic() || c >= 0x80 {
            let start = i;
            let mut j = i;
            while j < b.len()
                && (b[j].is_ascii_alphanumeric() || b[j] == b'_' || b[j] == b'.' || b[j] >= 0x80)
            {
                j += 1;
            }
            if b.get(j) == Some(&b'!') {
                out.extend_from_slice(&b[start..=j]);
                qual = Some(formula[start..j].to_string());
                i = j + 1;
                continue;
            }
        }
        // 引用
        if c == b'$' || c.is_ascii_alphabetic() || c.is_ascii_digit() {
            let boundary = i == 0 || !is_ident_byte(b[i - 1]);
            if boundary {
                if let Some(r) = parse_ref(b, i, sh, qual.as_deref()) {
                    out.extend_from_slice(r.text.as_bytes());
                    i = r.end;
                    qual = None;
                    continue;
                }
            }
        }
        if c == b'!' {
            qual = None;
        }
        out.push(c);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

/// 整行引用 1:3
pub fn rewrite_row_range(text: &str, sh: &Shift) -> String {
    // 只处理形如 "3:7" 的裸行区间（放在 rewrite 之后兜底用）
    let t = text.trim();
    if let Some((a, b)) = t.split_once(':') {
        if let (Ok(x), Ok(y)) = (a.parse::<u32>(), b.parse::<u32>()) {
            if sh.axis == Axis::Row && shift_applies(sh, None) {
                return match (sh.map(x - 1), sh.map(y - 1)) {
                    (Some(p), Some(q)) => format!("{}:{}", p + 1, q + 1),
                    _ => "#REF!".into(),
                };
            }
        }
    }
    text.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn insert_row(at: u32, n: u32) -> Shift {
        Shift {
            axis: Axis::Row,
            at,
            n,
            delete: false,
            target_sheet: "S".into(),
            own_sheet: "S".into(),
            translate: None,
        }
    }
    fn delete_row(at: u32, n: u32) -> Shift {
        Shift { delete: true, ..insert_row(at, n) }
    }
    fn insert_col(at: u32, n: u32) -> Shift {
        Shift { axis: Axis::Col, ..insert_row(at, n) }
    }
    fn delete_col(at: u32, n: u32) -> Shift {
        Shift { axis: Axis::Col, delete: true, ..insert_row(at, n) }
    }

    #[test]
    fn insert_row_moves_refs() {
        assert_eq!(rewrite("=A1+1", &insert_row(0, 1)), "=A2+1");
        assert_eq!(rewrite("=A5*2", &insert_row(0, 2)), "=A7*2");
        assert_eq!(rewrite("=A1+B9", &insert_row(3, 1)), "=A1+B10");
    }

    #[test]
    fn absolute_refs_move_too() {
        assert_eq!(rewrite("=$A$1+$B2", &insert_row(0, 1)), "=$A$2+$B3");
    }

    #[test]
    fn delete_row_gives_ref_error() {
        assert_eq!(rewrite("=A2+1", &delete_row(1, 1)), "=#REF!+1");
        assert_eq!(rewrite("=A3+1", &delete_row(1, 1)), "=A2+1");
        assert_eq!(rewrite("=A1+1", &delete_row(1, 1)), "=A1+1");
    }

    #[test]
    fn ranges_shift_on_both_ends() {
        assert_eq!(rewrite("=SUM(A1:A5)", &insert_row(0, 1)), "=SUM(A2:A6)");
        assert_eq!(rewrite("=SUM($A$1:$A$5)", &insert_row(2, 1)), "=SUM($A$1:$A$6)");
        // 区间收缩：只删掉一部分时保留活下来的那一格
        assert_eq!(rewrite("=SUM(A1:A5)", &delete_row(1, 4)), "=SUM(A1:A1)");
        // 整段都被删掉才是 #REF!
        assert_eq!(rewrite("=SUM(A2:A5)", &delete_row(1, 4)), "=SUM(#REF!)");
    }

    #[test]
    fn columns_and_whole_columns() {
        assert_eq!(rewrite("=B1*2", &insert_col(1, 1)), "=C1*2");
        assert_eq!(rewrite("=SUM(B:B)", &insert_col(1, 1)), "=SUM(C:C)");
        assert_eq!(rewrite("=SUM(A:C)", &delete_col(0, 1)), "=SUM(A:B)");
        // 行方向的改动不该动列引用
        assert_eq!(rewrite("=B1*2", &insert_row(0, 3)), "=B4*2");
    }

    #[test]
    fn strings_and_functions_untouched() {
        assert_eq!(rewrite("=\"A1 不是引用\"&A1", &insert_row(0, 1)), "=\"A1 不是引用\"&A2");
        assert_eq!(rewrite("=LOG10(A1)", &insert_row(0, 1)), "=LOG10(A2)");
        assert_eq!(rewrite("=LOG10(100)", &insert_row(0, 1)), "=LOG10(100)");
        assert_eq!(rewrite("=IF(A1>0,\"A1\",\"\")", &insert_row(0, 1)), "=IF(A2>0,\"A1\",\"\")");
    }

    #[test]
    fn sheet_qualifiers_respected() {
        let sh = Shift {
            axis: Axis::Row,
            at: 0,
            n: 1,
            delete: false,
            target_sheet: "数据".into(),
            own_sheet: "汇总".into(),
            translate: None,
        };
        // 指着被改的表 → 跟着动
        assert_eq!(rewrite("=数据!A1", &sh), "=数据!A2");
        assert_eq!(rewrite("='数据'!A1", &sh), "='数据'!A2");
        // 指着别的表 → 不动
        assert_eq!(rewrite("=别处!A1", &sh), "=别处!A1");
        // 不带表名 = 本表（汇总），不是被改的表 → 不动
        assert_eq!(rewrite("=A1", &sh), "=A1");
    }

    #[test]
    fn leaves_plain_numbers_alone() {
        assert_eq!(rewrite("=1+2*3", &insert_row(0, 1)), "=1+2*3");
        assert_eq!(rewrite("=SUM(1,2)", &insert_row(0, 1)), "=SUM(1,2)");
    }
}
