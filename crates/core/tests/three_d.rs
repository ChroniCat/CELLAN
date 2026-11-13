//! C11.4：三维引用 `Sheet1:Sheet3!A1` 的跨表求值。

use sheetcore::formula::{eval_formula, recalc_all_force, FValue};
use sheetcore::model::{Cell, Sheet, Value, Workbook};

fn f(text: &str) -> Cell {
    Cell { value: Value::Empty, formula: Some(text.into()) }
}

fn num(v: FValue) -> f64 {
    match v {
        FValue::Number(n) => n,
        other => panic!("expected number, got {other:?}"),
    }
}

fn book3() -> Workbook {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "First".into();
    wb.sheets[0].set(0, 0, Cell { value: Value::Number(10.0), formula: None });
    wb.sheets[0].set(0, 1, Cell { value: Value::Number(1.0), formula: None });
    for (name, a, b) in [("Second", 20.0, 2.0), ("Third", 30.0, 3.0)] {
        let mut sh = Sheet::new(name);
        sh.set(0, 0, Cell { value: Value::Number(a), formula: None });
        sh.set(0, 1, Cell { value: Value::Number(b), formula: None });
        wb.sheets.push(sh);
    }
    wb.sheets.push(Sheet::new("Other"));
    wb
}

#[test]
fn three_d_cell_reference_sums_every_sheet_in_order() {
    let mut wb = book3();
    let other = wb.sheets.len() - 1;
    wb.sheets[other].set(0, 0, f("=SUM(First:Third!A1)"));
    wb.sheets[other].set(1, 0, f("=SUM(First:Third!A1:B1)"));
    recalc_all_force(&mut wb);
    assert_eq!(wb.sheets[other].value(0, 0), Value::Number(60.0));
    assert_eq!(wb.sheets[other].value(1, 0), Value::Number(66.0));
    assert_eq!(num(eval_formula(&wb, other, "=SUM(First:Third!A1)")), 60.0);
}

#[test]
fn renamed_endpoint_updates_three_d_formula() {
    let mut wb = book3();
    let other = wb.sheets.len() - 1;
    wb.sheets[other].set(0, 0, f("=SUM(First:Third!A1)"));
    assert!(sheetcore::sheet_ops::rename_sheet(&mut wb, 0, "Start"), "rename");
    assert_eq!(wb.sheets[other].get(0, 0).unwrap().formula.as_deref(), Some("=SUM(Start:Third!A1)"));
    recalc_all_force(&mut wb);
    assert_eq!(wb.sheets[other].value(0, 0), Value::Number(60.0));
}

#[test]
fn inserting_sheet_between_endpoints_is_included() {
    let mut wb = book3();
    let other = wb.sheets.len() - 1;
    wb.sheets[other].set(0, 0, f("=SUM(First:Third!A1)"));
    // 在 Second 之后插入（Third 之前），三维区间应自然包含新表。
    let mut sh = Sheet::new("Middle");
    sh.set(0, 0, Cell { value: Value::Number(40.0), formula: None });
    wb.sheets.insert(2, sh);
    recalc_all_force(&mut wb);
    assert_eq!(wb.sheets[other + 1].value(0, 0), Value::Number(100.0));
}

#[test]
fn refs_copy_keeps_three_d_endpoints_and_moves_cell() {
    assert_eq!(
        sheetcore::refs::translate("=SUM(First:Third!A1)", 1, 0, "Other"),
        "=SUM(First:Third!A2)"
    );
}

#[test]
fn deleting_endpoint_shrinks_three_d_reference_to_neighbor() {
    let mut wb = book3();
    let other = wb.sheets.len() - 1;
    wb.sheets[other].set(0, 0, f("=SUM(First:Third!A1)"));
    assert!(sheetcore::sheet_ops::delete_sheet(&mut wb, 0), "delete");
    let other = wb.sheets.len() - 1;
    // Excel 删 3D 起点会收缩到下一张表，不是把整个区间变成 #REF!。
    assert_eq!(wb.sheets[other].get(0, 0).unwrap().formula.as_deref(), Some("=SUM(Second:Third!A1)"));
    recalc_all_force(&mut wb);
    assert_eq!(wb.sheets[other].value(0, 0), Value::Number(50.0));
}

#[test]
fn moving_sheet_into_three_d_span_changes_membership() {
    let mut wb = book3();
    let other = wb.sheets.len() - 1;
    wb.sheets[other].set(0, 0, f("=SUM(First:Third!A1)"));
    let mut outside = Sheet::new("Outside");
    outside.set(0, 0, Cell { value: Value::Number(100.0), formula: None });
    wb.sheets.push(outside);
    // Outside 原在 Third 之后，移到 First 与 Third 之间。
    assert!(sheetcore::sheet_ops::move_sheet(&mut wb, 4, 1), "move");
    recalc_all_force(&mut wb);
    let other = wb.sheets.iter().position(|s| s.name == "Other").unwrap();
    // 交换后 Second 落到区间外，First:Third 只剩 First(10)+Outside(100)+Third(30)。
    assert_eq!(wb.sheets[other].value(0, 0), Value::Number(140.0));
}

