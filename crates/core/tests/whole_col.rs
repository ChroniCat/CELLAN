use sheetcore::formula;
use sheetcore::model::{Cell, Value, Workbook};

fn t(s: &str) -> Cell {
    Cell { value: Value::Text(s.into()), formula: None }
}
fn n(v: f64) -> Cell {
    Cell { value: Value::Number(v), formula: None }
}

#[test]
fn vlookup_over_whole_columns() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(0, 0, t("a"));
    wb.sheets[0].set(0, 2, n(1.0));
    wb.sheets[0].set(1, 0, t("b"));
    wb.sheets[0].set(1, 2, n(2.0));
    wb.sheets[0].set(4, 4, Cell { value: Value::Empty, formula: Some("=VLOOKUP(\"b\",A:C,3,FALSE)".into()) });
    formula::recalc_all(&mut wb);
    assert_eq!(wb.sheets[0].value(4, 4), Value::Number(2.0));
}

#[test]
fn sum_over_whole_columns() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(0, 0, n(3.0));
    wb.sheets[0].set(2, 0, n(4.0));
    wb.sheets[0].set(4, 2, Cell { value: Value::Empty, formula: Some("=SUM(A:A)".into()) });
    formula::recalc_all(&mut wb);
    assert_eq!(wb.sheets[0].value(4, 2), Value::Number(7.0));
}
