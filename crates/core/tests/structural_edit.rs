//! 插入/删除行列：值、合并、批注、条件格式、数据验证、冻结都要跟着走
use sheetcore::model::{Cell, Value};
use sheetcore::structural::Structural;

fn sheet_with_data() -> sheetcore::model::Sheet {
    let mut s = sheetcore::model::Sheet::new("测试");
    for r in 0..4u32 {
        s.set(r, 0, Cell { value: Value::Text(format!("R{r}").into()), formula: None });
        s.set(r, 1, Cell { value: Value::Number(r as f64 * 10.0), formula: None });
    }
    s.merge_range(1, 0, 1, 1);
    s
}

#[test]
fn insert_row_shifts_down() {
    let mut s = sheet_with_data();
    s.apply_structural(&Structural::InsertRows { at: 1, n: 1 });
    assert_eq!(s.get(0, 0).unwrap().value.display(), "R0");
    assert!(s.get(1, 0).is_none(), "插入的空行应该是空的");
    assert_eq!(s.get(2, 0).unwrap().value.display(), "R1");
    assert_eq!(s.get(4, 0).unwrap().value.display(), "R3");
    assert_eq!(s.get(2, 1).unwrap().value.display(), "10");
    // 合并区域跟着下移
    assert_eq!(s.merges, vec![(2, 0, 2, 1)]);
}

#[test]
fn dense_shift_journals_only_real_style_changes_and_vacancies() {
    use sheetcore::model::{CellRef, Sheet};
    let mut sheet = Sheet::new("S");
    for row in 0..1024 {
        sheet.set(row, 1, Cell { value: Value::Number(row as f64), formula: None });
        sheet.set_style_loaded(row, 1, 3);
    }
    // The tail contains a style-only blank with the same style as the old value.
    sheet.set_style_loaded(1024, 1, 3);
    sheet.apply_structural(&Structural::DeleteRows { at: 0, n: 1 });
    assert_eq!(sheet.style_edits, [CellRef::new(1023, 1), CellRef::new(1024, 1)].into());
    assert_eq!(sheet.cleared, [CellRef::new(1023, 1)].into());
    assert_eq!(sheet.style_of(1023, 1), 3);
    assert_eq!(sheet.get(1022, 1).unwrap().value, Value::Number(1023.0));
    sheet.apply_structural(&Structural::InsertCols { at: 0, n: 2 });
    assert_eq!(sheet.style_of(1023, 3), 3);
    assert_eq!(sheet.get(1022, 3).unwrap().value, Value::Number(1023.0));
}

#[test]
fn delete_row_removes_and_pulls_up() {
    let mut s = sheet_with_data();
    s.apply_structural(&Structural::DeleteRows { at: 1, n: 1 });
    assert_eq!(s.get(1, 0).unwrap().value.display(), "R2");
    assert_eq!(s.get(2, 0).unwrap().value.display(), "R3");
    assert!(s.get(3, 0).is_none());
    // 合并区域被删掉的行覆盖，应消失
    assert!(s.merges.is_empty(), "整块被删的合并要清掉");
}

#[test]
fn insert_and_delete_cols() {
    let mut s = sheet_with_data();
    s.apply_structural(&Structural::InsertCols { at: 0, n: 2 });
    assert_eq!(s.get(0, 2).unwrap().value.display(), "R0");
    assert!(s.get(0, 0).is_none());
    s.apply_structural(&Structural::DeleteCols { at: 0, n: 2 });
    assert_eq!(s.get(0, 0).unwrap().value.display(), "R0");
    assert_eq!(s.get(1, 1).unwrap().value.display(), "10");
}

