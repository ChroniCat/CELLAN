//! C03 公式预处理不得改写字符串和标识符。
//!
//! Excel 的规则很简单：引号里是数据，不是公式。所以
//! `="_xlfn.TEXT"`、`="Sales[Amount]"`、`="A:A"` 的计算结果是这些字符本身，
//! 存盘也必须一字不改。旧实现用全局 `str::replace` 去 `_xlfn.` 前缀、用字符扫描
//! 展开整列引用与结构化引用，两者都不看引号状态，会把字符串一起改掉。

use sheetcore::formula::{self, FValue};
use sheetcore::table::expand_structured;
use sheetcore::{Cell, Value, Workbook};

fn ev(wb: &Workbook, f: &str) -> String {
    formula::eval_formula(wb, 0, f).display()
}

/// A1:B4 的表，表名 `Sales`，列 `Amount`；表列值 10/20/30。
fn book_with_table() -> Workbook {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "数据".to_string();
    for (r, c, v) in [
        (0, 0, Value::Text("Amount".into())),
        (1, 0, Value::Number(10.0)),
        (2, 0, Value::Number(20.0)),
        (3, 0, Value::Number(30.0)),
    ] {
        wb.sheets[0].set(r, c, Cell { value: v, formula: None });
    }
    wb.sheets[0].tables.push(sheetcore::table::Table {
        name: "Sales".into(),
        display: "Sales".into(),
        range: (0, 0, 3, 0),
        columns: vec!["Amount".into()],
        header_row: true,
        totals_row: false,
    });
    wb
}

// ---------------------------------------------------------------- 字符串原样

#[test]
fn xlfn_prefix_inside_a_string_is_not_stripped() {
    let src = r#"=" _xlfn.TEXT""#;
    assert_eq!(sheetcore::normalize_formula(src), src, "字符串里的 _xlfn. 被改写了");
    let src = r#"="前缀_xlfn.TEXT后缀""#;
    assert_eq!(sheetcore::normalize_formula(src), src);
}

#[test]
fn escaped_quotes_keep_the_string_intact() {
    // ="他说""_xlfn.X""了" —— "" 是转义引号，整段仍是一个字符串
    let src = "=\"他说\"\"_xlfn.X\"\"了\"";
    assert_eq!(sheetcore::normalize_formula(&src), src);
}

#[test]
fn whole_column_reference_inside_a_string_is_not_expanded() {
    let src = r#"="A:A""#;
    assert_eq!(sheetcore::normalize_formula(src), src, "字符串里的 A:A 被当成整列引用展开");
    let src = r#"="A:C""#;
    assert_eq!(sheetcore::normalize_formula(src), src);
}

// ---------------------------------------------------------------- 结构化引用

