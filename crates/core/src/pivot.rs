//! 数据透视表：按行字段（可选列字段）分组，对值字段做汇总，结果写成一块普通单元格。
//!
//! 说明：这里生成的是「值化」的透视结果（Excel 里叫「粘贴为值」），
//! 不会创建 Excel 的 pivotTable 对象，所以导出的文件在任何工具里都能看到数字，
//! 但不能在 Excel 里拖字段重组。这是有意的取舍：真 pivotTable 需要
//! pivotCacheDefinition + pivotTableDefinition 两套部件，投入产出不划算。

use crate::model::{Cell, Sheet, Value};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Agg {
    Sum,
    Count,
    Avg,
    Max,
    Min,
}

impl Agg {
    pub fn label(&self) -> &'static str {
        match self {
            Agg::Sum => "求和",
            Agg::Count => "计数",
            Agg::Avg => "平均值",
            Agg::Max => "最大值",
            Agg::Min => "最小值",
        }
    }
}

/// 行字段的分组方式
#[derive(Clone, Debug, PartialEq)]
pub enum GroupBy {
    None,
    /// 日期按年
    Year,
    /// 日期按月（yyyy-mm）
    Month,
    /// 数值按区间（桶宽）
    Bucket(f64),
}

impl Default for GroupBy {
    fn default() -> Self {
        GroupBy::None
    }
}

impl GroupBy {
    pub fn label(&self) -> String {
        match self {
            GroupBy::None => "不分组".into(),
            GroupBy::Year => "按年".into(),
            GroupBy::Month => "按月".into(),
            GroupBy::Bucket(n) => format!("每 {n} 一档"),
        }
    }
}

