//! C02 数组形状与二维广播。
//!
//! 需求表格三条：`=SEQUENCE(2,2)+1` 应是 2×2、`=SEQUENCE(3,1)*2` 应是 3×1、
//! `=SUM({1;2}+{10,20})` 应是 66。旧实现把操作数摊平、结果固定成 1×n，全部退化成横向。

use sheetcore::formula::{self, ErrKind};
use sheetcore::formula::value::FValue;
use sheetcore::{Cell, Value, Workbook};

fn eval(wb: &Workbook, f: &str) -> FValue {
    formula::eval_formula(wb, 0, f)
}

fn grid(v: &FValue) -> (usize, usize, String) {
    match v {
        FValue::Array(a) => {
            let cell = |x: &FValue| match x {
                FValue::Number(n) => {
                    if n.fract() == 0.0 { format!("{}", *n as i64) } else { format!("{n}") }
                }
                FValue::Bool(b) => format!("{b}"),
                FValue::Text(t) => t.clone(),
                FValue::Empty => "".into(),
                other => format!("{other:?}"),
            };
            let mut rows = Vec::new();
            for r in 0..a.rows {
                let line: Vec<String> = (0..a.cols).map(|c| cell(&a.get(r, c))).collect();
                rows.push(line.join(","));
            }
            (a.rows, a.cols, rows.join("/"))
        }
        other => panic!("不是数组：{other:?}"),
    }
}

#[test]
fn a_2x2_array_plus_scalar_keeps_its_shape() {
    let wb = Workbook::new_blank();
    assert_eq!(grid(&eval(&wb, "=SEQUENCE(2,2)+1")), (2, 2, "2,3/4,5".to_string()));
}

#[test]
fn a_column_array_times_scalar_stays_a_column() {
    let wb = Workbook::new_blank();
    assert_eq!(grid(&eval(&wb, "=SEQUENCE(3,1)*2")), (3, 1, "2/4/6".to_string()));
}

#[test]
fn a_column_plus_row_broadcasts_to_the_full_rectangle() {
    let wb = Workbook::new_blank();
    // {1;2} + {10,20} = [[11,21],[12,22]]，SUM=66
    let v = eval(&wb, "={1;2}+{10,20}");
    assert_eq!(grid(&v), (2, 2, "11,21/12,22".to_string()));
    assert_eq!(eval(&wb, "=SUM({1;2}+{10,20})"), FValue::Number(66.0));
}

#[test]
fn matching_shapes_multiply_elementwise() {
    let wb = Workbook::new_blank();
    assert_eq!(
        grid(&eval(&wb, "=SEQUENCE(2,2)+SEQUENCE(2,2)")),
        (2, 2, "2,4/6,8".to_string())
    );
    assert_eq!(grid(&eval(&wb, "=1+SEQUENCE(3,1)")), (3, 1, "2/3/4".to_string()));
    assert_eq!(
        grid(&eval(&wb, "=SEQUENCE(3,1)+SEQUENCE(1,4)")),
        (3, 4, "2,3,4,5/3,4,5,6/4,5,6,7".to_string())
    );
}

#[test]
fn incompatible_shapes_are_a_value_error() {
    let wb = Workbook::new_blank();
    assert_eq!(eval(&wb, "=SEQUENCE(2,2)+SEQUENCE(2,3)"), FValue::Error(ErrKind::Value));
    assert_eq!(eval(&wb, "=SEQUENCE(2,2)+SEQUENCE(3,2)"), FValue::Error(ErrKind::Value));
}

#[test]
fn comparison_and_concat_keep_the_operand_shape() {
    let wb = Workbook::new_blank();
    assert_eq!(
        grid(&eval(&wb, "=SEQUENCE(3,1)>1")),
        (3, 1, "false/true/true".to_string())
    );
    assert_eq!(
        grid(&eval(&wb, "=SEQUENCE(2,1)&\"x\"")),
        (2, 1, "1x/2x".to_string())
    );
}

#[test]
fn unary_minus_applies_to_every_element() {
    let wb = Workbook::new_blank();
    assert_eq!(grid(&eval(&wb, "=-SEQUENCE(2,2)")), (2, 2, "-1,-2/-3,-4".to_string()));
    assert_eq!(grid(&eval(&wb, "=SEQUENCE(2,1)%")), (2, 1, "0.01/0.02".to_string()));
}

