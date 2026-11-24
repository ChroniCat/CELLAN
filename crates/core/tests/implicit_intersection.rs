//! C11.3：隐式交集运算符 `@`。
//!
//! Excel 365 的 `@` 把多格引用收成一个与公式位置相交的标量：
//! 单列区域按公式行取同列，单行区域按公式列取同列；没有唯一交点时
//! 返回 #VALUE!。单格引用 `@A1` 在任何位置都返回 A1。

use sheetcore::formula::{deps, recalc_all_force, FValue};
use sheetcore::model::{Cell, Sheet, Value, Workbook};

fn formula(text: &str) -> Cell {
    Cell { value: Value::Empty, formula: Some(text.into()) }
}

fn value(wb: &Workbook, sheet: usize, r: u32, c: u32) -> Value {
    wb.sheets[sheet].value(r, c)
}

#[test]
fn implicit_intersection_uses_formula_cell_position() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Data".into();
    let sh = &mut wb.sheets[0];
    for (r, n) in [10.0, 20.0, 30.0].iter().enumerate() {
        sh.set(r as u32, 0, Cell { value: Value::Number(*n), formula: None });
    }
    // 单行区域 A1:C1 = 10/20/30；B2/B4 的 @A1:C1 应取 B1。
    sh.set(0, 1, Cell { value: Value::Number(20.0), formula: None });
    sh.set(0, 2, Cell { value: Value::Number(30.0), formula: None });
    // C2：单格引用在任何位置都返回 A1。
    sh.set(1, 2, formula("=@A1"));
    // D2：单列区域按公式行取 A2；D3 取 A3；D4 越界。
    sh.set(1, 3, formula("=@A1:A3"));
    sh.set(2, 3, formula("=@A1:A3"));
    sh.set(3, 3, formula("=@A1:A3"));
    // B2/B4：单行区域按公式列取 B1（不要求与区域同一行）。
    sh.set(1, 1, formula("=@A1:C1"));
    sh.set(3, 1, formula("=@A1:C1"));
    // E2/E4：公式列已越出 A:C，仍然没有交点。
    sh.set(1, 4, formula("=@A1:C1"));
    sh.set(3, 4, formula("=@A1:C1"));
    // F2：二维区域的列不相交。
    sh.set(1, 5, formula("=@A1:C3"));

    // 复制/插行时 @ 只是外层运算符，内部引用仍按普通 A1 规则平移。
    let mut shift = sheetcore::refs::Shift { axis: sheetcore::refs::Axis::Row, at: 0, n: 1, ..Default::default() };
    shift.target_sheet = "Data".into();
    shift.own_sheet = "Data".into();
    assert_eq!(sheetcore::refs::rewrite("=@A1:A3", &shift), "=@A2:A4");

    recalc_all_force(&mut wb);
    assert_eq!(value(&wb, 0, 1, 2), Value::Number(10.0), "@A1 在 C2");
    assert_eq!(value(&wb, 0, 1, 3), Value::Number(20.0), "@A1:A3 在 D2");
    assert_eq!(value(&wb, 0, 2, 3), Value::Number(30.0), "@A1:A3 在 D3");
    assert_eq!(value(&wb, 0, 1, 1), Value::Number(20.0), "@A1:C1 在 B2 取 B1");
    assert_eq!(value(&wb, 0, 3, 1), Value::Number(20.0), "@A1:C1 在 B4 取 B1");
    for (r, c) in [(3, 3), (1, 4), (3, 4), (1, 5)] {
        assert_eq!(
            value(&wb, 0, r, c),
            Value::Error(sheetcore::formula::ErrKind::Value),
            "@ 无唯一交点必须 #VALUE! ({r},{c})"
        );
    }

    // 跨表公式同样使用公式单元格的行/列，而不是目标表的 A1。
    let mut other = Sheet::new("Other");
    other.set(1, 1, formula("=@Data!A1:A3"));
    other.set(3, 1, formula("=@Data!A1:A3"));
    wb.sheets.push(other);
    recalc_all_force(&mut wb);
    assert_eq!(value(&wb, 1, 1, 1), Value::Number(20.0), "Other!B2 按行 2 取 A2");
    assert_eq!(
        value(&wb, 1, 3, 1),
        Value::Error(sheetcore::formula::ErrKind::Value),
        "Other!B4 超出 A1:A3"
    );
}

#[test]
fn implicit_intersection_is_a_dependency_and_survives_save_reload() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Data".into();
    wb.sheets[0].set(0, 0, Cell { value: Value::Number(10.0), formula: None });
    wb.sheets[0].set(1, 0, Cell { value: Value::Number(20.0), formula: None });
    wb.sheets[0].set(1, 3, formula("=@A1:A2"));

    let deps = deps::Deps::build(&wb);
    assert!(
        deps.refs.get(&(0, 1, 3)).is_some_and(|r| !r.is_empty()),
        "@A1:A2 必须进入依赖图"
    );
    let affected = deps.closure(&[(0, 1, 0)]);
    assert!(affected.contains(&(0, 1, 3)), "改动 A2 必须重算 @A1:A2");

    recalc_all_force(&mut wb);
    assert_eq!(value(&wb, 0, 1, 3), Value::Number(20.0));

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("sheetcore_c113_{}_{nanos}.xlsx", std::process::id()));
    sheetcore::xlsx::save(&path, &wb).expect("保存 @ 公式失败");
    let back = sheetcore::xlsx::load(&path).expect("重新加载 @ 公式失败");
    assert_eq!(
        back.sheets[0].formula_of(1, 3),
        Some("=@A1:A2"),
        "保存重开必须保留 @ 语法"
    );
    let _ = std::fs::remove_file(path);
}

#[test]
fn implicit_intersection_array_operand_takes_top_left() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(0, 0, formula("=SEQUENCE(3,1)"));
    wb.sheets[0].set(0, 1, formula("=@A1#"));
    recalc_all_force(&mut wb);
    assert_eq!(value(&wb, 0, 0, 1), Value::Number(1.0));
}
