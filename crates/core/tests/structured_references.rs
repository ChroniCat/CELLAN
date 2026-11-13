//! C10.1：结构化引用的完整区域/当前行语法（不依赖外部夹具）。

use sheetcore::formula::{eval_cell, eval_formula, FValue};
use sheetcore::model::{Cell, Sheet, Value, Workbook};
use sheetcore::table::{expand_structured, expand_structured_at, Table};

fn sales_book() -> Workbook {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Data".into();
    let mut t = Table {
        name: "Sales".into(),
        display: "Sales".into(),
        range: (0, 0, 4, 3),
        columns: vec!["Region".into(), "Amount".into(), "Tax".into(), "Total".into()],
        header_row: true,
        totals_row: true,
    };
    wb.sheets[0].tables.push(t.clone());
    let rows = [
        ("Region", "Amount", "Tax", "Total"),
        ("North", "100", "0.1", ""),
        ("South", "200", "0.2", ""),
        ("East", "300", "0.3", ""),
        ("Total", "999", "0.6", ""),
    ];
    for (r, row) in rows.iter().enumerate() {
        for (c, raw) in [row.0, row.1, row.2, row.3].into_iter().enumerate() {
            let v = if raw.is_empty() {
                Value::Empty
            } else if raw.chars().all(|ch| ch.is_ascii_digit() || ch == '.') {
                Value::Number(raw.parse().unwrap())
            } else {
                Value::Text((*raw).into())
            };
            wb.sheets[0].set(r as u32, c as u32, Cell { value: v, formula: None });
        }
    }
    // 清除测试中未改动的 totals 列；表格对象本身保留。
    t.columns = vec!["Region".into(), "Amount".into(), "Tax".into(), "Total".into()];
    wb
}

#[test]
fn audit_two_row_samples_calculate_as_excel() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Data".into();
    wb.sheets[0].tables.push(Table {
        name: "Sales".into(),
        display: "Sales".into(),
        range: (0, 0, 2, 1),
        columns: vec!["Amount".into()],
        header_row: true,
        totals_row: false,
    });
    wb.sheets[0].set(0, 0, Cell { value: Value::Text("Amount".into()), formula: None });
    wb.sheets[0].set(1, 0, Cell { value: Value::Number(10.0), formula: None });
    wb.sheets[0].set(2, 0, Cell { value: Value::Number(20.0), formula: None });
    match eval_formula(&wb, 0, "=Sales[Amount]") {
        FValue::Array(a) => {
            assert_eq!((a.rows, a.cols), (2, 1));
            assert_eq!(a.data, vec![FValue::Number(10.0), FValue::Number(20.0)]);
        }
        other => panic!("expected 2x1 array, got {other:?}"),
    }
    assert_eq!(
        eval_formula(&wb, 0, "=SUM(Sales[Amount])"),
        FValue::Number(30.0)
    );
    assert_eq!(
        eval_formula(&wb, 0, "=SUM(Sales[[#Data],[Amount]])"),
        FValue::Number(30.0)
    );
}

#[test]
fn bare_table_name_at_formula_start_expands() {
    let wb = sales_book();
    assert_eq!(
        expand_structured(&wb, 0, "Sales[Amount]").as_deref(),
        Some("B2:B4"),
        "公式体从表名开始时也必须展开"
    );
    assert_eq!(
        expand_structured(&wb, 0, "SUM(Sales[Amount])").as_deref(),
        Some("SUM(B2:B4)")
    );
}

#[test]
fn structured_areas_headers_totals_all_and_column_range() {
    let wb = sales_book();
    assert_eq!(
        expand_structured(&wb, 0, "SUM(Sales[Amount])").as_deref(),
        Some("SUM(B2:B4)")
    );
    assert_eq!(
        expand_structured(&wb, 0, "SUM(Sales[[#Data],[Amount]])").as_deref(),
        Some("SUM(B2:B4)")
    );
    assert_eq!(
        expand_structured(&wb, 0, "SUM(Sales[[#Headers],[Amount]])").as_deref(),
        Some("SUM(B1)")
    );
    assert_eq!(
        expand_structured(&wb, 0, "SUM(Sales[[#Totals],[Amount]])").as_deref(),
        Some("SUM(B5)")
    );
    assert_eq!(
        expand_structured(&wb, 0, "SUM(Sales[[#All],[Amount]])").as_deref(),
        Some("SUM(B1:B5)")
    );
    assert_eq!(
        expand_structured(&wb, 0, "SUM(Sales[[Amount]:[Tax]])").as_deref(),
        Some("SUM(B2:C4)")
    );
}

