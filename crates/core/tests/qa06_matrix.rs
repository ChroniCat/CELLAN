//! QA-06.1：错误 / 空值 / 日期矩阵，逐条对拍本机 Excel 16.0 与 WPS 12.0 的实算结果。
//!
//! 夹具与 `scripts/qa06_fixture.py` 完全一致（`work/qa06/cases.json` 是同一份输入）：
//! ```text
//! A1 空      A2 "文本"   A3 5      A4 TRUE    A5 #N/A
//! A6 =""     A7 0       A8 1.5    A9 45351   A10 "5"
//! A11 #DIV/0!  A12 45000   B1..B5 = 1,2,3,4,5
//! ```
//! `matrix.tsv` 由 `scripts/qa06_excel_probe.ps1`（Excel + WPS 实算）与
//! `scripts/qa06_merge.py` 生成，列顺序：group / formula / Excel 显示值 / Excel Value2 / WPS 显示值。
//!
//! 比较口径：错误码严格按显示值比对；布尔严格；数字按相对 1e-9（绝对值下限 1e-9）比较；
//! 其余（文本、空）按显示值严格比对。

use sheetcore::formula::value::FValue;
use sheetcore::formula::{self, ErrKind};
use sheetcore::{Cell, Value, Workbook};

fn book() -> Workbook {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "S".into();
    let s = &mut wb.sheets[0];
    let text = |v: &str| Cell {
        value: Value::Text(v.into()),
        formula: None,
    };
    let num = |v: f64| Cell {
        value: Value::Number(v),
        formula: None,
    };
    // A1 故意不设，留空
    s.set(1, 0, text("文本"));
    s.set(2, 0, num(5.0));
    s.set(3, 0, Cell {
        value: Value::Bool(true),
        formula: None,
    });
    s.set(4, 0, Cell {
        value: Value::Error(ErrKind::Na),
        formula: None,
    });
    s.set(5, 0, Cell {
        value: Value::Text("".into()),
        formula: Some("=\"\"".into()),
    });
    s.set(6, 0, num(0.0));
    s.set(7, 0, num(1.5));
    s.set(8, 0, num(45351.0));
    s.set(9, 0, text("5"));
    s.set(10, 0, Cell {
        value: Value::Error(ErrKind::Div0),
        formula: None,
    });
    s.set(11, 0, num(45000.0));
    for i in 0..5 {
        s.set(i, 1, num((i + 1) as f64));
    }
    // QA-06.3 追加的输入：查找表 D1:E5、现金流 F1:F6、文本 H1:H4
    for (i, k) in ["k1", "k2", "k3", "k4", "k5"].iter().enumerate() {
        s.set(i as u32, 3, text(k));
        s.set(i as u32, 4, num(10.0 * (i as f64 + 1.0)));
    }
    for (i, v) in [-1000.0, 300.0, 400.0, 500.0, 200.0, 100.0].iter().enumerate() {
        s.set(i as u32, 5, num(*v));
    }
    s.set(0, 7, text("  Hello World  "));
    s.set(1, 7, text("Sheet1"));
    s.set(2, 7, text("a,b,c"));
    s.set(3, 7, text("中文ABC123"));
    wb
}

fn render(v: &FValue) -> String {
    match v {
        FValue::Bool(true) => "TRUE".into(),
        FValue::Bool(false) => "FALSE".into(),
        FValue::Error(e) => match e {
            ErrKind::Na => "#N/A".into(),
            ErrKind::Value => "#VALUE!".into(),
            ErrKind::Div0 => "#DIV/0!".into(),
            ErrKind::Ref => "#REF!".into(),
            ErrKind::Name => "#NAME?".into(),
            ErrKind::Num => "#NUM!".into(),
            ErrKind::Null => "#NULL!".into(),
            other => format!("{other:?}"),
        },
        FValue::Text(s) => s.clone(),
        FValue::Number(n) => {
            if n.fract().abs() < f64::EPSILON && n.abs() < 1e15 {
                format!("{}", *n as i64)
            } else {
                format!("{n}")
            }
        }
        FValue::Empty => String::new(),
        other => other.display(),
    }
}

