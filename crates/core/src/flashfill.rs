//! 快速填充（Flash Fill）：从目标列已有的示例反推一条变换规则，再套用到其余行。
//!
//! Excel 用的是基于程序合成的搜索引擎；这里给的是**候选规则集 + 示例匹配**：
//! 先把常见的拆分/截取/大小写/拼接写成候选，逐个拿目标列里的示例去比对，
//! 全部示例都命中的第一条就用。规则表达不出来时明确报错，不猜。

use crate::model::{Cell, Sheet, Value};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Rule {
    /// 原样
    Whole,
    /// 第一个分隔符之前 / 之后
    BeforeFirst(char),
    AfterFirst(char),
    /// 最后一个分隔符之后
    AfterLast(char),
    /// 前 N 个字符 / 后 N 个字符
    Left(usize),
    Right(usize),
    /// 只留数字 / 只留非数字
    Digits,
    NonDigits,
    /// 大小写
    Upper,
    Lower,
    /// 拼接：源 + 分隔符 + 另一列
    Concat(char, u32),
    /// 直接拼接：源 + 另一列（无分隔符）
    Join(u32),
}

impl Rule {
    pub fn label(&self) -> String {
        match self {
            Rule::Whole => "原样".into(),
            Rule::BeforeFirst(c) => format!("「{c}」之前"),
            Rule::AfterFirst(c) => format!("「{c}」之后"),
            Rule::AfterLast(c) => format!("最后一段"),
            Rule::Left(n) => format!("前 {n} 个字符"),
            Rule::Right(n) => format!("后 {n} 个字符"),
            Rule::Digits => "只留数字".into(),
            Rule::NonDigits => "只留非数字".into(),
            Rule::Upper => "大写".into(),
            Rule::Lower => "小写".into(),
            Rule::Concat(c, c2) => format!("与 {} 列用「{c}」拼接", crate::model::col_label(*c2)),
            Rule::Join(c2) => format!("与 {} 列直接拼接", crate::model::col_label(*c2)),
        }
    }

    pub fn apply(&self, src: &str, sheet: &Sheet, row: u32) -> String {
        match self {
            Rule::Whole => src.to_string(),
            Rule::BeforeFirst(c) => src.split(*c).next().unwrap_or("").to_string(),
            Rule::AfterFirst(c) => src.splitn(2, *c).nth(1).unwrap_or("").to_string(),
            Rule::AfterLast(c) => src.rsplit(*c).next().unwrap_or("").to_string(),
            Rule::Left(n) => src.chars().take(*n).collect(),
            Rule::Right(n) => {
                let len = src.chars().count();
                src.chars().skip(len.saturating_sub(*n)).collect()
            }
            Rule::Digits => src.chars().filter(|c| c.is_ascii_digit()).collect(),
            Rule::NonDigits => src
                .chars()
                .filter(|c| !c.is_ascii_digit() && !c.is_whitespace())
                .collect(),
            Rule::Upper => src.to_uppercase(),
            Rule::Lower => src.to_lowercase(),
            Rule::Concat(c, col) => {
                let other = sheet.value(row, *col).display();
                format!("{src}{c}{other}")
            }
            Rule::Join(col) => {
                let other = sheet.value(row, *col).display();
                format!("{src}{other}")
            }
        }
    }
}

const DELIMS: [char; 7] = [' ', ',', '，', '-', '_', '/', '|'];

#[derive(Debug)]
pub struct FlashFill {
    pub rule: Rule,
    pub filled: usize,
}

