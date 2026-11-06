//! 动态数组函数测试：结果、溢出、#SPILL!

use sheetcore::formula::{self, FValue};
use sheetcore::{Cell, Value, Workbook};

fn num(v: f64) -> Cell {
    Cell { value: Value::Number(v), formula: None }
}

fn text(s: &str) -> Cell {
    Cell { value: Value::Text(s.into()), formula: None }
}

fn f(f: &str) -> Cell {
    Cell { value: Value::Empty, formula: Some(f.into()) }
}

fn run(wb: &mut Workbook, f: &str) -> FValue {
    formula::eval_formula(wb, 0, f)
}

#[test]
fn sequence_spills_down_and_right() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(0, 0, f("=SEQUENCE(3,2,1,1)"));
    formula::recalc_all(&mut wb);
    // 3 行 2 列：1 2 / 3 4 / 5 6
    assert_eq!(wb.sheets[0].value(0, 0), Value::Number(1.0));
    assert_eq!(wb.sheets[0].value(0, 1), Value::Number(2.0));
    assert_eq!(wb.sheets[0].value(1, 0), Value::Number(3.0));
    assert_eq!(wb.sheets[0].value(2, 1), Value::Number(6.0));
}

#[test]
fn spill_is_cleared_on_recalc() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(0, 0, f("=SEQUENCE(3,1)"));
    formula::recalc_all(&mut wb);
    assert_eq!(wb.sheets[0].value(2, 0), Value::Number(3.0));
    // 改成 1 行，原来溢出到 A2/A3 的值必须消失
    wb.sheets[0].set(0, 0, f("=SEQUENCE(1,1)"));
    formula::recalc_all(&mut wb);
    assert_eq!(wb.sheets[0].value(2, 0), Value::Empty);
}

#[test]
fn spill_blocked_by_existing_data() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(1, 0, text("占用"));
    wb.sheets[0].set(0, 0, f("=SEQUENCE(3,1)"));
    formula::recalc_all(&mut wb);
    assert_eq!(wb.sheets[0].value(0, 0), Value::Text("#SPILL!".into()));
    assert_eq!(wb.sheets[0].value(1, 0), Value::Text("占用".into()));
}

#[test]
fn transpose_and_mmult() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(0, 0, num(1.0));
    wb.sheets[0].set(0, 1, num(2.0));
    wb.sheets[0].set(1, 0, num(3.0));
    wb.sheets[0].set(1, 1, num(4.0));

    match run(&mut wb, "=TRANSPOSE(A1:B2)") {
        FValue::Array(a) => {
            assert_eq!(a.rows, 2);
            assert_eq!(a.cols, 2);
            assert_eq!(a.get(0, 1), FValue::Number(3.0));
        }
        other => panic!("{other:?}"),
    }
    match run(&mut wb, "=MMULT(A1:B2,A1:B2)") {
        FValue::Array(a) => {
            assert_eq!(a.get(0, 0), FValue::Number(7.0)); // 1*1+2*3
            assert_eq!(a.get(1, 1), FValue::Number(22.0)); // 3*2+4*4
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn filter_sort_unique() {
    let mut wb = Workbook::new_blank();
    let data = [("apple", 10.0), ("pear", 30.0), ("apple", 20.0), ("fig", 5.0)];
    for (i, (name, qty)) in data.iter().enumerate() {
        wb.sheets[0].set(i as u32, 0, text(name));
        wb.sheets[0].set(i as u32, 1, num(*qty));
    }

    // 数量 > 10 的行
    match run(&mut wb, "=FILTER(A1:B4,B1:B4>10)") {
        FValue::Array(a) => {
            assert_eq!(a.rows, 2, "应该只剩两行");
            assert_eq!(a.get(0, 0), FValue::Text("pear".into()));
            assert_eq!(a.get(1, 0), FValue::Text("apple".into()));
        }
        other => panic!("FILTER 返回 {other:?}"),
    }

    // 按第 2 列降序
    match run(&mut wb, "=SORT(A1:B4,2,-1)") {
        FValue::Array(a) => {
            assert_eq!(a.get(0, 0), FValue::Text("pear".into()));
            assert_eq!(a.get(3, 0), FValue::Text("fig".into()));
        }
        other => panic!("SORT 返回 {other:?}"),
    }

    // 去重（按行）
    match run(&mut wb, "=UNIQUE(A1:A4)") {
        FValue::Array(a) => {
            assert_eq!(a.rows, 3);
            assert_eq!(a.get(0, 0), FValue::Text("apple".into()));
        }
        other => panic!("UNIQUE 返回 {other:?}"),
    }
}

#[test]
fn take_drop_stack() {
    let mut wb = Workbook::new_blank();
    for i in 0..4u32 {
        wb.sheets[0].set(i, 0, num((i + 1) as f64));
    }
    match run(&mut wb, "=TAKE(A1:A4,2)") {
        FValue::Array(a) => {
            assert_eq!(a.rows, 2);
            assert_eq!(a.get(1, 0), FValue::Number(2.0));
        }
        other => panic!("TAKE {other:?}"),
    }
    match run(&mut wb, "=DROP(A1:A4,3)") {
        FValue::Array(a) => {
            assert_eq!(a.rows, 1);
            assert_eq!(a.get(0, 0), FValue::Number(4.0));
        }
        other => panic!("DROP {other:?}"),
    }
    match run(&mut wb, "=HSTACK(A1:A2,A3:A4)") {
        FValue::Array(a) => {
            assert_eq!((a.rows, a.cols), (2, 2));
            assert_eq!(a.get(1, 1), FValue::Number(4.0));
        }
        other => panic!("HSTACK {other:?}"),
    }
}

#[test]
fn let_binds_names() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(0, 0, num(10.0));
    match run(&mut wb, "=LET(x,A1*2,y,x+5,y*2)") {
        FValue::Number(n) => assert_eq!(n, 50.0),
        other => panic!("LET {other:?}"),
    }
    // 名字作用域不泄漏
    assert!(matches!(run(&mut wb, "=x+1"), FValue::Error(_)));
}

#[test]
fn spills_survive_save_and_reload() {
    let dir = std::env::temp_dir().join("sheets_native_test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("spill_roundtrip.xlsx");
    let _ = std::fs::remove_file(&path);

    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(0, 0, f("=SEQUENCE(3,2)"));
    formula::recalc_all(&mut wb);
    sheetcore::xlsx::save(&path, &wb).unwrap();

    let mut back = sheetcore::xlsx::load(&path).unwrap();
    formula::recalc_all(&mut back);
    assert_eq!(back.sheets[0].value(0, 0), Value::Number(1.0));
    assert_eq!(back.sheets[0].value(2, 1), Value::Number(6.0));
}

#[test]
fn array_result_feeds_scalar_functions() {
    let mut wb = Workbook::new_blank();
    // SUM 吃数组结果
    match run(&mut wb, "=SUM(SEQUENCE(5,1))") {
        FValue::Number(n) => assert_eq!(n, 15.0),
        other => panic!("SUM(SEQUENCE) {other:?}"),
    }
}
