//! C10.2：表格行扩展、计算列自动填充与列重命名。

use sheetcore::formula::recalc_all_force;
use sheetcore::model::{Cell, Value, Workbook};
use sheetcore::table::{
    convert_table_to_range, fill_calculated_column, rename_table_column, Table,
};

fn table_book() -> Workbook {
    let mut wb = Workbook::new_blank();
    let sh = &mut wb.sheets[0];
    sh.name = "Data".into();
    sh.tables.push(Table {
        name: "Sales".into(),
        display: "Sales".into(),
        range: (0, 0, 1, 2),
        columns: vec!["Amount".into(), "Tax".into(), "Total".into()],
        column_ids: vec![11, 12, 13],
        header_row: true,
        totals_row: false,
        ..Default::default()
    });
    let headers = ["Amount", "Tax", "Total"];
    for (c, h) in headers.iter().enumerate() {
        sh.set(
            0,
            c as u32,
            Cell {
                value: Value::Text((*h).into()),
                formula: None,
            },
        );
    }
    sh.set(1, 0, Cell { value: Value::Number(100.0), formula: None });
    sh.set(1, 1, Cell { value: Value::Number(0.1), formula: None });
    sh.set(
        1,
        2,
        Cell {
            value: Value::Empty,
            formula: Some("=[@Amount]*[@Tax]".into()),
        },
    );
    wb
}

#[test]
fn inserting_a_row_inside_a_table_expands_it_and_fills_calculated_column() {
    let mut wb = table_book();
    let sh = &mut wb.sheets[0];
    sh.insert_rows(2, 1);

    let t = &sh.tables[0];
    assert_eq!(t.range, (0, 0, 2, 2), "表体插入行应扩展表格末行");
    assert!(sh.tables_dirty, "表格范围改变必须标记待保存");
    assert_eq!(
        sh.formula_of(2, 2),
        Some("=[@Amount]*[@Tax]"),
        "新行应复制上一数据行的计算列公式"
    );
    assert_eq!(
        sh.formula_of(1, 2),
        Some("=[@Amount]*[@Tax]"),
        "原数据行应保留公式"
    );

    sh.set(2, 0, Cell { value: Value::Number(50.0), formula: None });
    sh.set(2, 1, Cell { value: Value::Number(0.2), formula: None });
    recalc_all_force(&mut wb);
    let sh = &wb.sheets[0];
    assert_eq!(
        sh.get(2, 2).map(|c| c.value.clone()),
        Some(Value::Number(10.0)),
        "新行公式应按本行值求值"
    );
}

