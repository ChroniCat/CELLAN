//! C11.1：整行引用 `1:3` 的解析、求值和规范化边界。

use sheetcore::formula::{eval_formula, FValue};
use sheetcore::normalize_formula;
use sheetcore::model::{Cell, Sheet, Value, Workbook};

fn num(v: FValue) -> f64 {
    match v {
        FValue::Number(n) => n,
        other => panic!("expected number, got {other:?}"),
    }
}

fn book() -> Workbook {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Data".into();
    for (r, c, n) in [(0u32, 0u32, 1.0), (1, 1, 2.0), (2, 2, 3.0), (3, 3, 4.0)] {
        wb.sheets[0].set(r, c, Cell { value: Value::Number(n), formula: None });
    }
    wb
}

#[test]
fn whole_row_reference_normalizes_to_a1_but_not_strings() {
    assert_eq!(normalize_formula("=SUM(1:3)"), "=SUM(A1:XFD3)");
    assert_eq!(normalize_formula("=SUM($1:$3)"), "=SUM(A$1:XFD$3)");
    assert_eq!(normalize_formula(r#"="1:3""#), r#"="1:3""#);
    assert_eq!(normalize_formula("=SUM(A1:B2)"), "=SUM(A1:B2)");
}

#[test]
fn whole_row_sum_uses_used_range_and_is_sheet_scoped() {
    let mut wb = book();
    assert_eq!(num(eval_formula(&wb, 0, "=SUM(1:3)")), 6.0);
    assert_eq!(num(eval_formula(&wb, 0, "=SUM(1:1)")), 1.0);
    assert_eq!(num(eval_formula(&wb, 0, "=SUM(2:2)")), 2.0);
    assert_eq!(num(eval_formula(&wb, 0, "=SUM($2:$2)")), 2.0);

    let mut other = Sheet::new("Other");
    other.set(0, 0, Cell { value: Value::Number(10.0), formula: None });
    other.set(1, 0, Cell { value: Value::Number(20.0), formula: None });
    wb.sheets.push(other);
    assert_eq!(num(eval_formula(&wb, 0, "=SUM(Other!1:2)")), 30.0);
    assert_eq!(num(eval_formula(&wb, 0, "=SUM(Other!$2:$2)")), 20.0);
}

#[test]
fn whole_row_inside_structured_reference_is_not_rewritten() {
    // 方括号里是列名，不是行区间；解析器不能把它拆成 A1:XFD。
    let out = normalize_formula("=SUM(Sales[1:3])");
    assert_eq!(out, "=SUM(Sales[1:3])");
}

#[test]
fn whole_row_is_a_dependency_and_moves_on_row_insert() {
    let mut wb = book();
    wb.sheets[0].set(
        3,
        0,
        Cell { value: Value::Empty, formula: Some("=SUM(1:3)".into()) },
    );
    let deps = sheetcore::formula::deps::Deps::build(&wb);
    let dirty = deps.closure(&[(0, 1, 1)]);
    assert!(dirty.contains(&(0, 3, 0)), "A4 formula must depend on row 2");

    let mut shift = sheetcore::refs::Shift { axis: sheetcore::refs::Axis::Row, at: 0, n: 1, ..Default::default() };
    shift.target_sheet = "Data".into();
    shift.own_sheet = "Data".into();
    let out = sheetcore::refs::rewrite("=SUM(1:3)", &shift);
    assert_eq!(out, "=SUM(2:4)");
}
