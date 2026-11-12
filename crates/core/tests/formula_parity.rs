//! 公式引擎对拍测试：期望值来自 Excel 365 的实际结果

use sheetcore::formula::{self, FValue};
use sheetcore::{Cell, Value, Workbook};

fn book() -> Workbook {
    let mut wb = Workbook::new_blank();
    let data = [
        (0, 0, Value::Number(1.0)),
        (1, 0, Value::Number(2.0)),
        (2, 0, Value::Number(3.0)),
        (3, 0, Value::Text("b".into())),
        (0, 1, Value::Text("apple".into())),
        (1, 1, Value::Text("banana".into())),
        (2, 1, Value::Text("cherry".into())),
        (0, 2, Value::Number(10.0)),
        (1, 2, Value::Number(20.0)),
        (2, 2, Value::Number(30.0)),
    ];
    for (r, c, v) in data {
        wb.sheets[0].set(r, c, Cell { value: v, formula: None });
    }
    wb
}

fn ev(wb: &Workbook, f: &str) -> String {
    formula::eval_formula(wb, 0, f).display()
}

#[test]
fn arithmetic_and_precedence() {
    let wb = book();
    assert_eq!(ev(&wb, "=1+2*3"), "7");
    assert_eq!(ev(&wb, "=(1+2)*3"), "9");
    assert_eq!(ev(&wb, "=2^3^2"), "64"); // Excel 的 ^ 是左结合
    assert_eq!(ev(&wb, "=-2^2"), "4"); // 一元负号优先于乘方（Excel 行为）
    assert_eq!(ev(&wb, "=10/4"), "2.5");
    assert_eq!(ev(&wb, "=10%"), "0.1");
    assert_eq!(ev(&wb, "=1/0"), "#DIV/0!");
}

#[test]
fn comparisons_and_logic() {
    let wb = book();
    assert_eq!(ev(&wb, "=1>0"), "TRUE");
    assert_eq!(ev(&wb, "=1<>1"), "FALSE");
    assert_eq!(ev(&wb, "=\"a\"&\"b\""), "ab");
    assert_eq!(ev(&wb, "=AND(TRUE,1)"), "TRUE");
    assert_eq!(ev(&wb, "=OR(FALSE,0)"), "FALSE");
    assert_eq!(ev(&wb, "=NOT(FALSE)"), "TRUE");
    assert_eq!(ev(&wb, "=IF(A1=1,\"yes\",\"no\")"), "yes");
    // 未被选中的分支不能求值
    assert_eq!(ev(&wb, "=IF(A1=0,1,1/A1)"), "1");
    assert_eq!(ev(&wb, "=IFERROR(1/0,42)"), "42");
}

#[test]
fn ranges_and_aggregates() {
    let wb = book();
    assert_eq!(ev(&wb, "=SUM(A1:A3)"), "6");
    assert_eq!(ev(&wb, "=SUM(A1:C3)"), "66");
    assert_eq!(ev(&wb, "=AVERAGE(A1:A3)"), "2");
    assert_eq!(ev(&wb, "=COUNT(A1:C3)"), "6");
    assert_eq!(ev(&wb, "=COUNTA(A1:C3)"), "9");
    assert_eq!(ev(&wb, "=MAX(A1:A3)"), "3");
    assert_eq!(ev(&wb, "=MIN(A1:A3)"), "1");
    assert_eq!(ev(&wb, "=SUMPRODUCT(A1:A3,C1:C3)"), "140");
    assert_eq!(ev(&wb, "=MEDIAN(A1:A3)"), "2");
    assert_eq!(ev(&wb, "=STDEV(A1:A3)"), "1");
}

#[test]
fn conditional_aggregates() {
    let wb = book();
    assert_eq!(ev(&wb, "=COUNTIF(A1:A3,\">1\")"), "2");
    assert_eq!(ev(&wb, "=SUMIF(A1:A3,\">1\")"), "5");
    assert_eq!(ev(&wb, "=AVERAGEIF(A1:A3,\">1\")"), "2.5");
    assert_eq!(ev(&wb, "=COUNTIF(B1:B3,\"a*\")"), "1");
    assert_eq!(ev(&wb, "=SUMIFS(C1:C3,A1:A3,\">1\")"), "50");
}

#[test]
fn text_functions() {
    let wb = book();
    assert_eq!(ev(&wb, "=LEN(\"hello\")"), "5");
    assert_eq!(ev(&wb, "=LEFT(\"hello\",2)"), "he");
    assert_eq!(ev(&wb, "=RIGHT(\"hello\",2)"), "lo");
    assert_eq!(ev(&wb, "=MID(\"hello\",2,3)"), "ell");
    assert_eq!(ev(&wb, "=UPPER(\"ab\")"), "AB");
    assert_eq!(ev(&wb, "=TRIM(\"  a   b  \")"), "a b");
    assert_eq!(ev(&wb, "=SUBSTITUTE(\"a-b-c\",\"-\",\"+\")"), "a+b+c");
    assert_eq!(ev(&wb, "=REPT(\"ab\",3)"), "ababab");
    assert_eq!(ev(&wb, "=TEXTJOIN(\",\",TRUE,\"a\",\"b\")"), "a,b");
    assert_eq!(ev(&wb, "=FIND(\"l\",\"hello\")"), "3");
    assert_eq!(ev(&wb, "=CONCATENATE(\"x\",1,TRUE)"), "x1TRUE");
}

