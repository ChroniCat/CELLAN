//! 数据工具：多级排序、删除重复、分列。
//!
//! 和 Excel 一样：排序前整块备份（按原始行序），落位后写回；删除重复保留第一次出现；
//! 分列把一格按分隔符拆到右边相邻的列，右边的原有内容会被推开。

use crate::model::{Cell, Sheet, Value};

/// 一个排序键
#[derive(Clone, Debug, PartialEq)]
pub struct SortKey {
    pub col: u32,
    pub asc: bool,
    /// 中文按拼音排（做不到，这里退化为字典序）；true 时大小写不敏感
    pub ignore_case: bool,
}

/// 比较两个格子（空值永远排最后）
fn cmp_cell(a: &Option<Cell>, b: &Option<Cell>, ignore_case: bool) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let av = a.as_ref().map(|c| c.value.clone()).unwrap_or(Value::Empty);
    let bv = b.as_ref().map(|c| c.value.clone()).unwrap_or(Value::Empty);
    match (&av, &bv) {
        (Value::Empty, Value::Empty) => Ordering::Equal,
        (Value::Empty, _) => Ordering::Greater,
        (_, Value::Empty) => Ordering::Less,
        (Value::Number(x), Value::Number(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        (Value::Number(_), _) => Ordering::Less,
        (_, Value::Number(_)) => Ordering::Greater,
        _ => {
            let (x, y) = (av.display(), bv.display());
            if ignore_case {
                x.to_lowercase().cmp(&y.to_lowercase())
            } else {
                x.cmp(&y)
            }
        }
    }
}

/// 多级排序；返回实际移动的行数
pub fn sort_multi(
    sheet: &mut Sheet,
    region: (u32, u32, u32, u32),
    keys: &[SortKey],
    header: bool,
) -> usize {
    let (r1, c1, r2, c2) = region;
    let r1 = if header { r1 + 1 } else { r1 };
    if r2 <= r1 || keys.is_empty() {
        return 0;
    }
    // 整块备份（原样，含样式）
    let mut rows: Vec<(u32, Vec<(Cell, u16)>)> = Vec::new();
    for r in r1..=r2 {
        let mut line = Vec::new();
        for c in c1..=c2 {
            let cell = sheet.get(r, c).cloned().unwrap_or(Cell {
                value: Value::Empty,
                formula: None,
            });
            line.push((cell, sheet.style_of(r, c)));
        }
        rows.push((r, line));
    }
    let before: Vec<Vec<(Cell, u16)>> = rows.iter().map(|(_, l)| l.clone()).collect();
    let mut order: Vec<usize> = (0..rows.len()).collect();
    order.sort_by(|&i, &j| {
        for k in keys {
            let ci = (k.col - c1) as usize;
            if ci >= before[i].len() {
                continue;
            }
            let a = Some(before[i][ci].0.clone());
            let b = Some(before[j][ci].0.clone());
            let mut ord = cmp_cell(&a, &b, k.ignore_case);
            if !k.asc {
                ord = ord.reverse();
            }
            // 空值不参与反向：空永远最后
            let a_empty = matches!(before[i][ci].0.value, Value::Empty);
            let b_empty = matches!(before[j][ci].0.value, Value::Empty);
            if a_empty != b_empty {
                ord = if a_empty {
                    std::cmp::Ordering::Greater
                } else {
                    std::cmp::Ordering::Less
                };
            }
            if ord != std::cmp::Ordering::Equal {
                return ord;
            }
        }
        std::cmp::Ordering::Equal
    });
    let mut moved = 0;
    for (target_r, &src_i) in order.iter().enumerate() {
        let r = r1 + target_r as u32;
        for (k, (cell, st)) in before[src_i].iter().enumerate() {
            let c = c1 + k as u32;
            sheet.set(r, c, cell.clone());
            sheet.set_style(r, c, *st);
        }
        if order[target_r] != target_r {
            moved += 1;
        }
    }
    let _ = c2;
    moved
}

/// 删除重复行（按指定列判断），保留第一次出现；返回删除的行数
pub fn remove_duplicates(
    sheet: &mut Sheet,
    region: (u32, u32, u32, u32),
    cols: &[u32],
    header: bool,
) -> usize {
    let (r1, c1, r2, c2) = region;
    let r1 = if header && r2 > r1 { r1 + 1 } else { r1 };
    if r2 < r1 {
        return 0;
    }
    let use_cols: Vec<u32> = if cols.is_empty() {
        (c1..=c2).collect()
    } else {
        cols.to_vec()
    };
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut keep: Vec<u32> = Vec::new();
    for r in r1..=r2 {
        let key: String = use_cols
            .iter()
            .map(|c| sheet.get(r, *c).map(|x| x.value.display()).unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\u{1}");
        if seen.insert(key) {
            keep.push(r);
        }
    }
    let removed = (r2 - r1 + 1) as usize - keep.len();
    if removed == 0 {
        return 0;
    }
    // 把保留的行依次搬到顶部，再清掉尾部
    let mut snapshot: Vec<Vec<(Cell, u16)>> = Vec::new();
    for r in keep.iter() {
        let mut line = Vec::new();
        for c in c1..=c2 {
            let cell = sheet.get(*r, c).cloned().unwrap_or(Cell {
                value: Value::Empty,
                formula: None,
            });
            line.push((cell, sheet.style_of(*r, c)));
        }
        snapshot.push(line);
    }
    for (i, line) in snapshot.iter().enumerate() {
        let r = r1 + i as u32;
        for (k, (cell, st)) in line.iter().enumerate() {
            let c = c1 + k as u32;
            sheet.set(r, c, cell.clone());
            sheet.set_style(r, c, *st);
        }
    }
    for r in r1 + snapshot.len() as u32..=r2 {
        for c in c1..=c2 {
            sheet.set(r, c, Cell { value: Value::Empty, formula: None });
        }
    }
    removed
}

/// 分列：按分隔符把每格拆到右边相邻列（右侧内容整体右移腾位置）
pub fn text_to_columns(sheet: &mut Sheet, region: (u32, u32, u32, u32), delim: char) -> usize {
    let (r1, c1, r2, c2) = region;
    let mut splits: Vec<(u32, Vec<String>)> = Vec::new();
    let mut max_parts = 1usize;
    for r in r1..=r2 {
        let text = sheet.get(r, c1).map(|c| c.value.display()).unwrap_or_default();
        let parts: Vec<String> = text.split(delim).map(|s| s.trim().to_string()).collect();
        max_parts = max_parts.max(parts.len());
        splits.push((r, parts));
    }
    if max_parts <= 1 {
        return 0;
    }
    // 右侧整体右移 (max_parts - 1) 列，避免覆盖
    let shift = (max_parts - 1) as u32;
    let mut moved: Vec<(u32, u32, Cell, u16)> = Vec::new();
    for r in r1..=r2 {
        for c in (c1 + 1..=c2).rev() {
            if let Some(cell) = sheet.get(r, c).cloned() {
                moved.push((r, c + shift, cell, sheet.style_of(r, c)));
            }
        }
    }
    for r in r1..=r2 {
        for c in c1 + 1..=c2 + shift {
            sheet.set(r, c, Cell { value: Value::Empty, formula: None });
        }
    }
    for (r, c, cell, st) in moved {
        sheet.set(r, c, cell);
        sheet.set_style(r, c, st);
    }
    for (r, parts) in splits {
        for (i, part) in parts.iter().enumerate() {
            sheet.set(
                r,
                c1 + i as u32,
                Cell { value: Value::parse_input(part), formula: None },
            );
        }
    }
    max_parts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sheet_with(rows: &[&[&str]]) -> Sheet {
        let mut s = Sheet::new("T");
        for (r, row) in rows.iter().enumerate() {
            for (c, v) in row.iter().enumerate() {
                s.set(
                    r as u32,
                    c as u32,
                    Cell { value: Value::parse_input(v), formula: None },
                );
            }
        }
        s
    }

    fn col(s: &Sheet, c: u32, r1: u32, r2: u32) -> Vec<String> {
        (r1..=r2)
            .map(|r| s.get(r, c).map(|x| x.value.display()).unwrap_or_default())
            .collect()
    }

    #[test]
    fn multi_level_sort() {
        let mut s = sheet_with(&[
            &["部门", "姓名", "金额"],
            &["研发", "甲", "300"],
            &["销售", "乙", "100"],
            &["研发", "丙", "200"],
        ]);
        let moved = sort_multi(
            &mut s,
            (0, 0, 3, 2),
            &[
                SortKey { col: 0, asc: true, ignore_case: true },
                SortKey { col: 2, asc: false, ignore_case: false },
            ],
            true,
        );
        assert!(moved > 0);
        assert_eq!(col(&s, 0, 1, 3), vec!["研发", "研发", "销售"]);
        // 同部门内按金额降序
        assert_eq!(col(&s, 2, 1, 3), vec!["300", "200", "100"]);
    }

    #[test]
    fn sort_puts_empty_last() {
        let mut s = sheet_with(&[&["3"], &[""], &["1"]]);
        sort_multi(&mut s, (0, 0, 2, 0), &[SortKey { col: 0, asc: true, ignore_case: true }], false);
        assert_eq!(col(&s, 0, 0, 2), vec!["1", "3", ""]);
    }

    #[test]
    fn remove_duplicates_keeps_first() {
        let mut s = sheet_with(&[
            &["a", "1"],
            &["b", "2"],
            &["a", "1"],
            &["c", "3"],
            &["a", "1"],
        ]);
        let removed = remove_duplicates(&mut s, (0, 0, 4, 1), &[0, 1], false);
        assert_eq!(removed, 2);
        assert_eq!(col(&s, 0, 0, 2), vec!["a", "b", "c"]);
        assert_eq!(col(&s, 0, 3, 4), vec!["", ""]);
    }

    #[test]
    fn text_to_columns_splits_and_shifts() {
        let mut s = sheet_with(&[&["甲,100", "备注A"], &["乙,200", "备注B"]]);
        let n = text_to_columns(&mut s, (0, 0, 1, 1), ',');
        assert_eq!(n, 2);
        assert_eq!(col(&s, 0, 0, 1), vec!["甲", "乙"]);
        assert_eq!(col(&s, 1, 0, 1), vec!["100", "200"]);
        // 原来的备注被推到右边
        assert_eq!(col(&s, 2, 0, 1), vec!["备注A", "备注B"]);
    }
}