/// 迭代求根类函数（IRR/XIRR/RATE）Excel 自己也是数值解，两个实现收敛到同一点但末位不同，
/// 对这类函数用 1e-7 相对容差；其余按 1e-9。
fn rel_tol(f: &str) -> f64 {
    let u = f.to_uppercase();
    if u.contains("XIRR") || u.contains("IRR(") || u.contains("RATE(") {
        1e-7
    } else {
        1e-9
    }
}

#[test]
fn qa06_1_error_blank_date_matrix_matches_excel() {
    let tsv = include_str!("fixtures/qa06/matrix.tsv");
    let wb = book();
    let mut checked = 0usize;
    let mut per_group = std::collections::BTreeMap::<String, usize>::new();
    let mut diffs = Vec::new();

    for line in tsv.lines() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() < 3 {
            continue;
        }
        let (group, f, excel) = (cols[0], cols[1], cols[2].trim());
        // Excel 的 Text 是按单元格格式显示的（日期会显示成 "1/1/2025" 或 "########"，
        // 平均值会被四舍五入到 15 位有效数字），数字比较必须用 Value2。
        let excel_v2 = cols.get(3).map(|s| s.trim()).unwrap_or("");
        let got = formula::eval_formula(&wb, 0, f);
        checked += 1;
        *per_group.entry(group.to_string()).or_default() += 1;

        let ok = match &got {
            // 错误码、布尔按显示值严格比对
            FValue::Error(_) | FValue::Bool(_) => render(&got) == excel,
            FValue::Number(n) => {
                let cand = excel_v2
                    .parse::<f64>()
                    .ok()
                    .or_else(|| excel.parse::<f64>().ok());
                match cand {
                    Some(e) => (*n - e).abs() <= rel_tol(f) * e.abs().max(1.0),
                    None => false,
                }
            }
            other => render(other) == excel,
        };
        if !ok {
            diffs.push(format!(
                "[{group}] {f}: 本引擎 {:?}（{}） != Excel {:?}",
                render(&got),
                match &got {
                    FValue::Number(n) => format!("{n}"),
                    _ => "-".into(),
                },
                excel
            ));
        }
    }

    assert!(checked >= 50, "矩阵太小：{checked}");
    for g in ["err", "blank", "date"] {
        assert!(
            per_group.get(g).copied().unwrap_or(0) >= 15,
            "{g} 组不足 15 条：{per_group:?}"
        );
    }
    assert!(
        diffs.is_empty(),
        "与 Excel 不一致 {} 条：\n{}",
        diffs.len(),
        diffs.join("\n")
    );
}

#[test]
fn qa06_3_fin_lookup_text_matrix_matches_excel() {
    let tsv = include_str!("fixtures/qa06/matrix.tsv");
    let wb = book();
    let mut per_group = std::collections::BTreeMap::<String, usize>::new();
    let mut diffs = Vec::new();

    for line in tsv.lines() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() < 3 {
            continue;
        }
        let (group, f, excel) = (cols[0], cols[1], cols[2].trim());
        if !matches!(group, "fin" | "lookup" | "text") {
            continue;
        }
        let excel_v2 = cols.get(3).map(|s| s.trim()).unwrap_or("");
        let got = formula::eval_formula(&wb, 0, f);
        *per_group.entry(group.to_string()).or_default() += 1;

        let close = |n: f64, cand: Option<f64>| match cand {
            Some(e) => (n - e).abs() <= rel_tol(f) * e.abs().max(1.0),
            None => false,
        };
        let ok = match &got {
            FValue::Error(_) | FValue::Bool(_) => render(&got) == excel,
            FValue::Number(n) => {
                // 显示值可能带货币符号或千分位，优先用 Value2
                let cand = excel_v2
                    .parse::<f64>()
                    .ok()
                    .or_else(|| excel.parse::<f64>().ok());
                close(*n, cand)
            }
            other => render(other) == excel,
        };
        if !ok {
            diffs.push(format!("[{group}] {f}: 本引擎 {:?} != Excel {:?}", render(&got), excel));
        }
    }

    assert!(per_group.get("fin").copied().unwrap_or(0) >= 15, "{per_group:?}");
    assert!(per_group.get("lookup").copied().unwrap_or(0) >= 14, "{per_group:?}");
    assert!(per_group.get("text").copied().unwrap_or(0) >= 20, "{per_group:?}");
    assert!(
        diffs.is_empty(),
        "与 Excel 不一致 {} 条：
{}",
        diffs.len(),
        diffs.join("
")
    );
}
