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