#[test]
fn sumproduct_requires_matching_shapes() {
    let wb = Workbook::new_blank();
    assert_eq!(
        eval(&wb, "=SUMPRODUCT({1,2;3,4},{1,1;1,1})"),
        FValue::Number(10.0)
    );
    assert_eq!(
        eval(&wb, "=SUMPRODUCT({1,2;3,4},{1,1,1,1})"),
        FValue::Error(ErrKind::Value),
        "2×2 与 1×4 元素个数相同但形状不同，Excel 报 #VALUE!"
    );
}

#[test]
fn filter_uses_the_condition_shape_not_its_length() {
    let mut wb = Workbook::new_blank();
    for (i, v) in [1.0, 2.0, 3.0, 4.0, 5.0].iter().enumerate() {
        wb.sheets[0].set(i as u32, 0, Cell { value: Value::Number(*v), formula: None });
        wb.sheets[0].set(0, i as u32, Cell { value: Value::Number(*v), formula: None });
    }
    assert_eq!(
        grid(&eval(&wb, "=FILTER(A1:A5,A1:A5>2)")),
        (3, 1, "3/4/5".to_string()),
        "列条件的 FILTER 结果必须还是列"
    );
    assert_eq!(
        grid(&eval(&wb, "=FILTER(A1:E1,A1:E1>2)")),
        (1, 3, "3,4,5".to_string()),
        "行条件的结果必须还是行"
    );
}

// ---------------------------------------------------- 锚点 / 阻挡 / 下游 / 存盘

fn spill_book() -> Workbook {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(0, 0, Cell { value: Value::Empty, formula: Some("=SEQUENCE(2,2)+1".into()) });
    wb.sheets[0].set(0, 3, Cell { value: Value::Empty, formula: Some("=SUM(A1:B2)".into()) });
    formula::recalc_all(&mut wb);
    wb
}

#[test]
fn spill_anchor_shape_downstream_and_blocking() {
    let mut wb = spill_book();
    let s = &wb.sheets[0];
    assert_eq!(s.value(0, 0), Value::Number(2.0), "A1 锚点");
    assert_eq!(s.value(0, 1), Value::Number(3.0), "B1 同行第二列（旧实现会写成 A2）");
    assert_eq!(s.value(1, 0), Value::Number(4.0), "A2 第二行");
    assert_eq!(s.value(1, 1), Value::Number(5.0), "B2");
    assert_eq!(s.value(0, 3), Value::Number(14.0), "下游 SUM(A1:B2)=2+3+4+5");

    // 阻挡：占住 B2 → #SPILL!
    wb.sheets[0].set(1, 1, Cell { value: Value::Text("挡".into()), formula: None });
    formula::recalc_all(&mut wb);
    assert_eq!(wb.sheets[0].value(0, 0), Value::Error(ErrKind::Spill));

    // 移开阻挡 → 形状回来
    wb.sheets[0].set(1, 1, Cell::default());
    formula::recalc_all(&mut wb);
    assert_eq!(wb.sheets[0].value(0, 1), Value::Number(3.0));
    assert_eq!(wb.sheets[0].value(1, 1), Value::Number(5.0));
}

#[test]
fn the_spilled_shape_survives_a_save_and_reopen() {
    let mut wb = spill_book();
    wb.sheets[0].set(3, 3, Cell { value: Value::Number(1.0), formula: None }); // 让文件非空
    let dir = std::env::temp_dir().join(format!("c02-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("shape.xlsx");
    sheetcore::xlsx::save_atomic(&path, &wb).expect("保存失败");

    let mut back = sheetcore::xlsx::load(&path).expect("打开失败");
    // 文件里只存锚点公式（溢出值不落盘），打开后按 Excel 语义重算
    assert_eq!(
        back.sheets[0].get(0, 0).unwrap().formula.as_deref(),
        Some("=SEQUENCE(2,2)+1")
    );
    assert_eq!(back.sheets[0].value(0, 1), Value::Empty, "溢出子格不应写死在文件里");
    formula::recalc_all(&mut back);
    let s = &back.sheets[0];
    assert_eq!(s.value(0, 0), Value::Number(2.0), "重开后 A1");
    assert_eq!(s.value(0, 1), Value::Number(3.0), "重开后 B1");
    assert_eq!(s.value(1, 0), Value::Number(4.0), "重开后 A2");
    assert_eq!(s.value(1, 1), Value::Number(5.0), "重开后 B2");
    assert_eq!(s.get(0, 0).unwrap().formula.as_deref(), Some("=SEQUENCE(2,2)+1"));
}
