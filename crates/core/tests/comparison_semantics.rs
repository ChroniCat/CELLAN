//! C05：空值 / 布尔 / 文本比较的 Excel 语义。
//!
//! Excel 的真值（本机 Excel 16.0 与 WPS 12.0 实测，见
//! `implementation-evidence/c05-comparison/`）：
//! - 空引用在比较时会先被**强制成对方的类型**：和 0 比当 0，和 "" 比当 ""，和 FALSE 比当 FALSE；
//! - 不同类别的排序固定为 数字 < 文本 < 布尔；
//! - 任何一侧是错误值，结果就是那个错误。
//!
//! 旧实现把 Empty 当成独立的最低等级，导致 `A1=0`、`A1=""`、`A1=FALSE` 全是 FALSE。

use sheetcore::formula::{self, ErrKind};
use sheetcore::formula::value::FValue;
use sheetcore::{Cell, Value, Workbook};

fn book() -> Workbook {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "S".into();
    // A1 故意留空（不写单元格）
    wb.sheets[0].set(1, 0, Cell { value: Value::Text("文本".into()), formula: None });
    wb.sheets[0].set(2, 0, Cell { value: Value::Number(5.0), formula: None });
    wb.sheets[0].set(3, 0, Cell { value: Value::Bool(true), formula: None });
    wb.sheets[0].set(4, 0, Cell { value: Value::Error(ErrKind::Na), formula: Some("=NA()".into()) });
    wb
}

fn eval(f: &str) -> FValue {
    let wb = book();
    formula::recalc_all(&mut { wb.clone() });
    formula::eval_formula(&wb, 0, f)
}

fn b(v: FValue) -> bool {
    match v {
        FValue::Bool(x) => x,
        other => panic!("不是布尔：{other:?}"),
    }
}

fn err(v: FValue) -> ErrKind {
    match v {
        FValue::Error(e) => e,
        other => panic!("不是错误：{other:?}"),
    }
}

#[test]
fn blank_compares_as_zero_empty_string_and_false() {
    // = 与 <>
    for f in ["=A1=0", "=A1=\"\"", "=A1=FALSE"] {
        assert!(b(eval(f)), "{f} 应为 TRUE（空引用按对方类型强制）");
    }
    for f in ["=A1<>0", "=A1<>\"\"", "=A1<>FALSE"] {
        assert!(!b(eval(f)), "{f} 应为 FALSE");
    }
    // 大小关系：与 0 / "" / FALSE 都相等
    for f in ["=A1<0", "=A1>0", "=A1<\"\"", "=A1>\"\"", "=A1<FALSE", "=A1>FALSE"] {
        assert!(!b(eval(f)), "{f} 应为 FALSE（相等，不大于也不小于）");
    }
    for f in ["=A1<=0", "=A1>=0", "=A1<=\"\"", "=A1>=\"\"", "=A1<=FALSE", "=A1>=FALSE"] {
        assert!(b(eval(f)), "{f} 应为 TRUE");
    }
    // 与文本 / TRUE 比较：空 → ""，"" < "文本"；空 → FALSE，FALSE < TRUE
    assert!(!b(eval("=A1=\"文本\"")));
    assert!(b(eval("=A1<\"文本\"")));
    assert!(!b(eval("=A1=TRUE")));
    assert!(b(eval("=A1<TRUE")));
}

#[test]
fn type_order_is_number_then_text_then_boolean() {
    // 数字 < 文本 < 布尔（Excel 的跨类别排序）
    assert!(b(eval("=A3<A2")), "5 < \"文本\"");
    assert!(b(eval("=A2<A4")), "\"文本\" < TRUE");
    assert!(b(eval("=A3<A4")), "5 < TRUE");
    assert!(b(eval("=A2>0")), "文本 > 数字");
    assert!(b(eval("=A4>9")), "TRUE > 数字");
    assert!(!b(eval("=A2=5")), "文本 \"文本\" 不等于数字 5");
    assert!(!b(eval("=A3=\"5\"")), "数字 5 不等于文本 \"5\"");
    // 文本比较不区分大小写
    let mut wb = book();
    wb.sheets[0].set(9, 0, Cell { value: Value::Text("ABC".into()), formula: None });
    wb.sheets[0].set(9, 1, Cell { value: Value::Text("abc".into()), formula: None });
    assert!(b(formula::eval_formula(&wb, 0, "=A10=B10")));
}

#[test]
fn errors_propagate_from_either_side() {
    assert_eq!(err(eval("=A1=A5")), ErrKind::Na);
    assert_eq!(err(eval("=A5=0")), ErrKind::Na);
    assert_eq!(err(eval("=A5>A5")), ErrKind::Na);
}

#[test]
fn conditional_aggregates_keep_their_blank_semantics() {
    // 修比较语义不能把空值相关函数带坏
    let wb = book();
    assert_eq!(eval("=COUNTBLANK(A1:A5)"), FValue::Number(1.0));
    assert_eq!(eval("=IF(A1=0,\"空\",\"非空\")"), FValue::Text("空".into()));
    assert_eq!(eval("=IF(A1=\"\",\"空\",\"非空\")"), FValue::Text("空".into()));
    assert_eq!(eval("=IF(A1,\"真\",\"假\")"), FValue::Text("假".into()));
    // COUNTIF 是条件匹配上下文，不等同于比较运算：
    // Excel 16.0 / WPS 12.0 实测 COUNTIF(A1:A5,0) 在 A1 为空、A3=5 时是 0（空单元格不参与数字条件）。
    assert_eq!(eval("=COUNTIF(A1:A5,0)"), FValue::Number(0.0));
}