#[test]
fn lookup_functions() {
    let wb = book();
    assert_eq!(ev(&wb, "=VLOOKUP(2,A1:C3,3,FALSE)"), "20");
    assert_eq!(ev(&wb, "=INDEX(B1:B3,2)"), "banana");
    assert_eq!(ev(&wb, "=MATCH(\"banana\",B1:B3,0)"), "2");
    assert_eq!(ev(&wb, "=XLOOKUP(\"cherry\",B1:B3,C1:C3)"), "30");
    assert_eq!(ev(&wb, "=CHOOSE(2,\"x\",\"y\")"), "y");
    assert_eq!(ev(&wb, "=ROWS(A1:A3)"), "3");
}

#[test]
fn date_functions() {
    let wb = book();
    assert_eq!(ev(&wb, "=YEAR(DATE(2026,11,6))"), "2026");
    assert_eq!(ev(&wb, "=MONTH(DATE(2026,11,6))"), "11");
    assert_eq!(ev(&wb, "=DAY(DATE(2026,11,6))"), "6");
    assert_eq!(ev(&wb, "=TEXT(DATE(2026,11,6),\"yyyy-mm-dd\")"), "2026-11-06");
    // Excel: 1900-01-01 = 1，且带 1900 闰年 bug
    assert_eq!(ev(&wb, "=DATE(1900,1,1)"), "1");
    assert_eq!(ev(&wb, "=DATE(1900,3,1)"), "61");
    assert_eq!(ev(&wb, "=DATE(2026,11,6)-DATE(2026,11,1)"), "5");
    assert_eq!(ev(&wb, "=EDATE(DATE(2026,1,31),1)"), "46081"); // 2026-02-28
}

#[test]
fn math_and_rounding() {
    let wb = book();
    assert_eq!(ev(&wb, "=ROUND(2.675,2)"), "2.68");
    assert_eq!(ev(&wb, "=ROUNDDOWN(2.679,2)"), "2.67");
    assert_eq!(ev(&wb, "=ROUNDUP(2.671,2)"), "2.68");
    assert_eq!(ev(&wb, "=MOD(-3,2)"), "1");
    assert_eq!(ev(&wb, "=INT(-2.5)"), "-3");
    assert_eq!(ev(&wb, "=ABS(-5)"), "5");
    assert_eq!(ev(&wb, "=POWER(2,10)"), "1024");
    assert_eq!(ev(&wb, "=SQRT(16)"), "4");
    assert_eq!(ev(&wb, "=SQRT(-1)"), "#NUM!");
    assert_eq!(ev(&wb, "=GCD(12,18)"), "6");
}

#[test]
fn financial_functions() {
    let wb = book();
    // Excel: PMT(0.05/12, 360, -1000000) = 5368.2162...
    let v = formula::eval_formula(&wb, 0, "=PMT(0.05/12,360,-1000000)");
    match v {
        FValue::Number(n) => assert!((n - 5368.2162).abs() < 0.01, "got {n}"),
        other => panic!("unexpected {other:?}"),
    }
    let fv = formula::eval_formula(&wb, 0, "=FV(0.05/12,360,5368.22,-1000000)");
    match fv {
        FValue::Number(n) => assert!(n.abs() < 5.0, "got {n}"),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn errors_propagate() {
    let wb = book();
    assert_eq!(ev(&wb, "=1+\"abc\""), "#VALUE!");
    assert_eq!(ev(&wb, "=NOSUCHFUNC(1)"), "#NAME?");
    assert_eq!(ev(&wb, "=#N/A"), "#N/A");
    assert_eq!(ev(&wb, "=SUM(1,1/0)"), "#DIV/0!");
}

#[test]
fn recalc_chain_in_workbook() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(0, 0, Cell { value: Value::Number(1.0), formula: None });
    wb.sheets[0].set(1, 0, Cell { value: Value::Empty, formula: Some("=A1*2".into()) });
    wb.sheets[0].set(2, 0, Cell { value: Value::Empty, formula: Some("=A2+1".into()) });
    formula::recalc_all(&mut wb);
    assert_eq!(wb.sheets[0].value(1, 0), Value::Number(2.0));
    assert_eq!(wb.sheets[0].value(2, 0), Value::Number(3.0));
}

#[test]
fn circular_reference_is_caught() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(0, 0, Cell { value: Value::Empty, formula: Some("=A2".into()) });
    wb.sheets[0].set(1, 0, Cell { value: Value::Empty, formula: Some("=A1".into()) });
    formula::recalc_all(&mut wb);
    // C01：循环引用是错误**类型**（显示为 #REF!），不是一段文本
    assert_eq!(
        wb.sheets[0].value(0, 0),
        Value::Error(sheetcore::formula::ErrKind::Circular)
    );
    assert_eq!(wb.sheets[0].value(0, 0).display(), "#REF!");
}

#[test]
fn cross_sheet_reference() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Data".into();
    wb.sheets[0].set(0, 0, Cell { value: Value::Number(7.0), formula: None });
    let mut s2 = sheetcore::Sheet::new("Calc");
    s2.set(0, 0, Cell { value: Value::Empty, formula: Some("=Data!A1*3".into()) });
    wb.sheets.push(s2);
    formula::recalc_all(&mut wb);
    assert_eq!(wb.sheets[1].value(0, 0), Value::Number(21.0));
}

#[test]
fn function_coverage_is_reported() {
    let list = formula::functions::implemented();
    assert!(list.len() >= 300, "实现数量 {}", list.len());
}
