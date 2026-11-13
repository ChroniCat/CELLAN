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
        "阈值 =$A$1 且 A1=10 时 0 应拒绝"
    );
    assert!(
        sheetcore::dv::validate_wb(&wb, 0, dv_num, &Value::Number(11.0), 1, 2, &mut ev2),
        "阈值 =$A$1 且 A1=10 时 11 应通过"
    );
}

#[test]
fn workbook_sources_and_relative_anchor_are_limited_to_real_file() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/c07/refs.xlsx");
    let wb = sheetcore::xlsx::load(&path).expect("加载 C07 夹具");
    let sheet = wb.sheet(0);
    let find = |r: u32, c: u32| sheetcore::dv::find(&sheet.dv, r, c).cloned().expect("规则");

    let mut calc = sheetcore::formula::Calc::new(&wb);
    let mut ev = |f: &str, r: u32, c: u32| {
        let shifted = sheetcore::cf::shift_formula(f.trim_start_matches('='), r, c);
        sheetcore::formula::parse(&shifted)
            .map(|node| calc.eval_node(&node, 0))
            .unwrap_or(sheetcore::formula::FValue::Bool(true))
    };

    // 跨表区域，真实 xlsx 路径
    let cross = find(1, 3);
    assert!(sheetcore::dv::validate_wb(&wb, 0, &cross, &Value::Text("乙".into()), 1, 3, &mut ev));
    assert!(!sheetcore::dv::validate_wb(&wb, 0, &cross, &Value::Text("丙".into()), 1, 3, &mut ev));

    // 不存在的名称：配置失败，不能退化成“全部允许”
    let bad_name = find(1, 4);
    assert!(sheetcore::dv::list_items_wb(&wb, 0, &bad_name, 1, 4).is_none());
    assert!(!sheetcore::dv::validate_wb(&wb, 0, &bad_name, &Value::Text("甲".into()), 1, 4, &mut ev));

    // 动态来源 FILTER(区域, 区域<>"")：真解析成候选，不能当成"全部允许"或一律失败
    let dynamic = find(1, 6);
    assert_eq!(
        sheetcore::dv::list_items_wb(&wb, 0, &dynamic, 1, 6).as_deref(),
        Some(&["甲".to_string(), "乙".to_string()][..]),
        "FILTER 非空筛选应解析出列表候选"
    );
    assert!(sheetcore::dv::validate_wb(&wb, 0, &dynamic, &Value::Text("甲".into()), 1, 6, &mut ev));
    assert!(!sheetcore::dv::validate_wb(&wb, 0, &dynamic, &Value::Text("丙".into()), 1, 6, &mut ev));

    // O'Brien 表名的双引号转义
    let ob = find(1, 7);
    assert!(sheetcore::dv::validate_wb(&wb, 0, &ob, &Value::Text("丙".into()), 1, 7, &mut ev));
    assert!(!sheetcore::dv::validate_wb(&wb, 0, &ob, &Value::Text("甲".into()), 1, 7, &mut ev));

    // 同名全局/本表级：本表级优先，I2 应为乙而不是甲
    let local = find(1, 8);
    assert!(sheetcore::dv::validate_wb(&wb, 0, &local, &Value::Text("乙".into()), 1, 8, &mut ev));
    assert!(!sheetcore::dv::validate_wb(&wb, 0, &local, &Value::Text("甲".into()), 1, 8, &mut ev));

    // 混合引用 $A1，范围 F2:F3；F2 对 A1=10，F3 对 A2=20
    let mixed = find(1, 5);
    let mut calc2 = sheetcore::formula::Calc::new(&wb);
    let mut ev2 = |f: &str, r: u32, c: u32| {
        let (ar, ac) = sheetcore::dv::range_anchor(&mixed, r, c);
        let shifted = sheetcore::cf::shift_formula(
            f.trim_start_matches('='),
            r.saturating_sub(ar),
            c.saturating_sub(ac),
        );
        sheetcore::formula::parse(&shifted)
            .map(|node| calc2.eval_node(&node, 0))
            .unwrap_or(sheetcore::formula::FValue::Bool(true))
    };
    assert!(sheetcore::dv::validate_wb(&wb, 0, &mixed, &Value::Number(15.0), 1, 5, &mut ev2));
    assert!(sheetcore::dv::validate_wb(&wb, 0, &mixed, &Value::Number(25.0), 2, 5, &mut ev2));
    assert!(!sheetcore::dv::validate_wb(&wb, 0, &mixed, &Value::Number(5.0), 1, 5, &mut ev2));
}