/// 条件匹配的空值语义：空白只在条件是空文本时命中，数字条件一律跳过空白。
/// 真值来自本机 Excel 16.0 与 WPS 12.0 实算（范围 A1:A6 = 空 / "文本" / 5 / TRUE / #N/A / 0）。
#[test]
fn criteria_matching_has_its_own_blank_rules() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "S".into();
    wb.sheets[0].set(1, 0, Cell { value: Value::Text("文本".into()), formula: None });
    wb.sheets[0].set(2, 0, Cell { value: Value::Number(5.0), formula: None });
    wb.sheets[0].set(3, 0, Cell { value: Value::Bool(true), formula: None });
    wb.sheets[0].set(4, 0, Cell { value: Value::Error(ErrKind::Na), formula: None });
    wb.sheets[0].set(5, 0, Cell { value: Value::Number(0.0), formula: None });
    let f = |x: &str| formula::eval_formula(&wb, 0, x);
    assert_eq!(f("=COUNTIF(A1:A6,0)"), FValue::Number(1.0), "只有显式的 0 命中");
    assert_eq!(f("=COUNTIF(A1:A6,\"<>0\")"), FValue::Number(5.0), "「不等于」把空白算进来");
    assert_eq!(f("=COUNTIF(A1:A6,\">0\")"), FValue::Number(1.0));
    assert_eq!(f("=COUNTIF(A1:A6,\"\")"), FValue::Number(1.0), "空文本条件只命中空白");
    assert_eq!(f("=COUNTIF(A1:A6,\"<>\")"), FValue::Number(5.0), "空白不算非空");
    assert_eq!(f("=COUNTIF(A1:A6,\"文本\")"), FValue::Number(1.0));
    assert_eq!(f("=COUNTIF(A1:A6,\"<>文本\")"), FValue::Number(5.0));
    assert_eq!(f("=SUMIF(A1:A6,0)"), FValue::Number(0.0), "SUMIF 同样跳过空白");
    assert_eq!(f("=COUNTIF(A1:A6,\">=0\")"), FValue::Number(2.0));
    // 文本数字与布尔：相等条件会带文本 "5"，关系条件只数真正的数字
    let mut wb2 = Workbook::new_blank();
    wb2.sheets[0].set(0, 0, Cell { value: Value::Text("5".into()), formula: None });
    wb2.sheets[0].set(1, 0, Cell { value: Value::Number(5.0), formula: None });
    wb2.sheets[0].set(2, 0, Cell { value: Value::Bool(true), formula: None });
    wb2.sheets[0].set(3, 0, Cell { value: Value::Text("abc".into()), formula: None });
    let g = |x: &str| formula::eval_formula(&wb2, 0, x);
    assert_eq!(g("=COUNTIF(A1:A4,5)"), FValue::Number(2.0), "数字 5 与文本 5 都算");
    assert_eq!(g("=COUNTIF(A1:A4,\">0\")"), FValue::Number(1.0), "关系条件只数数字");
    assert_eq!(g("=COUNTIF(A1:A4,\">=5\")"), FValue::Number(1.0));
    assert_eq!(g("=COUNTIF(A1:A4,TRUE)"), FValue::Number(1.0));
    assert_eq!(g("=COUNTIF(A1:A4,\"abc\")"), FValue::Number(1.0));
    assert_eq!(g("=SUMIF(A1:A4,\">0\")"), FValue::Number(5.0), "SUMIF 只加数字");
}
/// 生成给 Excel/WPS 实机算的矩阵夹具（C1.. 每行一条公式）
#[test]
#[ignore = "生成 C05 对拍夹具（Excel/WPS 实机计算）"]
fn make_c05_fixture() {
    let dir = std::env::temp_dir().join("sc_c05");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut wb = book();
    let formulas = [
        "=A1=0", "=A1=\"\"", "=A1=FALSE", "=A1=TRUE", "=A1=\"文本\"", "=A1=A3",
        "=A1<>0", "=A1<>\"\"", "=A1<>FALSE",
        "=A1<0", "=A1<=0", "=A1>0", "=A1>=0",
        "=A1<\"\"", "=A1<=\"\"", "=A1>\"\"", "=A1>=\"\"",
        "=A1<FALSE", "=A1<=FALSE", "=A1>FALSE", "=A1>=FALSE",
        "=A1<\"文本\"", "=A1>=\"文本\"", "=A1<TRUE",
        "=A3<A2", "=A2<A4", "=A3<A4", "=A2>0", "=A4>9", "=A2=5", "=A3=\"5\"",
        "=A1=A5", "=A5=0", "=COUNTBLANK(A1:A5)", "=IF(A1=0,\"空\",\"非空\")",
        "=IF(A1=\"\",\"空\",\"非空\")", "=IF(A1,\"真\",\"假\")", "=COUNTIF(A1:A5,0)",
        // 条件匹配上下文：空白/文本数字/布尔
        "=COUNTIF(A1:A5,\"<>0\")", "=COUNTIF(A1:A5,\"\")", "=COUNTIF(A1:A5,\"<>\")",
        "=COUNTIF(A1:A5,\"文本\")", "=COUNTIF(A1:A5,\"<>文本\")", "=SUMIF(A1:A5,0)",
        "=COUNTIF(A1:A5,\">0\")", "=COUNTIF(A1:A5,\">=0\")", "=COUNTIF(A1:A5,\"abc\")",
    ];
    for (i, f) in formulas.iter().enumerate() {
        wb.sheets[0].set(i as u32, 2, Cell { value: Value::Empty, formula: Some((*f).into()) });
    }
    let p = dir.join("c05.xlsx");
    sheetcore::xlsx::save(&p, &wb).unwrap();
    println!("FIXTURE {} 共 {} 条公式", p.display(), formulas.len());
}