#[test]
fn three_d_formula_survives_save_and_reload() {
    let mut wb = book3();
    let other = wb.sheets.len() - 1;
    wb.sheets[other].set(0, 0, f("=SUM(First:Third!A1)"));
    let path = std::env::temp_dir().join(format!("three_d_{}.xlsx", std::process::id()));
    sheetcore::xlsx::save(&path, &wb).expect("save");
    let mut back = sheetcore::xlsx::load(&path).expect("load");
    let _ = std::fs::remove_file(&path);
    let other = back.sheets.iter().position(|s| s.name == "Other").unwrap();
    assert_eq!(back.sheets[other].get(0, 0).unwrap().formula.as_deref(), Some("=SUM(First:Third!A1)"));
    recalc_all_force(&mut back);
    assert_eq!(back.sheets[other].value(0, 0), Value::Number(60.0));
}

#[test]
fn deleting_sheet_shrinks_three_d_range_and_keeps_cell_range() {
    let mut wb = book3();
    let other = wb.sheets.len() - 1;
    wb.sheets[other].set(0, 0, f("=SUM(First:Third!A1:B1)"));
    assert!(sheetcore::sheet_ops::delete_sheet(&mut wb, 0), "delete");
    let other = wb.sheets.len() - 1;
    assert_eq!(
        wb.sheets[other].get(0, 0).unwrap().formula.as_deref(),
        Some("=SUM(Second:Third!A1:B1)")
    );
    recalc_all_force(&mut wb);
    assert_eq!(wb.sheets[other].value(0, 0), Value::Number(55.0));
}

#[test]
fn quoted_three_d_sheet_names_are_parsed_and_renamed() {
    let mut wb = book3();
    let other = wb.sheets.len() - 1;
    assert!(sheetcore::sheet_ops::rename_sheet(&mut wb, 0, "First 1"));
    assert!(sheetcore::sheet_ops::rename_sheet(&mut wb, 2, "Third 3"));
    wb.sheets[other].set(0, 0, f("=SUM('First 1':'Third 3'!A1)"));
    recalc_all_force(&mut wb);
    assert_eq!(wb.sheets[other].value(0, 0), Value::Number(60.0));
}

#[test]
fn whole_quoted_three_d_range_renames_endpoints_after_formula_is_set() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Start 1".into();
    wb.sheets[0].set(0, 0, Cell { value: Value::Number(10.0), formula: None });
    let mut middle = Sheet::new("Middle");
    middle.set(0, 0, Cell { value: Value::Number(20.0), formula: None });
    wb.sheets.push(middle);
    let mut end = Sheet::new("End 3");
    end.set(0, 0, Cell { value: Value::Number(30.0), formula: None });
    wb.sheets.push(end);
    let mut other = Sheet::new("Other");
    other.set(0, 0, f("=SUM('Start 1:End 3'!A1)"));
    wb.sheets.push(other);
    recalc_all_force(&mut wb);
    assert_eq!(wb.sheets[3].value(0, 0), Value::Number(60.0));

    assert!(sheetcore::sheet_ops::rename_sheet(&mut wb, 0, "Begin"));
    assert_eq!(wb.sheets[3].get(0, 0).unwrap().formula.as_deref(), Some("=SUM('Begin:End 3'!A1)"));
    recalc_all_force(&mut wb);
    assert_eq!(wb.sheets[3].value(0, 0), Value::Number(60.0));

    assert!(sheetcore::sheet_ops::rename_sheet(&mut wb, 2, "Finish"));
    assert_eq!(wb.sheets[3].get(0, 0).unwrap().formula.as_deref(), Some("=SUM('Begin:Finish'!A1)"));
    recalc_all_force(&mut wb);
    assert_eq!(wb.sheets[3].value(0, 0), Value::Number(60.0));
}

#[test]
fn whole_quoted_three_d_range_handles_escaped_apostrophes() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Start 1's".into();
    wb.sheets[0].set(0, 0, Cell { value: Value::Number(10.0), formula: None });
    let mut middle = Sheet::new("Middle");
    middle.set(0, 0, Cell { value: Value::Number(20.0), formula: None });
    wb.sheets.push(middle);
    let mut end = Sheet::new("End 3");
    end.set(0, 0, Cell { value: Value::Number(30.0), formula: None });
    wb.sheets.push(end);
    let mut other = Sheet::new("Other");
    other.set(0, 0, f("=SUM('Start 1''s:End 3'!A1)"));
    wb.sheets.push(other);
    recalc_all_force(&mut wb);
    assert_eq!(wb.sheets[3].value(0, 0), Value::Number(60.0));
    assert!(sheetcore::sheet_ops::rename_sheet(&mut wb, 0, "Begin's"));
    assert_eq!(
        wb.sheets[3].get(0, 0).unwrap().formula.as_deref(),
        Some("=SUM('Begin''s:End 3'!A1)")
    );
    recalc_all_force(&mut wb);
    assert_eq!(wb.sheets[3].value(0, 0), Value::Number(60.0));
}