#[test]
fn freeze_and_notes_follow_rows() {
    let mut s = sheet_with_data();
    s.freeze = Some((2, 1));
    s.notes.insert(
        sheetcore::model::CellRef::new(3, 0),
        sheetcore::notes::Note { r: 3, c: 0, text: "备注".into(), author: "我".into() },
    );
    s.apply_structural(&Structural::InsertRows { at: 0, n: 2 });
    assert_eq!(s.freeze, Some((4, 1)), "冻结行要跟着挪");
    assert!(s.notes.contains_key(&sheetcore::model::CellRef::new(5, 0)), "批注要跟着走");
    assert_eq!(s.get(5, 0).unwrap().value.display(), "R3");
}

#[test]
fn row_heights_shift() {
    let mut s = sheet_with_data();
    s.rows.set(1, 40.0);
    let before = s.rows.size(1);
    s.apply_structural(&Structural::InsertRows { at: 0, n: 1 });
    assert_eq!(s.rows.size(2), before, "自定义行高要移到新行");
}

#[test]
fn structure_only_leaves_cells_alone() {
    // 加入房间时用：格子从快照来，结构只搬合并且不能动值
    let mut s = sheet_with_data();
    s.apply_structural_structure_only(&Structural::InsertRows { at: 0, n: 2 });
    assert_eq!(s.get(0, 0).unwrap().value.display(), "R0", "格子不该被搬");
    assert_eq!(s.merges, vec![(3, 0, 3, 1)], "合并要跟着挪两行");
}

#[test]
fn formulas_are_rewritten_on_insert() {
    let mut wb = sheetcore::model::Workbook::new_blank();
    wb.sheets[0].name = "S".into();
    for r in 0..3u32 {
        wb.sheet_mut(0).set(r, 0, Cell { value: Value::Number((r + 1) as f64), formula: None });
    }
    wb.sheet_mut(0).set(
        0,
        1,
        Cell { value: Value::Number(0.0), formula: Some("=SUM(A1:A3)".into()) },
    );
    sheetcore::structural::apply_structural_wb(
        &mut wb,
        0,
        &Structural::InsertRows { at: 0, n: 1 },
    );
    // 公式本身跟着走：B1 → B2，区间 A1:A3 → A2:A4
    let f = wb.sheet(0).get(1, 1).and_then(|c| c.formula.clone());
    assert_eq!(f.as_deref(), Some("=SUM(A2:A4)"), "引用没改写");
    assert!(wb.sheet(0).get(1, 0).unwrap().value.display() == "1");
}

#[test]
fn delete_turns_reference_into_ref_error() {
    let mut wb = sheetcore::model::Workbook::new_blank();
    wb.sheets[0].name = "S".into();
    wb.sheet_mut(0).set(0, 0, Cell { value: Value::Number(5.0), formula: None });
    wb.sheet_mut(0).set(1, 1, Cell { value: Value::Number(0.0), formula: Some("=A1*2".into()) });
    sheetcore::structural::apply_structural_wb(&mut wb, 0, &Structural::DeleteRows { at: 0, n: 1 });
    // A1 被删 → 公式变成 #REF!*2，且公式自己也上移到 B1
    let f = wb.sheet(0).get(0, 1).and_then(|c| c.formula.clone());
    assert_eq!(f.as_deref(), Some("=#REF!*2"), "应变成 #REF!，实际 {f:?}");
}

#[test]
fn cross_sheet_references_follow() {
    let mut wb = sheetcore::model::Workbook::new_blank();
    wb.sheets[0].name = "数据".into();
    wb.sheets.push(sheetcore::model::Sheet::new("汇总"));
    wb.sheet_mut(0).set(0, 0, Cell { value: Value::Number(9.0), formula: None });
    wb.sheet_mut(1).set(0, 0, Cell {
        value: Value::Number(0.0),
        formula: Some("=数据!A1+1".into()),
    });
    sheetcore::structural::apply_structural_wb(
        &mut wb,
        0,
        &Structural::InsertRows { at: 0, n: 2 },
    );
    let f = wb.sheet(1).get(0, 0).and_then(|c| c.formula.clone());
    assert_eq!(f.as_deref(), Some("=数据!A3+1"), "跨表引用应跟着改");
}
