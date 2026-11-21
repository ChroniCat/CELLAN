//! 合并计算：把多张表（或多个区域）按行/列标签汇总到一张表里。
//!
//! 口径照 Excel 的「合并计算」：
//! - 勾了「首行」：每个源区域的第一行是列标签，按标签对齐列
//! - 勾了「最左列」：第一列是行标签，按标签对齐行
//! - 都不勾：按位置（各源左上角对齐）直接相加
//! - 函数支持 求和 / 计数 / 平均值 / 最大值 / 最小值

use crate::model::{Cell, Sheet, Value, Workbook};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ConsFn {
    #[default]
    Sum,
    Count,
    Average,
    Max,
    Min,
}

impl ConsFn {
    pub fn label(self) -> &'static str {
        match self {
            ConsFn::Sum => "求和",
            ConsFn::Count => "计数",
            ConsFn::Average => "平均值",
            ConsFn::Max => "最大值",
            ConsFn::Min => "最小值",
        }
    }
    fn fold(self, vals: &[f64]) -> f64 {
        if vals.is_empty() {
            return 0.0;
        }
        match self {
            ConsFn::Sum => vals.iter().sum(),
            ConsFn::Count => vals.len() as f64,
            ConsFn::Average => vals.iter().sum::<f64>() / vals.len() as f64,
            ConsFn::Max => vals.iter().cloned().fold(f64::MIN, f64::max),
            ConsFn::Min => vals.iter().cloned().fold(f64::MAX, f64::min),
        }
    }
}

/// 一个来源区域
#[derive(Clone, Debug)]
pub struct Source {
    pub sheet: String,
    pub rect: (u32, u32, u32, u32),
}

#[derive(Clone, Debug, Default)]
pub struct ConsReport {
    pub rows: usize,
    pub cols: usize,
}