#[test]
fn deleting_unrelated_sheet_keeps_chinese_plain_and_quoted_references() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "表一".into();
    wb.sheets[0].set(0, 0, Cell { value: Value::Number(10.0), formula: None });
    let mut unrelated = Sheet::new("表二");
    unrelated.set(0, 0, Cell { value: Value::Number(99.0), formula: None });
    wb.sheets.push(unrelated);
    let mut data = Sheet::new("数据");
    data.set(0, 0, f("=表一!A1"));
    data.set(1, 0, f("='表一'!A1"));
    wb.sheets.push(data);
    assert!(sheetcore::sheet_ops::delete_sheet(&mut wb, 1));
    assert_eq!(wb.sheets[1].get(0, 0).unwrap().formula.as_deref(), Some("=表一!A1"));
    assert_eq!(wb.sheets[1].get(1, 0).unwrap().formula.as_deref(), Some("='表一'!A1"));
    recalc_all_force(&mut wb);
    assert_eq!(wb.sheets[1].value(0, 0), Value::Number(10.0));
    assert_eq!(wb.sheets[1].value(1, 0), Value::Number(10.0));
}

#[test]
fn deleting_chinese_three_d_start_uses_original_span_neighbor() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "甲".into();
    wb.sheets[0].set(0, 0, Cell { value: Value::Number(10.0), formula: None });
    let mut yi = Sheet::new("乙");
    yi.set(0, 0, Cell { value: Value::Number(20.0), formula: None });
    wb.sheets.push(yi);
    let mut bing = Sheet::new("丙");
    bing.set(0, 0, Cell { value: Value::Number(30.0), formula: None });
    wb.sheets.push(bing);
    let mut other = Sheet::new("其他");
    other.set(0, 0, f("=SUM(甲:丙!A1)"));
    other.set(1, 0, f("=SUM('甲':'丙'!A1)"));
    other.set(2, 0, f("=SUM('甲:丙'!A1)"));
    wb.sheets.push(other);
    assert!(sheetcore::sheet_ops::delete_sheet(&mut wb, 0));
    assert_eq!(wb.sheets[2].get(0, 0).unwrap().formula.as_deref(), Some("=SUM(乙:丙!A1)"));
    assert_eq!(wb.sheets[2].get(1, 0).unwrap().formula.as_deref(), Some("=SUM('乙':'丙'!A1)"));
    assert_eq!(wb.sheets[2].get(2, 0).unwrap().formula.as_deref(), Some("=SUM('乙:丙'!A1)"));
    recalc_all_force(&mut wb);
    for r in 0..3 {
        assert_eq!(wb.sheets[2].value(r, 0), Value::Number(50.0));
    }
}

#[test]
fn deleting_unique_member_of_three_d_span_is_whole_ref_error() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Second".into();
    wb.sheets[0].set(0, 0, Cell { value: Value::Number(20.0), formula: None });
    let mut other = Sheet::new("Other");
    other.set(0, 0, f("=SUM(Second:Second!A1)"));
    wb.sheets.push(other);
    assert!(sheetcore::sheet_ops::delete_sheet(&mut wb, 0));
    assert_eq!(wb.sheets[0].get(0, 0).unwrap().formula.as_deref(), Some("=SUM(#REF!)"));
    recalc_all_force(&mut wb);
    assert_eq!(wb.sheets[0].value(0, 0), Value::Error(sheetcore::formula::ErrKind::Ref));
}

fn reverse_three_d_book() -> Workbook {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Third".into();
    wb.sheets[0].set(0, 0, Cell { value: Value::Number(30.0), formula: None });
    let mut second = Sheet::new("Second");
    second.set(0, 0, Cell { value: Value::Number(20.0), formula: None });
    wb.sheets.push(second);
    let mut first = Sheet::new("First");
    first.set(0, 0, Cell { value: Value::Number(10.0), formula: None });
    wb.sheets.push(first);
    let mut other = Sheet::new("Other");
    other.set(0, 0, f("=SUM(Third:First!A1)"));
    wb.sheets.push(other);
    wb
}

#[test]
fn deleting_reverse_span_end_shrinks_toward_start() {
    let mut wb = reverse_three_d_book();
    assert!(sheetcore::sheet_ops::delete_sheet(&mut wb, 2));
    assert_eq!(wb.sheets[2].get(0, 0).unwrap().formula.as_deref(), Some("=SUM(Third:Second!A1)"));
    recalc_all_force(&mut wb);
    assert_eq!(wb.sheets[2].value(0, 0), Value::Number(50.0));
}

#[test]
fn deleting_reverse_span_start_shrinks_toward_end() {
    let mut wb = reverse_three_d_book();
    assert!(sheetcore::sheet_ops::delete_sheet(&mut wb, 0));
    assert_eq!(wb.sheets[2].get(0, 0).unwrap().formula.as_deref(), Some("=SUM(Second:First!A1)"));
    recalc_all_force(&mut wb);
    assert_eq!(wb.sheets[2].value(0, 0), Value::Number(30.0));
}
