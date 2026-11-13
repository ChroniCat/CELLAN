//! C05：用本机 Excel/WPS 实算的矩阵逐条对拍空值、布尔、文本比较与条件匹配。
//!
//! `fixtures/c05/matrix.tsv` 每行 `公式<TAB>Excel结果<TAB>WPS结果`，由
//! `scripts/excel_c05_probe.ps1` 打开同一夹具计算后导出（Excel 16.0 与 WPS 12.0 结果一致）。

use sheetcore::formula::{self, ErrKind};
use sheetcore::formula::value::FValue;
use sheetcore::{Cell, Value, Workbook};

fn book() -> Workbook {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "S".into();
    // 与夹具一致：A1 空、A2 "文本"、A3 5、A4 TRUE、A5 #N/A
    wb.sheets[0].set(1, 0, Cell { value: Value::Text("文本".into()), formula: None });
    wb.sheets[0].set(2, 0, Cell { value: Value::Number(5.0), formula: None });
    wb.sheets[0].set(3, 0, Cell { value: Value::Bool(true), formula: None });
    wb.sheets[0].set(4, 0, Cell { value: Value::Error(ErrKind::Na), formula: None });
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
            if (n.fract()).abs() < f64::EPSILON {
                format!("{}", *n as i64)
            } else {
                format!("{n}")
            }
        }
        FValue::Empty => String::new(),
        other => other.display(),
    }
}

#[test]
fn comparison_and_criteria_matrix_matches_excel_and_wps() {
    let tsv = include_str!("fixtures/c05/matrix.tsv");
    let wb = book();
    let mut checked = 0;
    let mut diffs = Vec::new();
    for line in tsv.lines() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let mut it = line.split('\t');
        let Some(f) = it.next() else { continue };
        let excel = it.next().unwrap_or("").trim();
        let wps = it.next().unwrap_or("").trim();
        assert_eq!(excel, wps, "{f} 两个办公软件结果不一致，夹具本身有问题");
        let got = render(&formula::eval_formula(&wb, 0, f));
        checked += 1;
        if got != excel {
            diffs.push(format!("{f}: 本引擎 {got:?} != Excel {excel:?}"));
        }
    }
    assert!(checked >= 40, "矩阵太小：{checked}");
    assert!(diffs.is_empty(), "与 Excel/WPS 不一致：\n{}", diffs.join("\n"));
}