#[test]
fn rename_table_column_updates_definition_and_formulas_and_preserves_ids() {
    let mut wb = table_book();
    let sh = &mut wb.sheets[0];
    sh.set(
        2,
        2,
        Cell {
            value: Value::Empty,
            formula: Some("=SUM(Sales[Amount])".into()),
        },
    );
    sh.set(
        3,
        2,
        Cell {
            value: Value::Empty,
            formula: Some("=SUM(sales[Amount])".into()),
        },
    );
    sh.set(
        3,
        3,
        Cell {
            value: Value::Empty,
            formula: Some(r#"="sales[Amount]""#.into()),
        },
    );

    assert!(rename_table_column(&mut wb, "Sales", "Amount", "Qty"));
    let sh = &wb.sheets[0];
    assert_eq!(sh.tables[0].columns, vec!["Qty", "Tax", "Total"]);
    assert_eq!(sh.tables[0].column_ids, vec![11, 12, 13]);
    assert_eq!(
        sh.get(0, 0).map(|c| c.value.clone()),
        Some(Value::Text("Qty".into())),
        "表头单元格必须同步改名，不能只改表定义"
    );
    assert_eq!(
        sh.formula_of(1, 2),
        Some("=[@Qty]*[@Tax]")
    );
    assert_eq!(
        sh.formula_of(2, 2),
        Some("=SUM(Sales[Qty])")
    );
    assert_eq!(
        sh.formula_of(3, 2),
        Some("=SUM(sales[Qty])"),
        "表名匹配不能区分大小写，且应保留原文大小写"
    );
    assert_eq!(
        sh.formula_of(3, 3),
        Some(r#"="sales[Amount]""#),
        "字符串里的 sales[Amount] 不是表格引用"
    );
}

#[test]
fn renamed_table_column_and_ids_survive_save_and_reload() {
    let mut wb = table_book();
    let sh = &mut wb.sheets[0];
    sh.set(
        1,
        2,
        Cell {
            value: Value::Empty,
            formula: Some("=[@Amount]*[@Tax]".into()),
        },
    );
    assert!(rename_table_column(&mut wb, "Sales", "Amount", "Qty"));
    let sh = &mut wb.sheets[0];
    sh.tables_dirty = true;
    let dir = std::env::temp_dir().join("sheets_native_c102");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("table_c102_{}.xlsx", std::process::id()));
    let _ = std::fs::remove_file(&path);
    sheetcore::xlsx::save(&path, &wb).expect("save");
    let back = sheetcore::xlsx::load(&path).expect("load");
    let t = &back.sheets[0].tables[0];
    assert_eq!(t.columns, vec!["Qty", "Tax", "Total"]);
    assert_eq!(t.column_ids, vec![11, 12, 13], "稳定列 id 必须原样往返");
    assert_eq!(
        back.sheets[0].get(0, 0).map(|c| c.value.clone()),
        Some(Value::Text("Qty".into())),
        "保存重开后表头必须与列定义一致"
    );
    assert_eq!(
        back.sheets[0].formula_of(1, 2),
        Some("=[@Qty]*[@Tax]")
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn converting_table_to_range_keeps_formulas_usable_after_reopen() {
    let mut wb = table_book();
    assert!(convert_table_to_range(&mut wb, "Sales"));
    let sh = &wb.sheets[0];
    assert!(sh.tables.is_empty(), "转换后表格对象应删除");
    assert!(sh.tables_dirty, "删除表格必须触发表格部件同步");
    assert_eq!(
        sh.formula_of(1, 2),
        Some("=A2*B2"),
        "当前行结构化引用应展开为 A1"
    );
    recalc_all_force(&mut wb);
    assert_eq!(
        wb.sheets[0].get(1, 2).map(|c| c.value.clone()),
        Some(Value::Number(10.0)),
        "转换后公式仍应计算"
    );
    let dir = std::env::temp_dir().join("sheets_native_c102");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("table_convert_{}.xlsx", std::process::id()));
    let _ = std::fs::remove_file(&path);
    sheetcore::xlsx::save(&path, &wb).expect("save");
    let back = sheetcore::xlsx::load(&path).expect("load");
    assert!(back.sheets[0].tables.is_empty(), "重开后不应再有表格部件");
    assert_eq!(
        back.sheets[0].formula_of(1, 2),
        Some("=A2*B2")
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn calculated_column_fill_copies_relative_formulas_to_other_rows() {
    let mut wb = table_book();
    let sh = &mut wb.sheets[0];
    sh.tables[0].range = (0, 0, 3, 2);
    for r in 2..=3 {
        sh.set(r, 0, Cell { value: Value::Number(100.0 * r as f64), formula: None });
        sh.set(r, 1, Cell { value: Value::Number(0.1), formula: None });
    }
    sh.set(
        1,
        2,
        Cell {
            value: Value::Empty,
            formula: Some("=A2*B2".into()),
        },
    );
    assert_eq!(fill_calculated_column(sh, "Sales", "Total"), 2);
    assert_eq!(sh.formula_of(2, 2), Some("=A3*B3"));
    assert_eq!(sh.formula_of(3, 2), Some("=A4*B4"));
    recalc_all_force(&mut wb);
    let sh = &wb.sheets[0];
    assert_eq!(sh.get(2, 2).map(|c| c.value.clone()), Some(Value::Number(20.0)));
    assert_eq!(sh.get(3, 2).map(|c| c.value.clone()), Some(Value::Number(30.0)));
}
#[test]
fn converting_one_table_leaves_other_tables_structured_refs_untouched() {
    let mut wb = table_book();
    {
        let sh = &mut wb.sheets[0];
        sh.tables.push(Table {
            name: "Other".into(),
            display: "Other".into(),
            range: (0, 4, 2, 5),
            columns: vec!["Amount".into(), "Out".into()],
            column_ids: vec![21, 22],
            header_row: true,
            totals_row: false,
            ..Default::default()
        });
        sh.set(0, 4, Cell { value: Value::Text("Amount".into()), formula: None });
        sh.set(0, 5, Cell { value: Value::Text("Out".into()), formula: None });
        sh.set(1, 4, Cell { value: Value::Number(3.0), formula: None });
        sh.set(
            1,
            5,
            Cell {
                value: Value::Empty,
                formula: Some("=[@Amount]*2".into()),
            },
        );
        sh.set(2, 4, Cell { value: Value::Number(4.0), formula: None });
        sh.set(
            2,
            5,
            Cell {
                value: Value::Empty,
                formula: Some("=SUM(Other[Amount])".into()),
            },
        );
        // 目标表的显式引用应该转换，非目标表的裸/显式引用与字符串必须保留。
        sh.set(
            3,
            0,
            Cell {
                value: Value::Empty,
                formula: Some("=SUM(Sales[Amount])".into()),
            },
        );
        sh.set(
            4,
            0,
            Cell {
                value: Value::Empty,
                formula: Some(r#"="Sales[Amount]""#.into()),
            },
        );
    }
    assert!(convert_table_to_range(&mut wb, "Sales"));
    let sh = &wb.sheets[0];
    assert_eq!(
        sh.formula_of(1, 5),
        Some("=[@Amount]*2"),
        "转换 Sales 不能把 Other 内部的裸当前行改成 VALUE"
    );
    assert_eq!(
        sh.formula_of(2, 5),
        Some("=SUM(Other[Amount])"),
        "转换 Sales 不能把 Other 的显式引用改成固定范围"
    );
    assert_eq!(
        sh.formula_of(4, 0),
        Some(r#"="Sales[Amount]""#),
        "字符串里的目标表名不能改"
    );
    let target = sh.formula_of(3, 0).unwrap();
    assert!(!target.contains("Sales["), "目标表显式引用应展开：{target}");
    recalc_all_force(&mut wb);
    let sh = &wb.sheets[0];
    assert_eq!(sh.get(1, 5).map(|c| c.value.clone()), Some(Value::Number(6.0)));
    assert_eq!(sh.get(2, 5).map(|c| c.value.clone()), Some(Value::Number(7.0)));
    assert_eq!(sh.get(3, 0).map(|c| c.value.clone()), Some(Value::Number(100.0)));
    assert_eq!(sh.get(1, 2).map(|c| c.value.clone()), Some(Value::Number(10.0)));
}
#[test]
fn rename_table_column_does_not_panic_on_chinese_formula_slices() {
    let mut wb = Workbook::new_blank();
    {
        let sh = &mut wb.sheets[0];
        sh.name = "Data".into();
        sh.tables.push(Table {
            name: "Sales".into(),
            display: "Sales".into(),
            range: (0, 0, 1, 2),
            columns: vec!["金额".into(), "税率".into(), "小计".into()],
            column_ids: vec![1, 2, 3],
            header_row: true,
            totals_row: false,
            ..Default::default()
        });
        for (c, h) in ["金额", "税率", "小计"].iter().enumerate() {
            sh.set(0, c as u32, Cell { value: Value::Text((*h).into()), formula: None });
        }
        sh.set(1, 0, Cell { value: Value::Number(100.0), formula: None });
        sh.set(1, 1, Cell { value: Value::Number(0.1), formula: None });
        sh.set(
            1,
            2,
            Cell {
                value: Value::Empty,
                formula: Some("=[@金额]*[@税率]".into()),
            },
        );
        // 无关中文公式/字符串：rename 遍历全簿时也不能因 UTF-8 切片崩溃或被改。
        sh.set(
            2,
            3,
            Cell {
                value: Value::Empty,
                formula: Some("=SUM(金额)".into()),
            },
        );
        sh.set(
            3,
            3,
            Cell {
                value: Value::Empty,
                formula: Some(r#"="金额""#.into()),
            },
        );
    }
    assert!(rename_table_column(&mut wb, "Sales", "金额", "含税金额"));
    let sh = &wb.sheets[0];
    assert_eq!(
        sh.get(0, 0).map(|c| c.value.clone()),
        Some(Value::Text("含税金额".into()))
    );
    assert_eq!(
        sh.formula_of(1, 2),
        Some("=[@含税金额]*[@税率]")
    );
    assert_eq!(
        sh.formula_of(2, 3),
        Some("=SUM(金额)"),
        "无关中文公式必须原样保留"
    );
    assert_eq!(
        sh.formula_of(3, 3),
        Some(r#"="金额""#)
    );
}