/// 把 `sources` 汇总到 `target_sheet` 的 `at` 位置。
pub fn consolidate(
    wb: &mut Workbook,
    target_sheet: usize,
    at: (u32, u32),
    sources: &[Source],
    f: ConsFn,
    row_labels: bool,
    col_labels: bool,
) -> Result<ConsReport, String> {
    if sources.is_empty() {
        return Err("至少给一个引用位置".into());
    }
    // 收集每个源的数值网格
    struct Grid {
        row_map: std::collections::HashMap<String, usize>,
        col_map: std::collections::HashMap<String, usize>,
        row_labels: Vec<String>,
        col_labels: Vec<String>,
        vals: Vec<Vec<Vec<f64>>>,
    }
    let mut grids: Vec<Grid> = Vec::new();
    for s in sources {
        let Some(sh) = wb.sheets.iter().find(|x| x.name == s.sheet) else {
            return Err(format!("没有工作表「{}」", s.sheet));
        };
        let (r1, c1, mut r2, mut c2) = s.rect;
        if (r2.saturating_sub(r1) as u64 + 1) * (c2.saturating_sub(c1) as u64 + 1) > 200_000 {
            let Some((_, _, ur2, uc2)) = sh.used_bounds() else {
                return Err(format!("区域 {} 没有有效数据", s.sheet));
            };
            r2 = r2.min(ur2);
            c2 = c2.min(uc2);
        }
        let (dr, dc) = (
            if row_labels { r1 + 1 } else { r1 },
            if col_labels { c1 + 1 } else { c1 },
        );
        let rows = if r2 >= dr { r2 - dr + 1 } else { 0 };
        let cols = if c2 >= dc { c2 - dc + 1 } else { 0 };
        if rows == 0 || cols == 0 {
            return Err(format!(
                "区域 {} 太小（勾了标签时至少要有标签行/列）",
                s.sheet
            ));
        }
        if (rows as u64) * (cols as u64) > 500_000 {
            return Err(format!("区域 {} 过大（超过 50 万单元格），请缩小选区范围", s.sheet));
        }
        let mut g = Grid {
            row_map: std::collections::HashMap::with_capacity(rows as usize),
            col_map: std::collections::HashMap::with_capacity(cols as usize),
            row_labels: Vec::with_capacity(rows as usize),
            col_labels: Vec::with_capacity(cols as usize),
            vals: vec![vec![Vec::new(); cols as usize]; rows as usize],
        };
        for i in 0..rows {
            let lbl = if row_labels {
                sh.value(dr + i, c1).display()
            } else {
                format!("第{}行", i + 1)
            };
            g.row_map.entry(lbl.clone()).or_insert(i as usize);
            g.row_labels.push(lbl);
        }
        for j in 0..cols {
            let lbl = if col_labels {
                sh.value(r1, dc + j).display()
            } else {
                format!("第{}列", j + 1)
            };
            g.col_map.entry(lbl.clone()).or_insert(j as usize);
            g.col_labels.push(lbl);
        }
        for i in 0..rows {
            for j in 0..cols {
                if let Value::Number(n) = sh.value(dr + i, dc + j) {
                    let ri = *g.row_map.get(&g.row_labels[i as usize]).unwrap_or(&(i as usize));
                    let cj = *g.col_map.get(&g.col_labels[j as usize]).unwrap_or(&(j as usize));
                    g.vals[ri][cj].push(n);
                }
            }
        }
        grids.push(g);
    }
    // 汇总标签（保持出现顺序，O(1) 哈希去重替代 O(N^2) Vec::contains）
    let mut rows: Vec<String> = Vec::new();
    let mut cols: Vec<String> = Vec::new();
    let mut seen_rows: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut seen_cols: std::collections::HashSet<String> = std::collections::HashSet::new();
    for g in &grids {
        for r in &g.row_labels {
            if seen_rows.insert(r.clone()) {
                rows.push(r.clone());
            }
        }
        for c in &g.col_labels {
            if seen_cols.insert(c.clone()) {
                cols.push(c.clone());
            }
        }
    }
    if (rows.len() as u64) * (cols.len() as u64) > 500_000 {
        return Err("合并计算结果矩阵超过 50 万单元格上限".into());
    }
    // 写结果：左上角标签 + 表头 + 数据
    let (ar, ac) = at;
    let out = wb.sheet_mut(target_sheet);
    if col_labels {
        out.set(
            ar,
            ac,
            Cell {
                value: Value::Text(if row_labels { "汇总" } else { "" }.into()),
                formula: None,
            },
        );
        for (j, c) in cols.iter().enumerate() {
            out.set(
                ar,
                ac + 1 + j as u32,
                Cell {
                    value: Value::Text(c.clone().into()),
                    formula: None,
                },
            );
        }
    }
    for (i, r) in rows.iter().enumerate() {
        let rr = ar + 1 + i as u32;
        if row_labels {
            out.set(
                rr,
                ac,
                Cell {
                    value: Value::Text(r.clone().into()),
                    formula: None,
                },
            );
        }
        for (j, c) in cols.iter().enumerate() {
            let mut vals: Vec<f64> = Vec::new();
            for g in &grids {
                let Some(&ri) = g.row_map.get(r) else {
                    continue;
                };
                let Some(&cj) = g.col_map.get(c) else {
                    continue;
                };
                vals.extend(g.vals[ri][cj].iter().copied());
            }
            let v = f.fold(&vals);
            out.set(
                rr,
                ac + 1 + j as u32,
                Cell {
                    value: Value::Number(v),
                    formula: None,
                },
            );
        }
    }
    Ok(ConsReport {
        rows: rows.len(),
        cols: cols.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wb_two_sheets() -> Workbook {
        let mut wb = Workbook::new_blank();
        let mut a = Sheet::new("一月");
        a.set(0, 0, Cell { value: Value::Text("地区".into()), formula: None });
        a.set(0, 1, Cell { value: Value::Text("销售额".into()), formula: None });
        a.set(1, 0, Cell { value: Value::Text("北京".into()), formula: None });
        a.set(1, 1, Cell { value: Value::Number(100.0), formula: None });
        a.set(2, 0, Cell { value: Value::Text("上海".into()), formula: None });
        a.set(2, 1, Cell { value: Value::Number(50.0), formula: None });
        let mut b = Sheet::new("二月");
        b.set(0, 0, Cell { value: Value::Text("地区".into()), formula: None });
        b.set(0, 1, Cell { value: Value::Text("销售额".into()), formula: None });
        b.set(1, 0, Cell { value: Value::Text("上海".into()), formula: None });
        b.set(1, 1, Cell { value: Value::Number(70.0), formula: None });
        b.set(2, 0, Cell { value: Value::Text("广州".into()), formula: None });
        b.set(2, 1, Cell { value: Value::Number(30.0), formula: None });
        wb.sheets = vec![a, b, Sheet::new("汇总")];
        wb
    }

    #[test]
    fn sums_by_row_label() {
        let mut wb = wb_two_sheets();
        let src = vec![
            Source { sheet: "一月".into(), rect: (0, 0, 2, 1) },
            Source { sheet: "二月".into(), rect: (0, 0, 2, 1) },
        ];
        let rep = consolidate(&mut wb, 2, (0, 0), &src, ConsFn::Sum, true, true).unwrap();
        assert_eq!(rep.rows, 3, "北京/上海/广州");
        let out = &wb.sheets[2];
        assert_eq!(out.value(1, 0).display(), "北京");
        assert_eq!(out.value(1, 1).display(), "100");
        assert_eq!(out.value(2, 0).display(), "上海");
        assert_eq!(out.value(2, 1).display(), "120", "50 + 70");
        assert_eq!(out.value(3, 0).display(), "广州");
        assert_eq!(out.value(3, 1).display(), "30");
    }

    #[test]
    fn average_and_count() {
        let mut wb = wb_two_sheets();
        let src = vec![
            Source { sheet: "一月".into(), rect: (0, 0, 2, 1) },
            Source { sheet: "二月".into(), rect: (0, 0, 2, 1) },
        ];
        consolidate(&mut wb, 2, (0, 0), &src, ConsFn::Average, true, true).unwrap();
        assert_eq!(wb.sheets[2].value(2, 1).display(), "60", "(50+70)/2");
        let mut wb2 = wb_two_sheets();
        consolidate(&mut wb2, 2, (0, 0), &src, ConsFn::Count, true, true).unwrap();
        assert_eq!(wb2.sheets[2].value(2, 1).display(), "2");
    }

    #[test]
    fn unknown_sheet_is_reported() {
        let mut wb = wb_two_sheets();
        let src = vec![Source { sheet: "三月".into(), rect: (0, 0, 2, 1) }];
        let err = consolidate(&mut wb, 2, (0, 0), &src, ConsFn::Sum, true, true).unwrap_err();
        assert!(err.contains("三月"), "{err}");
    }

    #[test]
    fn position_mode_aligns_by_offset() {
        let mut wb = wb_two_sheets();
        let src = vec![
            Source { sheet: "一月".into(), rect: (1, 1, 2, 1) },
            Source { sheet: "二月".into(), rect: (1, 1, 2, 1) },
        ];
        // 不勾标签：按位置相加 100+70、50+30
        let rep = consolidate(&mut wb, 2, (0, 0), &src, ConsFn::Sum, false, false).unwrap();
        assert_eq!((rep.rows, rep.cols), (2, 1));
        assert_eq!(wb.sheets[2].value(1, 1).display(), "170");
        assert_eq!(wb.sheets[2].value(2, 1).display(), "80");
    }
}