#[test]
fn special_column_names_are_not_split_on_space_comma_or_percent() {
    let mut wb = sales_book();
    let t = Table {
        name: "Rates".into(),
        display: "Rates".into(),
        range: (0, 4, 2, 5),
        columns: vec!["Amount USD".into(), "Tax, %".into()],
        header_row: true,
        totals_row: false,
    };
    wb.sheets[0].tables.push(t);
    assert_eq!(
        expand_structured(&wb, 0, "SUM(Rates[Amount USD])").as_deref(),
        Some("SUM(E2:E3)")
    );
    assert_eq!(
        expand_structured(&wb, 0, "SUM(Rates[[Tax, %]])").as_deref(),
        Some("SUM(F2:F3)")
    );
    assert_eq!(
        expand_structured(&wb, 0, "SUM(Rates[[Amount USD]:[Tax, %]])").as_deref(),
        Some("SUM(E2:F3)")
    );
}

#[test]
fn structured_reference_does_not_change_strings_or_sheet_refs() {
    let wb = sales_book();
    assert_eq!(
        expand_structured(&wb, 0, r#"="Sales[Amount]""#),
        None,
        "字符串里的方括号不能展开"
    );
    assert_eq!(
        expand_structured(&wb, 0, "'Sales'!A1"),
        None,
        "工作表名不能当表格名"
    );
}

#[test]
fn this_row_expands_to_current_formula_row() {
    let wb = sales_book();
    assert_eq!(
        expand_structured_at(&wb, 0, Some(1), Some(0), "Sales[[#This Row],[Amount]]").as_deref(),
        Some("B2")
    );
    assert_eq!(
        expand_structured_at(&wb, 0, Some(2), Some(0), "Sales[@[Amount]]").as_deref(),
        Some("B3")
    );
    assert_eq!(
        expand_structured_at(&wb, 0, Some(1), Some(0), "Sales[@]").as_deref(),
        Some("A2:D2")
    );
    assert_eq!(
        expand_structured_at(&wb, 0, Some(4), Some(0), "Sales[@Amount]").as_deref(),
        Some("#VALUE!"),
        "汇总行不在数据区，当前行引用应报错"
    );
}

#[test]
fn cross_sheet_structured_reference_is_qualified() {
    let mut wb = sales_book();
    let mut other = Sheet::new("Other");
    other.set(0, 0, Cell { value: Value::Text("x".into()), formula: None });
    wb.sheets.push(other);
    assert_eq!(
        expand_structured(&wb, 1, "SUM(Sales[Amount])").as_deref(),
        Some("SUM('Data'!B2:B4)")
    );
}

#[test]
fn structured_areas_really_calculate() {
    let wb = sales_book();
    assert_eq!(
        format!("{:?}", eval_formula(&wb, 0, "=SUM(Sales[[#Data],[Amount]])")),
        "Number(600.0)"
    );
    assert_eq!(
        format!("{:?}", eval_formula(&wb, 0, "=SUM(Sales[[#Totals],[Amount]])")),
        "Number(999.0)"
    );
    assert_eq!(
        format!("{:?}", eval_formula(&wb, 0, "=SUM(Sales[[#All],[Amount]])")),
        "Number(1599.0)"
    );
}

#[test]
fn current_row_really_calculates_per_row() {
    let mut wb = sales_book();
    for (r, want) in [(1u32, 10.0), (2, 40.0), (3, 90.0)] {
        let f = "=Sales[@Amount]*Sales[@Tax]".to_string();
        wb.sheets[0].set(r, 5, Cell { value: Value::Empty, formula: Some(f.into()) });
        assert_eq!(
            format!("{:?}", eval_cell(&wb, 0, r, 5)),
            format!("Number({want:?})"),
            "row {}",
            r + 1
        );
    }
    let f = "=Sales[@Amount]".to_string();
    wb.sheets[0].set(4, 5, Cell { value: Value::Empty, formula: Some(f.into()) });
    assert_eq!(format!("{:?}", eval_cell(&wb, 0, 4, 5)), "Error(Value)");
}

#[test]
fn bare_current_row_formula_uses_formula_cell_table() {
    let mut wb = sales_book();
    wb.sheets[0].set(1, 3, Cell {
        value: Value::Empty,
        formula: Some("=[@Amount]*[@Tax]".into()),
    });
    wb.sheets[0].set(2, 3, Cell {
        value: Value::Empty,
        formula: Some("=[@Amount]*[@Tax]".into()),
    });
    sheetcore::formula::recalc_all_force(&mut wb);
    assert_eq!(wb.sheets[0].value(1, 3), Value::Number(10.0));
    assert_eq!(wb.sheets[0].value(2, 3), Value::Number(40.0));

    let deps = sheetcore::formula::deps::Deps::build(&wb);
    let dirty = deps.closure(&[(0, 1, 1)]);
    assert!(dirty.contains(&(0, 1, 3)), "D2 依赖 B2/C2，D2 必须进 dirty");
}

#[test]
fn bare_current_row_picks_table_by_formula_column_when_row_has_two_tables() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Data".into();
    wb.sheets[0].tables.push(Table {
        name: "Left".into(),
        display: "Left".into(),
        range: (0, 0, 2, 1),
        columns: vec!["Amount".into(), "Tax".into()],
        header_row: true,
        totals_row: false,
    });
    wb.sheets[0].tables.push(Table {
        name: "Right".into(),
        display: "Right".into(),
        range: (0, 3, 2, 4),
        columns: vec!["Rate".into(), "Out".into()],
        header_row: true,
        totals_row: false,
    });
    for (r, a, b, rate) in [(0u32, "Amount", "Tax", "Rate"), (1, "10", "0.1", "3"), (2, "20", "0.2", "4")] {
        if r == 0 {
            wb.sheets[0].set(r, 0, Cell { value: Value::Text(a.into()), formula: None });
            wb.sheets[0].set(r, 1, Cell { value: Value::Text(b.into()), formula: None });
            wb.sheets[0].set(r, 3, Cell { value: Value::Text(rate.into()), formula: None });
            wb.sheets[0].set(r, 4, Cell { value: Value::Text("Out".into()), formula: None });
        } else {
            wb.sheets[0].set(r, 0, Cell { value: Value::Number(a.parse().unwrap()), formula: None });
            wb.sheets[0].set(r, 1, Cell { value: Value::Number(b.parse().unwrap()), formula: None });
            wb.sheets[0].set(r, 3, Cell { value: Value::Number(rate.parse().unwrap()), formula: None });
        }
    }
    // E2 在 Right 表内，同一行也有 Left 表；必须取 Right.Rate=3，而不是 Left。
    wb.sheets[0].set(1, 4, Cell {
        value: Value::Empty,
        formula: Some("=[@Rate]*2".into()),
    });
    sheetcore::formula::recalc_all_force(&mut wb);
    assert_eq!(wb.sheets[0].value(1, 4), Value::Number(6.0));

    let deps = sheetcore::formula::deps::Deps::build(&wb);
    assert!(deps.closure(&[(0, 1, 3)]).contains(&(0, 1, 4)));
}

