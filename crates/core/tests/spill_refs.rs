//! C11.2：动态数组溢出引用 `A1#`。

use sheetcore::formula::{eval_formula, recalc_all_force, FValue};
use sheetcore::model::{Cell, Value, Workbook};

fn f(text: &str) -> Cell {
    Cell { value: Value::Empty, formula: Some(text.into()) }
}

#[test]
fn spill_reference_tracks_anchor_array_and_shrinks() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Data".into();
    wb.sheets[0].set(0, 0, f("=SEQUENCE(3,2)"));
    wb.sheets[0].set(0, 2, f("=SUM(A1#)"));
    wb.sheets[0].set(0, 3, f("=SUM(Data!A1#)"));
    recalc_all_force(&mut wb);
    match eval_formula(&wb, 0, "=A1#") {
        FValue::Array(a) => {
            assert_eq!((a.rows, a.cols), (3, 2));
            assert_eq!(a.get(2, 0), FValue::Number(5.0));
        }
        other => panic!("expected spill array, got {other:?}"),
    }
    assert_eq!(wb.sheets[0].value(0, 2), Value::Number(21.0));
    assert_eq!(wb.sheets[0].value(0, 3), Value::Number(21.0));
    assert_eq!(wb.sheets[0].value(2, 0), Value::Number(5.0));
    assert!(wb.sheets[0].spilled.contains(&sheetcore::model::CellRef::new(2, 0)));

    // 源数组缩小：旧尾格必须清掉，A1# 只覆盖新区域。
    wb.sheets[0].set(0, 0, f("=SEQUENCE(2,1)"));
    recalc_all_force(&mut wb);
    assert_eq!(wb.sheets[0].value(0, 2), Value::Number(3.0));
    assert!(wb.sheets[0].get(2, 0).is_none(), "旧溢出尾格应被清掉");

    // 再扩大：A1# 必须覆盖新尾格，SUM 同步变化。
    wb.sheets[0].set(0, 0, f("=SEQUENCE(4,1)"));
    recalc_all_force(&mut wb);
    assert_eq!(wb.sheets[0].value(0, 2), Value::Number(10.0));
    assert_eq!(wb.sheets[0].value(3, 0), Value::Number(4.0));
}

#[test]
fn spill_on_non_array_is_ref_error() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(0, 0, Cell { value: Value::Number(5.0), formula: None });
    match eval_formula(&wb, 0, "=SUM(A1#)") {
        FValue::Error(sheetcore::formula::ErrKind::Ref) => {}
        other => panic!("expected #REF!, got {other:?}"),
    }
}

#[test]
fn spill_ref_moves_on_row_insert_and_dependency_is_dynamic() {
    let mut shift = sheetcore::refs::Shift {
        axis: sheetcore::refs::Axis::Row,
        at: 0,
        n: 1,
        ..Default::default()
    };
    shift.target_sheet = "Data".into();
    shift.own_sheet = "Data".into();
    assert_eq!(sheetcore::refs::rewrite("=SUM(A1#)", &shift), "=SUM(A2#)");

    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(0, 0, f("=SEQUENCE(2,1)"));
    wb.sheets[0].set(0, 1, f("=SUM(A1#)"));
    let deps = sheetcore::formula::deps::Deps::build(&wb);
    assert!(deps.requires_full_recalc(), "spill 区域随源数组变化，必须保守全量重算");
}
