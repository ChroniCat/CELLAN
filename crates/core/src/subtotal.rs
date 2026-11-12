//! 分类汇总：按某一列把**相邻的相同值**归成一组，每组下面插一行小计，末尾加总计。
//!
//! 和 Excel 一致的口径：
//! - 只对相邻同值分组（Excel 也要求先按该列排序，界面上会提示）
//! - 小计用 `SUBTOTAL(9/1/2/4/5, 区域)`，不是 SUM —— 这样以后再嵌套分类汇总
//!   或手动筛选时，小计不会把自己算进去
//! - 明细行打上分级显示级别 1，小计行是级别 2 的父级位置（汇总行在下方）
//! - 插入行复用 `Sheet::insert_rows`，所以条件格式/数据验证/批注会跟着一起移动

use crate::model::{Cell, Sheet, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SubFn {
    #[default]
    Sum,
    Average,
    Count,
    Max,
    Min,
}

impl SubFn {
    /// SUBTOTAL 的函数号
    pub fn code(self) -> u8 {
        match self {
            SubFn::Average => 1,
            SubFn::Count => 2,
            SubFn::Max => 4,
            SubFn::Min => 5,
            SubFn::Sum => 9,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            SubFn::Sum => "求和",
            SubFn::Average => "平均值",
            SubFn::Count => "计数",
            SubFn::Max => "最大值",
            SubFn::Min => "最小值",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct SubtotalReport {
    /// 插了几组
    pub groups: usize,
    /// 插了几行（含总计行）
    pub rows: usize,
}

/// `region` 含表头行；`key_col` 分组列；`value_cols` 要汇总的列。
/// 返回插入的组数与行数。
///
/// 做法分两趟：先按原始坐标找出所有组，从下往上插空行（这样插入点以上的引用不会被推乱），
/// 再按「最终坐标」写小计与总计的 SUBTOTAL。总计**只引用各组的明细段**（多区域参数），
/// 不把上一条小计算进去——我们的公式引擎里 SUBTOTAL 目前不会跳过嵌套小计，这样写两边结果一致。
pub fn subtotal(
    sheet: &mut Sheet,
    region: (u32, u32, u32, u32),
    key_col: u32,
    value_cols: &[u32],
    f: SubFn,
) -> SubtotalReport {
    let (r1, c1, r2, c2) = region;
    if r2 <= r1 || key_col < c1 || key_col > c2 {
        return SubtotalReport::default();
    }
    // 扫描分组（原始坐标）
    let mut groups: Vec<(u32, u32)> = Vec::new();
    let mut start = r1 + 1;
    let mut cur = sheet.value(r1 + 1, key_col).display();
    for r in (r1 + 2)..=r2 {
        let v = sheet.value(r, key_col).display();
        if v != cur {
            groups.push((start, r - 1));
            start = r;
            cur = v;
        }
    }
    groups.push((start, r2));
    let n = groups.len();
    if n == 0 {
        return SubtotalReport::default();
    }
    // 第一趟：从下往上插空行
    let mut inserted = 0u32;
    for i in (0..n).rev() {
        // 插在「本组最后一条明细」的下一行；下面那些插入都在更靠下的位置，不影响这个下标
        let at = groups[i].1 + 1;
        sheet.insert_rows(at, 1);
        inserted += 1;
    }
    // 第二趟：按最终坐标写内容（组 i 上方有 i 行插入）
    let mut total_ranges: Vec<String> = Vec::new();
    for (i, (gs, ge)) in groups.iter().enumerate() {
        let shift = i as u32;
        let fgs = gs + shift;
        let fge = ge + shift;
        let sub_row = fge + 1;
        let name = sheet.value(fgs, key_col).display();
        sheet.set(
            sub_row,
            c1,
            Cell {
                value: Value::Text(format!("{name} 汇总").into()),
                formula: None,
            },
        );
        for c in value_cols.iter().copied() {
            if c < c1 || c > c2 {
                continue;
            }
            let range = format!(
                "{}{}:{}{}",
                crate::model::col_label(c),
                fgs + 1,
                crate::model::col_label(c),
                fge + 1
            );
            let formula = format!("=SUBTOTAL({},{range})", f.code());
            let val = match f {
                SubFn::Count => Value::Number((fge - fgs + 1) as f64),
                _ => Value::Empty,
            };
            sheet.set(
                sub_row,
                c,
                Cell {
                    value: val,
                    formula: Some(formula.into()),
                },
            );
            if c == *value_cols.first().unwrap_or(&c) {
                total_ranges.push(range);
            }
        }
        // 明细打 1 级（Excel 的 summaryBelow 约定：汇总行在明细下方）
        for r in fgs..=fge {
            sheet.row_levels.insert(r, 1);
        }
    }
    // 总计行
    let total_at = r2 + inserted + 1;
    sheet.insert_rows(total_at, 1);
    inserted += 1;
    sheet.set(
        total_at,
        c1,
        Cell {
            value: Value::Text("总计".into()),
            formula: None,
        },
    );
    let first_val_col = value_cols.first().copied();
    for c in value_cols.iter().copied() {
        if c < c1 || c > c2 {
            continue;
        }
        let args: Vec<String> = if Some(c) == first_val_col {
            // 第一汇总列：只算各组的明细段
            total_ranges.clone()
        } else {
            // 其余列用同一批行号换成自己的列
            total_ranges
                .iter()
                .map(|r| {
                    let (a, b) = r.split_once(':').unwrap_or((r.as_str(), r.as_str()));
                    let row_a: String = a.chars().filter(|ch| ch.is_ascii_digit()).collect();
                    let row_b: String = b.chars().filter(|ch| ch.is_ascii_digit()).collect();
                    format!(
                        "{}{}:{}{}",
                        crate::model::col_label(c),
                        row_a,
                        crate::model::col_label(c),
                        row_b
                    )
                })
                .collect()
        };
        let formula = format!("=SUBTOTAL({},{})", f.code(), args.join(","));
        sheet.set(
            total_at,
            c,
            Cell {
                value: Value::Empty,
                formula: Some(formula.into()),
            },
        );
    }
    sheet.edited = true;
    SubtotalReport {
        groups: n,
        rows: inserted as usize,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sheet_with(rows: &[(&str, f64)]) -> Sheet {
        let mut sh = Sheet::new("S");
        sh.set(0, 0, Cell { value: Value::Text("部门".into()), formula: None });
        sh.set(0, 1, Cell { value: Value::Text("金额".into()), formula: None });
        for (i, (k, v)) in rows.iter().enumerate() {
            let r = i as u32 + 1;
            sh.set(r, 0, Cell { value: Value::Text((*k).into()), formula: None });
            sh.set(r, 1, Cell { value: Value::Number(*v), formula: None });
        }
        sh
    }

    #[test]
    fn groups_and_inserts_summary_rows() {
        let mut sh = sheet_with(&[("甲", 10.0), ("甲", 5.0), ("乙", 7.0)]);
        let rep = subtotal(&mut sh, (0, 0, 3, 1), 0, &[1], SubFn::Sum);
        assert_eq!(rep.groups, 2, "两组");
        assert_eq!(rep.rows, 3, "两条小计 + 总计");
        // 行布局：表头 / 甲 / 甲 / 甲 汇总 / 乙 / 乙 汇总 / 总计
        assert_eq!(sh.value(3, 0).display(), "甲 汇总");
        assert_eq!(sh.value(5, 0).display(), "乙 汇总");
        assert_eq!(sh.value(6, 0).display(), "总计");
        let f = sh.get(3, 1).and_then(|c| c.formula.clone()).unwrap_or_default();
        assert_eq!(&*f, "=SUBTOTAL(9,B2:B3)");
        let f2 = sh.get(5, 1).and_then(|c| c.formula.clone()).unwrap_or_default();
        assert_eq!(&*f2, "=SUBTOTAL(9,B5:B5)", "乙的明细被推到第 5 行");
        // 总计只引用两组明细段，不含上一条小计
        let ft = sh.get(6, 1).and_then(|c| c.formula.clone()).unwrap_or_default();
        assert_eq!(&*ft, "=SUBTOTAL(9,B2:B3,B5:B5)");
        // 明细行变成 1 级
        assert_eq!(sh.row_levels.get(&1), Some(&1));
    }

    #[test]
    fn average_and_count_codes() {
        assert_eq!(SubFn::Average.code(), 1);
        assert_eq!(SubFn::Count.code(), 2);
        assert_eq!(SubFn::Sum.code(), 9);
        let mut sh = sheet_with(&[("甲", 10.0), ("甲", 20.0)]);
        subtotal(&mut sh, (0, 0, 2, 1), 0, &[1], SubFn::Average);
        let f = sh.get(3, 1).and_then(|c| c.formula.clone()).unwrap_or_default();
        assert!(f.starts_with("=SUBTOTAL(1,"), "{f}");
    }

    #[test]
    fn unsorted_groups_are_split_like_excel() {
        let mut sh = sheet_with(&[("甲", 1.0), ("乙", 2.0), ("甲", 3.0)]);
        let rep = subtotal(&mut sh, (0, 0, 3, 1), 0, &[1], SubFn::Sum);
        assert_eq!(rep.groups, 3, "不相邻的同值算不同组（和 Excel 一致）");
    }

    #[test]
    fn empty_or_bad_region_does_nothing() {
        let mut sh = sheet_with(&[("甲", 1.0)]);
        let rep = subtotal(&mut sh, (0, 0, 0, 1), 0, &[1], SubFn::Sum);
        assert_eq!(rep.groups, 0, "只有表头不分组");
        let rep2 = subtotal(&mut sh, (0, 0, 1, 1), 9, &[1], SubFn::Sum);
        assert_eq!(rep2.groups, 0, "key_col 不在区域内要拒绝");
    }
}