/// 用 `dst_col` 里已有的示例反推规则，并填到 `r1..=r2` 的空目标格里。
pub fn flash_fill(
    sheet: &mut Sheet,
    src_col: u32,
    dst_col: u32,
    r1: u32,
    r2: u32,
    aux_col: Option<u32>,
) -> Result<FlashFill, String> {
    if src_col == dst_col {
        return Err("源列和目标列不能是同一列".into());
    }
    // 示例：目标列里已经有文字的格子
    let mut examples: Vec<(u32, String, String)> = Vec::new();
    for r in r1..=r2 {
        let dst = sheet.value(r, dst_col).display();
        if dst.trim().is_empty() {
            continue;
        }
        let src = sheet.value(r, src_col).display();
        if src.trim().is_empty() {
            continue;
        }
        examples.push((r, src, dst));
    }
    if examples.is_empty() {
        return Err("先在目标列写一个示例（比如从「张三-研发」里取出「研发」）".into());
    }
    // 候选规则
    let mut cands: Vec<Rule> = vec![
        Rule::Whole,
        Rule::Digits,
        Rule::NonDigits,
        Rule::Upper,
        Rule::Lower,
    ];
    for d in DELIMS {
        cands.push(Rule::BeforeFirst(d));
        cands.push(Rule::AfterFirst(d));
        cands.push(Rule::AfterLast(d));
    }
    // 长度类规则按示例长度推
    let mut lens: Vec<usize> = examples
        .iter()
        .map(|(_, _, dst)| dst.chars().count())
        .collect();
    lens.sort_unstable();
    lens.dedup();
    for n in lens {
        if n > 0 && n < 60 {
            cands.push(Rule::Left(n));
            cands.push(Rule::Right(n));
        }
    }
    if let Some(a) = aux_col {
        cands.push(Rule::Join(a));
        for d in [' ', '-', '/', '_'] {
            cands.push(Rule::Concat(d, a));
        }
    }
    let rule = cands
        .into_iter()
        .find(|rule| {
            examples
                .iter()
                .all(|(row, src, want)| rule.apply(src, sheet, *row) == *want)
        })
        .ok_or_else(|| {
            format!(
                "从 {} 条示例里推不出规则（示例：{:?}）：换一个更规整的示例试试",
                examples.len(),
                examples.first().map(|(_, s, d)| format!("{s} → {d}"))
            )
        })?;
    let mut filled = 0usize;
    for r in r1..=r2 {
        let src = sheet.value(r, src_col).display();
        if src.trim().is_empty() {
            continue;
        }
        if !sheet.value(r, dst_col).display().trim().is_empty() {
            continue;
        }
        let out = rule.apply(&src, sheet, r);
        if out.is_empty() {
            continue;
        }
        sheet.set(
            r,
            dst_col,
            Cell {
                value: Value::Text(out.into()),
                formula: None,
            },
        );
        filled += 1;
    }
    Ok(FlashFill { rule, filled })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sheet_with(rows: &[(&str, &str, &str)]) -> Sheet {
        let mut sh = Sheet::new("S");
        for (i, (a, b, c)) in rows.iter().enumerate() {
            let r = i as u32;
            sh.set(r, 0, Cell { value: Value::Text((*a).into()), formula: None });
            sh.set(r, 1, Cell { value: Value::Text((*b).into()), formula: None });
            sh.set(r, 2, Cell { value: Value::Text((*c).into()), formula: None });
        }
        sh
    }

    #[test]
    fn learns_suffix_after_dash() {
        let mut sh = sheet_with(&[
            ("张三-研发部", "研发部", ""),
            ("李四-销售部", "", ""),
            ("王五-研发部", "", ""),
        ]);
        let out = flash_fill(&mut sh, 0, 1, 0, 2, None).unwrap();
        assert_eq!(out.rule, Rule::AfterFirst('-'));
        assert_eq!(out.filled, 2);
        assert_eq!(sh.value(1, 1).display(), "销售部");
        assert_eq!(sh.value(2, 1).display(), "研发部");
    }

    #[test]
    fn learns_digits_only() {
        let mut sh = sheet_with(&[
            ("订单 A-1001", "1001", ""),
            ("订单 B-2002", "", ""),
        ]);
        let out = flash_fill(&mut sh, 0, 1, 0, 1, None).unwrap();
        assert_eq!(out.rule, Rule::Digits);
        assert_eq!(sh.value(1, 1).display(), "2002");
    }

    #[test]
    fn learns_concat_from_aux_column() {
        let mut sh = Sheet::new("S");
        sh.set(0, 0, Cell { value: Value::Text("张".into()), formula: None });
        sh.set(0, 1, Cell { value: Value::Text("三".into()), formula: None });
        sh.set(0, 2, Cell { value: Value::Text("张三".into()), formula: None });
        sh.set(1, 0, Cell { value: Value::Text("李".into()), formula: None });
        sh.set(1, 1, Cell { value: Value::Text("四".into()), formula: None });
        let out = flash_fill(&mut sh, 0, 2, 0, 1, Some(1)).unwrap();
        assert_eq!(out.rule, Rule::Join(1), "示例「张三」没有分隔符 → 直接拼接");
        assert_eq!(sh.value(1, 2).display(), "李四");
    }

    #[test]
    fn refuses_without_examples() {
        let mut sh = sheet_with(&[("甲", "", ""), ("乙", "", "")]);
        let err = flash_fill(&mut sh, 0, 1, 0, 1, None).unwrap_err();
        assert!(err.contains("示例"), "{err}");
    }

    #[test]
    fn refuses_when_no_rule_matches() {
        let mut sh = sheet_with(&[("abc", "xyz", ""), ("abd", "", "")]);
        let err = flash_fill(&mut sh, 0, 1, 0, 1, None).unwrap_err();
        assert!(err.contains("推不出"), "{err}");
    }

    #[test]
    fn does_not_overwrite_existing_targets() {
        let mut sh = sheet_with(&[
            ("A-1", "1", ""),
            ("B-2", "2", ""),
            ("C-3", "", ""),
        ]);
        let out = flash_fill(&mut sh, 0, 1, 0, 2, None).unwrap();
        assert_eq!(out.filled, 1, "只填空格");
        assert_eq!(sh.value(1, 1).display(), "2", "已有内容不动");
        assert_eq!(sh.value(2, 1).display(), "3");
    }
}