#[derive(Clone, Debug)]
pub struct PivotSpec {
    /// 源区域（含表头行）
    pub source: (u32, u32, u32, u32),
    /// 行字段（列号）
    pub row_field: u32,
    /// 列字段（列号），None 表示只有行
    pub col_field: Option<u32>,
    /// 值字段（列号）
    pub value_field: u32,
    pub agg: Agg,
    /// 行字段的分组
    pub group: GroupBy,
    /// 排除的行标签（分组后的字符串）
    pub exclude: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct PivotResult {
    /// 表头：第一列是行字段名，其余是列字段的取值
    pub headers: Vec<String>,
    /// 数据行：第一格是行标签，其余是汇总值
    pub rows: Vec<Vec<Value>>,
    /// 行字段名
    pub row_name: String,
    pub value_name: String,
}

/// 计算透视结果
/// 把行字段的原始值按分组方式变成标签
pub fn group_key(v: &Value, g: &GroupBy) -> String {
    match g {
        GroupBy::None => v.display(),
        GroupBy::Year | GroupBy::Month => {
            let (y, m) = match v {
                Value::Number(serial) => match crate::formula::serial_to_date_pub(*serial) {
                    Some((y, m, _)) => (y, m),
                    None => return v.display(),
                },
                Value::Text(t) => {
                    let t = t.trim();
                    let mut it = t.split(['-', '/', '年', '月']);
                    let y = it.next().and_then(|s| s.parse::<i32>().ok());
                    let m = it.next().and_then(|s| s.parse::<u32>().ok());
                    match (y, m) {
                        (Some(y), Some(m)) => (y, m),
                        _ => return v.display(),
                    }
                }
                _ => return v.display(),
            };
            if matches!(g, GroupBy::Year) {
                format!("{y}")
            } else {
                format!("{y}-{m:02}")
            }
        }
        GroupBy::Bucket(w) => {
            let n = match v {
                Value::Number(n) => *n,
                Value::Text(t) => t.trim().parse::<f64>().unwrap_or(0.0),
                _ => 0.0,
            };
            if *w <= 0.0 {
                return v.display();
            }
            let b = (n / w).floor() * w;
            let show = |v: f64| {
                if v.fract().abs() < 1e-9 {
                    format!("{}", v as i64)
                } else {
                    format!("{v}")
                }
            };
            format!("{}~{}", show(b), show(b + w))
        }
    }
}

pub fn compute(sheet: &Sheet, spec: &PivotSpec) -> PivotResult {
    let (r1, c1, r2, c2) = spec.source;
    let header = |c: u32| {
        sheet
            .get(r1, c)
            .map(|x| x.value.display())
            .unwrap_or_else(|| crate::model::col_label(c))
    };
    let row_name = header(spec.row_field);
    let value_name = header(spec.value_field);

    // 收集：行标签 → (列标签 → 值列表)
    let mut data: BTreeMap<String, BTreeMap<String, Vec<f64>>> = BTreeMap::new();
    for r in (r1 + 1)..=r2 {
        let raw = sheet
            .get(r, spec.row_field)
            .map(|x| x.value.clone())
            .unwrap_or(Value::Empty);
        let key = group_key(&raw, &spec.group);
        if key.is_empty() {
            continue;
        }
        if spec.exclude.iter().any(|e| e == &key) {
            continue;
        }
        let col_key = match spec.col_field {
            Some(cc) => sheet.get(r, cc).map(|x| x.value.display()).unwrap_or_default(),
            None => value_name.clone(),
        };
        let v = sheet.get(r, spec.value_field).map(|x| x.value.clone());
        let n = match v {
            Some(Value::Number(n)) => Some(n),
            Some(other) => other.display().parse::<f64>().ok(),
            None => None,
        };
        let entry = data.entry(key).or_default().entry(col_key).or_default();
        if let Some(n) = n {
            entry.push(n);
        } else if spec.agg == Agg::Count {
            entry.push(0.0);
        }
    }

    let mut col_keys: Vec<String> = Vec::new();
    for m in data.values() {
        for k in m.keys() {
            if !col_keys.contains(k) {
                col_keys.push(k.clone());
            }
        }
    }
    col_keys.sort_by(|a, b| match (a.parse::<f64>(), b.parse::<f64>()) {
        (Ok(x), Ok(y)) => x.partial_cmp(&y).unwrap_or(std::cmp::Ordering::Equal),
        _ => a.cmp(b),
    });

    let mut headers = vec![row_name.clone()];
    headers.extend(col_keys.iter().cloned());

    let mut rows = Vec::new();
    let mut grand: Vec<Vec<f64>> = vec![Vec::new(); col_keys.len()];
    for (key, m) in data.iter() {
        let mut line = vec![Value::Text(key.as_str().into())];
        for (i, ck) in col_keys.iter().enumerate() {
            let vals = m.get(ck).cloned().unwrap_or_default();
            grand[i].extend(vals.iter().copied());
            line.push(agg_value(&vals, spec.agg));
        }
        rows.push(line);
    }
    // 合计行
    if rows.len() > 1 {
        let mut total = vec![Value::Text("总计".into())];
        for i in 0..col_keys.len() {
            total.push(agg_value(&grand[i], spec.agg));
        }
        rows.push(total);
    }

    PivotResult { headers, rows, row_name, value_name }
}

fn agg_value(vals: &[f64], agg: Agg) -> Value {
    if vals.is_empty() {
        return Value::Empty;
    }
    match agg {
        Agg::Sum => Value::Number(vals.iter().sum()),
        Agg::Count => Value::Number(vals.len() as f64),
        Agg::Avg => Value::Number(vals.iter().sum::<f64>() / vals.len() as f64),
        Agg::Max => Value::Number(vals.iter().cloned().fold(f64::NEG_INFINITY, f64::max)),
        Agg::Min => Value::Number(vals.iter().cloned().fold(f64::INFINITY, f64::min)),
    }
}

/// 把结果写到指定位置，返回占用的矩形
pub fn write_to(sheet: &mut Sheet, at: (u32, u32), res: &PivotResult) -> (u32, u32, u32, u32) {
    let (r0, c0) = at;
    for (i, h) in res.headers.iter().enumerate() {
        sheet.set(
            r0,
            c0 + i as u32,
            Cell { value: Value::Text(h.as_str().into()), formula: None },
        );
    }
    for (ri, line) in res.rows.iter().enumerate() {
        for (ci, v) in line.iter().enumerate() {
            sheet.set(r0 + 1 + ri as u32, c0 + ci as u32, Cell { value: v.clone(), formula: None });
        }
    }
    (
        r0,
        c0,
        r0 + res.rows.len() as u32,
        c0 + res.headers.len().saturating_sub(1) as u32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sales() -> Sheet {
        let mut s = Sheet::new("销售");
        for (i, h) in ["部门", "月份", "金额"].iter().enumerate() {
            s.set(0, i as u32, Cell { value: Value::Text((*h).into()), formula: None });
        }
        let rows = [
            ("研发", "1月", 100.0),
            ("研发", "2月", 150.0),
            ("销售", "1月", 200.0),
            ("销售", "2月", 50.0),
            ("研发", "1月", 20.0),
        ];
        for (i, (d, m, v)) in rows.iter().enumerate() {
            let r = i as u32 + 1;
            s.set(r, 0, Cell { value: Value::Text((*d).into()), formula: None });
            s.set(r, 1, Cell { value: Value::Text((*m).into()), formula: None });
            s.set(r, 2, Cell { value: Value::Number(*v), formula: None });
        }
        s
    }

    #[test]
    fn pivot_rows_only() {
        let s = sales();
        let spec = PivotSpec {
            source: (0, 0, 5, 2),
            row_field: 0,
            col_field: None,
            value_field: 2,
            agg: Agg::Sum,
            group: GroupBy::None,
            exclude: Vec::new(),
        };
        let res = compute(&s, &spec);
        assert_eq!(res.headers, vec!["部门", "金额"]);
        // BTreeMap 按字符串排序：研发 < 销售
        assert_eq!(res.rows[0][0], Value::Text("研发".into()));
        assert_eq!(res.rows[0][1], Value::Number(270.0));
        assert_eq!(res.rows[1][0], Value::Text("销售".into()));
        assert_eq!(res.rows[1][1], Value::Number(250.0));
        assert_eq!(res.rows[2][0], Value::Text("总计".into()));
        assert_eq!(res.rows[2][1], Value::Number(520.0));
    }

    #[test]
    fn pivot_with_column_field() {
        let s = sales();
        let spec = PivotSpec {
            source: (0, 0, 5, 2),
            row_field: 0,
            col_field: Some(1),
            value_field: 2,
            agg: Agg::Sum,
            group: GroupBy::None,
            exclude: Vec::new(),
        };
        let res = compute(&s, &spec);
        assert_eq!(res.headers, vec!["部门", "1月", "2月"]);
        assert_eq!(res.rows[0][1], Value::Number(120.0), "研发 1月 100+20");
        assert_eq!(res.rows[0][2], Value::Number(150.0));
        assert_eq!(res.rows[1][1], Value::Number(200.0));
        assert_eq!(res.rows[1][2], Value::Number(50.0));
    }

    #[test]
    fn other_aggregations() {
        let s = sales();
        let mk = |agg| PivotSpec {
            source: (0, 0, 5, 2),
            row_field: 0,
            col_field: None,
            value_field: 2,
            agg,
            group: GroupBy::None,
            exclude: Vec::new(),
};
        assert_eq!(compute(&s, &mk(Agg::Count)).rows[0][1], Value::Number(3.0));
        assert_eq!(compute(&s, &mk(Agg::Max)).rows[0][1], Value::Number(150.0));
        assert_eq!(compute(&s, &mk(Agg::Min)).rows[0][1], Value::Number(20.0));
        let avg = compute(&s, &mk(Agg::Avg));
        if let Value::Number(v) = avg.rows[0][1] {
            assert!((v - 90.0).abs() < 0.001, "(100+150+20)/3 = 90，实际 {v}");
        } else {
            panic!("平均值类型不对");
        }
    }

    #[test]
    fn writes_block() {
        let mut s = sales();
        let spec = PivotSpec {
            source: (0, 0, 5, 2),
            row_field: 0,
            col_field: None,
            value_field: 2,
            agg: Agg::Sum,
            group: GroupBy::None,
            exclude: Vec::new(),
        };
        let res = compute(&s, &spec);
        let rect = write_to(&mut s, (8, 0), &res);
        assert_eq!(rect, (8, 0, 11, 1));
        assert_eq!(s.get(8, 0).unwrap().value.display(), "部门");
        assert_eq!(s.get(9, 0).unwrap().value.display(), "研发");
        assert_eq!(s.get(9, 1).unwrap().value.display(), "270");
    }
}

#[cfg(test)]
mod protection_extra_tests {
    use super::*;
    use crate::model::{Cell, Sheet, Value};

    #[test]
    fn groups_text_dates_by_month_and_excludes() {
        let mut sh = Sheet::new("t");
        sh.set(0, 0, Cell { value: Value::Text("date".into()), formula: None });
        sh.set(0, 1, Cell { value: Value::Text("amount".into()), formula: None });
        for (i, (d, a)) in [("2024-01-05", 100.0), ("2024-01-20", 50.0), ("2024-02-02", 30.0)]
            .iter()
            .enumerate()
        {
            sh.set(i as u32 + 1, 0, Cell { value: Value::Text((*d).into()), formula: None });
            sh.set(i as u32 + 1, 1, Cell { value: Value::Number(*a), formula: None });
        }
        let spec = PivotSpec {
            source: (0, 0, 3, 1),
            row_field: 0,
            col_field: None,
            value_field: 1,
            agg: Agg::Sum,
            group: GroupBy::Month,
            exclude: vec!["2024-02".into()],
        };
        let r = compute(&sh, &spec);
        assert_eq!(r.rows.len(), 1);
        assert_eq!(r.rows[0][0].display(), "2024-01");
        assert_eq!(r.rows[0][1].display(), "150");
    }

    #[test]
    fn bucket_group_labels_ranges() {
        let mut sh = Sheet::new("t");
        sh.set(0, 0, Cell { value: Value::Text("v".into()), formula: None });
        sh.set(0, 1, Cell { value: Value::Text("n".into()), formula: None });
        for (i, n) in [5.0, 42.0, 120.0].iter().enumerate() {
            sh.set(i as u32 + 1, 0, Cell { value: Value::Number(*n), formula: None });
            sh.set(i as u32 + 1, 1, Cell { value: Value::Number(1.0), formula: None });
        }
        let spec = PivotSpec {
            source: (0, 0, 3, 1),
            row_field: 0,
            col_field: None,
            value_field: 1,
            agg: Agg::Count,
            group: GroupBy::Bucket(50.0),
            exclude: Vec::new(),
        };
        let r = compute(&sh, &spec);
        let labels: Vec<String> = r.rows.iter().map(|row| row[0].display()).collect();
        assert_eq!(&labels[..2], &["0~50", "100~150"]);
    }
}