#[test]
fn structured_reference_inside_a_string_is_not_expanded() {
    let wb = book_with_table();
    assert_eq!(
        expand_structured(&wb, 0, r#"="Sales[Amount]""#),
        None,
        "字符串里的结构化引用不该被展开"
    );
    assert_eq!(expand_structured(&wb, 0, r#"="合计 Sales[Amount]""#), None);
}

#[test]
fn real_structured_reference_still_expands_and_calculates() {
    let wb = book_with_table();
    // 真引用照旧展开
    let out = expand_structured(&wb, 0, "=SUM(Sales[Amount])").expect("真结构化引用应该被展开");
    assert_eq!(out, "=SUM(A2:A4)");
    assert_eq!(ev(&wb, "=SUM(Sales[Amount])"), "60");
}

#[test]
fn single_quoted_sheet_name_is_not_mistaken_for_a_table() {
    // 'Sales'!A1 里的 Sales 是表名（工作表名），不是表格对象
    let wb = book_with_table();
    assert_eq!(
        expand_structured(&wb, 0, "='Sales'!A2"),
        None,
        "单引号工作表名里的名字被当成表格展开了"
    );
}

// ---------------------------------------------------------------- 真前缀仍生效

#[test]
fn real_function_prefixes_are_still_normalized() {
    assert_eq!(
        sheetcore::normalize_formula("=_xlfn.XLOOKUP(A1,A:A,B:B)"),
        "=XLOOKUP(A1,$A$1:$A$1048576,$B$1:$B$1048576)"
    );
    assert_eq!(
        sheetcore::normalize_formula("=_xlfn._xlws.FILTER(A1:A3,B1:B3>0)"),
        "=FILTER(A1:A3,B1:B3>0)"
    );
    assert_eq!(sheetcore::normalize_formula("=_xlfn._xlws.SORT(A1:A3)"), "=SORT(A1:A3)");
    // 字符串里的不能被顺手带走：真前缀在引号外被去掉，引号内的原样
    assert_eq!(
        sheetcore::normalize_formula(r#"="_xlfn.TEXT"&_xlfn.TEXT(1,"0")"#),
        r#"="_xlfn.TEXT"&TEXT(1,"0")"#
    );
}

// ---------------------------------------------------------------- 计算路径

#[test]
fn evaluated_strings_are_returned_verbatim() {
    let wb = book_with_table();
    assert_eq!(ev(&wb, r#"="Sales[Amount]""#), "Sales[Amount]");
    assert_eq!(ev(&wb, r#"="A:A""#), "A:A");
    assert_eq!(ev(&wb, r##"="""_xlfn.TEXT""""##), "\"_xlfn.TEXT\"");
    assert_eq!(ev(&wb, r#"="a""b""c""#), "a\"b\"c");
}

// ---------------------------------------------------------------- 五条路径

#[test]
fn input_save_load_copy_keep_the_formula_text() {
    let dir = std::env::temp_dir().join(format!("c03-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("strings.xlsx");
    let mut wb = book_with_table();
    let f = r#"="Sales[Amount]""#.to_string();
    wb.sheets[0].set(
        0,
        2,
        Cell { value: Value::Text("Sales[Amount]".into()), formula: Some(sheetcore::normalize_formula(&f).into()) },
    );
    wb.sheets[0].set(
        1,
        2,
        Cell { value: Value::Text("A:A".into()), formula: Some(sheetcore::normalize_formula(r#"="A:A""#).into()) },
    );
    // 输入路径：内存里的公式文本已经是原文
    assert_eq!(wb.sheets[0].get(0, 2).unwrap().formula.as_deref(), Some(r#"="Sales[Amount]""#));
    // 重算路径
    assert_eq!(ev(&wb, r#"="Sales[Amount]""#), "Sales[Amount]");
    // 保存 → 载入
    sheetcore::xlsx::save_atomic(&path, &wb).expect("保存失败");
    let back = sheetcore::xlsx::load(&path).expect("载入失败");
    assert_eq!(back.sheets[0].get(0, 2).unwrap().formula.as_deref(), Some(r#"="Sales[Amount]""#));
    assert_eq!(back.sheets[0].get(1, 2).unwrap().formula.as_deref(), Some(r#"="A:A""#));
    // 载入后重算仍一致
    assert_eq!(ev(&back, back.sheets[0].get(0, 2).unwrap().formula.as_deref().unwrap()), "Sales[Amount]");
    // 复制路径：公式文本原样搬走
    let src_formula = back.sheets[0].get(0, 2).unwrap().formula.clone().unwrap();
    let copied = sheetcore::sparse::shift_refs(&src_formula, 0, 0);
    assert_eq!(copied, src_formula.as_ref(), "复制路径改写了公式文本");
    // 再存一次，文本不变
    sheetcore::xlsx::save_atomic(&path, &back).expect("二次保存失败");
    let again = sheetcore::xlsx::load(&path).expect("二次载入失败");
    assert_eq!(again.sheets[0].get(0, 2).unwrap().formula.as_deref(), Some(r#"="Sales[Amount]""#));
}

#[test]
fn quoted_sheet_name_reference_escapes_and_resolves() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Bob's".to_string();
    wb.sheets[0].set(0, 0, Cell { value: Value::Number(7.0), formula: None });
    let v = formula::eval_formula(&wb, 0, "='Bob''s'!A1");
    assert_eq!(v.display(), "7", "含单引号的工作表名必须按 '' 转义解析");
}

#[test]
fn numbers_in_strings_are_not_mistaken_for_references() {
    let wb = book_with_table();
    assert_eq!(ev(&wb, r#"="1:1""#), "1:1");
    assert_eq!(ev(&wb, r#"="A1:B2""#), "A1:B2");
    let _ = FValue::Error(sheetcore::formula::ErrKind::Value);
}
