//! C11.2：动态数组溢出引用 `A1#`。

use sheetcore::formula::{eval_formula, recalc_all_force, recalc_sheet, FValue};
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

#[test]
fn cross_sheet_spill_is_available_when_only_dependent_sheet_is_recalculated() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Data".into();
    wb.sheets[0].set(0, 0, f("=SEQUENCE(3,1)"));
    let mut other = sheetcore::model::Sheet::new("Other");
    other.set(0, 0, f("=SUM(Data!A1#)"));
    wb.sheets.push(other);
    let mut noise = sheetcore::model::Sheet::new("Noise");
    noise.set(
        0,
        0,
        Cell {
            value: Value::Number(0.42),
            formula: Some("=RAND()".into()),
        },
    );
    wb.sheets.push(noise);
    recalc_all_force(&mut wb);
    assert_eq!(wb.sheets[1].value(0, 0), Value::Number(6.0));
    // 全量重算会更新 RAND；给无关表一个可识别的缓存，再验证逐表-only 不碰它。
    wb.sheets[2].set(
        0,
        0,
        Cell {
            value: Value::Number(0.42),
            formula: Some("=RAND()".into()),
        },
    );

    // 只算 Other 时不能重算 Data 的 RAND，也不能把跨表锚点当成标量。
    assert!(recalc_sheet(&mut wb, 1) > 0);
    assert_eq!(wb.sheets[1].value(0, 0), Value::Number(6.0));
    assert_eq!(wb.sheets[0].value(2, 0), Value::Number(3.0));
    assert_eq!(
        wb.sheets[2].value(0, 0),
        Value::Number(0.42),
        "只算 Other 不得重算无关表的 RAND"
    );

    wb.sheets[0].set(0, 0, f("=SEQUENCE(2,1)"));
    recalc_sheet(&mut wb, 0);
    recalc_sheet(&mut wb, 1);
    assert_eq!(wb.sheets[1].value(0, 0), Value::Number(3.0));

    wb.sheets[0].set(0, 0, f("=SEQUENCE(4,1)"));
    recalc_sheet(&mut wb, 0);
    recalc_sheet(&mut wb, 1);
    assert_eq!(wb.sheets[1].value(0, 0), Value::Number(10.0));
    assert_eq!(wb.sheets[0].value(3, 0), Value::Number(4.0));
}

#[test]
fn cross_sheet_spill_uses_recorded_anchor_shape_not_tail_guess() {
    // 1×1 数组没有尾格，仍必须记录锚点形状并在逐表重算时可用。
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Data".into();
    wb.sheets[0].set(0, 0, f("=SEQUENCE(1,1)"));
    let mut other = sheetcore::model::Sheet::new("Other");
    other.set(0, 0, f("=SUM(Data!A1#)"));
    wb.sheets.push(other);
    recalc_all_force(&mut wb);
    assert_eq!(wb.sheets[1].value(0, 0), Value::Number(1.0));
    recalc_sheet(&mut wb, 1);
    assert_eq!(wb.sheets[1].value(0, 0), Value::Number(1.0));

    // 3×1 的尾格不是锚点；A2# 在全量和逐表都必须 #REF!，不能返回 5。
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Data".into();
    wb.sheets[0].set(0, 0, f("=SEQUENCE(3,1)"));
    let mut other = sheetcore::model::Sheet::new("Other");
    other.set(0, 0, f("=SUM(Data!A2#)"));
    other.set(1, 0, f("=SUM(Data!B1#)"));
    wb.sheets.push(other);
    recalc_all_force(&mut wb);
    for value in [wb.sheets[1].value(0, 0), wb.sheets[1].value(1, 0)] {
        assert_eq!(value, Value::Error(sheetcore::formula::ErrKind::Ref));
    }
    recalc_sheet(&mut wb, 1);
    for value in [wb.sheets[1].value(0, 0), wb.sheets[1].value(1, 0)] {
        assert_eq!(value, Value::Error(sheetcore::formula::ErrKind::Ref));
    }
}

#[test]
fn spill_external_book_does_not_fall_back_to_local_same_named_sheet() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Data".into();
    wb.sheets[0].set(0, 0, f("=SEQUENCE(3,1)"));
    recalc_all_force(&mut wb);
    match eval_formula(&wb, 0, "=SUM('[Other.xlsx]Data'!A1#)") {
        FValue::Error(sheetcore::formula::ErrKind::Ref) => {}
        other => panic!("外部书名不能引用本簿同名表，应得 #REF!，got {other:?}"),
    }
    match eval_formula(&wb, 0, "=BYROW('[Other.xlsx]Data'!A1#,LAMBDA(r,SUM(r)))") {
        FValue::Error(sheetcore::formula::ErrKind::Ref) => {}
        other => panic!("grid_of 对 Spill 外部书名也必须 #REF!，got {other:?}"),
    }
}

#[test]
fn spill_inside_byrow_rejects_scalars_and_propagates_errors() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(0, 0, f("=SEQUENCE(2,2)"));
    match eval_formula(&wb, 0, "=BYROW(A1#,LAMBDA(r,SUM(r)))") {
        FValue::Array(a) => {
            assert_eq!((a.rows, a.cols), (2, 1));
            assert_eq!(a.get(0, 0), FValue::Number(3.0));
            assert_eq!(a.get(1, 0), FValue::Number(7.0));
        }
        other => panic!("expected 2x1 BYROW array, got {other:?}"),
    }

    let mut ordinary = Workbook::new_blank();
    ordinary.sheets[0].set(0, 0, Cell { value: Value::Number(5.0), formula: None });
    match eval_formula(&ordinary, 0, "=BYROW(A1#,LAMBDA(r,SUM(r)))") {
        FValue::Error(sheetcore::formula::ErrKind::Ref) => {}
        other => panic!("ordinary A1# in BYROW must be #REF!, got {other:?}"),
    }

    let mut error = Workbook::new_blank();
    error.sheets[0].set(0, 0, f("=1/0"));
    match eval_formula(&error, 0, "=SUM(A1#)") {
        FValue::Error(sheetcore::formula::ErrKind::Div0) => {}
        other => panic!("spill of error anchor must propagate #DIV/0!, got {other:?}"),
    }
}
