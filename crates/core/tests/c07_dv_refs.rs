//! C07.1：真实 xlsx 包中的命名列表与公式阈值。
use sheetcore::model::Value;

#[test]
fn named_list_and_formula_threshold_survive_load() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/c07/refs.xlsx");
    let wb = sheetcore::xlsx::load(&path).expect("加载 C07 夹具");
    let sheet = wb.sheet(0);
    let dv_list = sheetcore::dv::find(&sheet.dv, 1, 1).expect("B2 应有列表验证");
    assert_eq!(dv_list.kind, sheetcore::dv::DvKind::List);
    let mut calc = sheetcore::formula::Calc::new(&wb);
    let mut ev = |f: &str, r: u32, c: u32| {
        let shifted = sheetcore::cf::shift_formula(f.trim_start_matches('='), r, c);
        sheetcore::formula::parse(&shifted)
            .map(|node| calc.eval_node(&node, 0))
            .unwrap_or(sheetcore::formula::FValue::Bool(true))
    };
    assert!(
        sheetcore::dv::validate_wb(&wb, 0, dv_list, &Value::Text("甲".into()), 1, 1, &mut ev),
        "命名列表成员应通过"
    );
    assert!(
        !sheetcore::dv::validate_wb(&wb, 0, dv_list, &Value::Text("丙".into()), 1, 1, &mut ev),
        "命名列表外成员应拒绝"
    );

    let dv_num = sheetcore::dv::find(&sheet.dv, 1, 2).expect("C2 应有整数验证");
    assert_eq!(dv_num.kind, sheetcore::dv::DvKind::Whole);
    let mut calc2 = sheetcore::formula::Calc::new(&wb);
    let mut ev2 = |f: &str, r: u32, c: u32| {
        let shifted = sheetcore::cf::shift_formula(f.trim_start_matches('='), r, c);
        sheetcore::formula::parse(&shifted)
            .map(|node| calc2.eval_node(&node, 0))
            .unwrap_or(sheetcore::formula::FValue::Bool(true))
    };
    assert!(
        !sheetcore::dv::validate_wb(&wb, 0, dv_num, &Value::Number(0.0), 1, 2, &mut ev2),
        "阈值 =$A$1 且 A1=1 时 0 应拒绝"
    );
    assert!(
        sheetcore::dv::validate_wb(&wb, 0, dv_num, &Value::Number(2.0), 1, 2, &mut ev2),
        "阈值 =$A$1 且 A1=1 时 2 应通过"
    );
}