#[test]
fn original_chinese_bare_current_row_formula_evaluates_two_rows() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Data".into();
    wb.sheets[0].tables.push(Table {
        name: "Sales".into(),
        display: "Sales".into(),
        range: (0, 0, 2, 2),
        columns: vec!["金额".into(), "税率".into(), "小计".into()],
        header_row: true,
        totals_row: false,
    });
    for (r, label) in ["金额", "税率", "小计"].iter().enumerate() {
        wb.sheets[0].set(0, r as u32, Cell { value: Value::Text((*label).into()), formula: None });
    }
    for (r, amount, tax) in [(1u32, 100.0, 0.1), (2, 200.0, 0.2)] {
        wb.sheets[0].set(r, 0, Cell { value: Value::Number(amount), formula: None });
        wb.sheets[0].set(r, 1, Cell { value: Value::Number(tax), formula: None });
        wb.sheets[0].set(r, 2, Cell {
            value: Value::Empty,
            formula: Some("=[@金额]*[@税率]".into()),
        });
    }
    sheetcore::formula::recalc_all_force(&mut wb);
    assert_eq!(wb.sheets[0].value(1, 2), Value::Number(10.0));
    assert_eq!(wb.sheets[0].value(2, 2), Value::Number(40.0));

    let deps = sheetcore::formula::deps::Deps::build(&wb);
    let dirty = deps.closure(&[(0, 1, 0), (0, 2, 1)]);
    assert!(dirty.contains(&(0, 1, 2)), "C2 依赖 A2/B2");
    assert!(dirty.contains(&(0, 2, 2)), "C3 依赖 A3/B3");
}
