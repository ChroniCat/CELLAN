//! C07.1 动态列表来源的真实 xlsx 往返：OFFSET / INDIRECT / FILTER 非空 / 名称 /
//! 括号表名 / 相对来源按规则范围锚点平移。夹具由 scripts/c07_dv_dynamic_fixture.py 生成。

use sheetcore::model::Value;

fn fixture() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/c07/dynamic.xlsx")
}

fn items(wb: &sheetcore::model::Workbook, r: u32, c: u32) -> Option<Vec<String>> {
    let sheet = wb.sheet(0);
    let dv = sheetcore::dv::find(&sheet.dv, r, c).expect("规则存在").clone();
    sheetcore::dv::list_items_wb(wb, 0, &dv, r, c)
}

fn check(wb: &sheetcore::model::Workbook) {
    assert_eq!(
        items(wb, 1, 1).as_deref(),
        Some(&["甲".to_string(), "乙".to_string(), "丙".to_string()][..]),
        "OFFSET 动态来源"
    );
    assert_eq!(
        items(wb, 1, 2).as_deref(),
        Some(&["甲".to_string(), "乙".to_string()][..]),
        "INDIRECT 文本区域"
    );
    assert_eq!(
        items(wb, 1, 3).as_deref(),
        Some(&["甲".to_string(), "乙".to_string(), "丙".to_string()][..]),
        "FILTER 非空筛选，空白不成为候选"
    );
    assert_eq!(
        items(wb, 1, 4).as_deref(),
        Some(&["10".to_string(), "20".to_string()][..]),
        "E2 的来源 =$A1:$A2"
    );
    assert_eq!(
        items(wb, 2, 4).as_deref(),
        Some(&["20".to_string(), "30".to_string()][..]),
        "E3 的来源应平移到 A2:A3（Excel：C3 的 Formula1 = $A2:$A3）"
    );
    assert_eq!(
        items(wb, 1, 5).as_deref(),
        Some(&["甲".to_string(), "乙".to_string()][..]),
        "带括号的工作表名不是函数调用"
    );
    assert_eq!(
        items(wb, 1, 6).as_deref(),
        Some(&["甲".to_string(), "乙".to_string(), "丙".to_string()][..]),
        "名称指向 OFFSET 公式"
    );

    let mut ev = |_: &str, _: u32, _: u32| sheetcore::formula::FValue::Bool(true);
    let sheet = wb.sheet(0);
    let rule = |r: u32, c: u32| sheetcore::dv::find(&sheet.dv, r, c).expect("规则").clone();
    // E3 只允许 20/30，10 必须被拒绝
    assert!(sheetcore::dv::validate_wb(wb, 0, &rule(2, 4), &Value::Number(30.0), 2, 4, &mut ev));
    assert!(!sheetcore::dv::validate_wb(wb, 0, &rule(2, 4), &Value::Number(10.0), 2, 4, &mut ev));
    // FILTER 列表：丙在、丁不在
    assert!(sheetcore::dv::validate_wb(wb, 0, &rule(1, 3), &Value::Text("丙".into()), 1, 3, &mut ev));
    assert!(!sheetcore::dv::validate_wb(wb, 0, &rule(1, 3), &Value::Text("丁".into()), 1, 3, &mut ev));
}

#[test]
fn dynamic_sources_work_and_survive_save_reload() {
    let path = fixture();
    let wb = sheetcore::xlsx::load(&path).expect("加载动态列表夹具");
    check(&wb);

    let out = std::env::current_exe()
        .expect("test exe")
        .parent()
        .expect("dir")
        .join(format!("c07_dynamic_{}.xlsx", std::process::id()));
    let _ = std::fs::remove_file(&out);
    sheetcore::xlsx::save(&out, &wb).expect("保存");
    let back = sheetcore::xlsx::load(&out).expect("重开");
    check(&back);
    let _ = std::fs::remove_file(&out);
}
