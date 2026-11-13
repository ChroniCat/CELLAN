//! C11：合法但尚未实现的引用组合必须保留缓存并给出可见兼容诊断；
//! 真正的运算错误仍必须按错误值写回，不能被这条路径误保。

use sheetcore::formula::{recalc_all_force, ErrKind};
use sheetcore::model::{Cell, Value, Workbook};
use sheetcore::xlsx;
use std::path::PathBuf;

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/c11_external_3d.xlsx")
}

#[test]
fn external_three_d_reference_keeps_formula_and_cache_after_open_recalc_save_reload() {
    let (mut wb, diag) = xlsx::load_diag(&fixture()).unwrap();
    let cell = wb.sheets[0].get(0, 0).expect("fixture A1");
    assert_eq!(cell.formula.as_deref(), Some("=SUM([1]First:Third!A1)"));
    assert_eq!(cell.value, Value::Number(60.0));

    let compat: Vec<_> = diag
        .problems
        .iter()
        .filter(|p| p.what == "兼容提示")
        .collect();
    assert_eq!(compat.len(), 1, "必须给出一条兼容诊断：{diag:?}");
    assert!(!compat[0].fatal);
    assert!(compat[0].detail.contains("外部工作簿三维引用"));
    assert!(compat[0].detail.contains("缓存"));

    // 打开后的全量重算不能把已知缓存覆盖成 #REF!。
    recalc_all_force(&mut wb);
    let cell = wb.sheets[0].get(0, 0).unwrap();
    assert_eq!(cell.formula.as_deref(), Some("=SUM([1]First:Third!A1)"));
    assert_eq!(cell.value, Value::Number(60.0), "重算必须保留原缓存");

    let dir = std::env::temp_dir().join(format!(
        "sc_c11_external3d_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("saved.xlsx");
    xlsx::save(&out, &wb).unwrap();
    let (wb2, diag2) = xlsx::load_diag(&out).unwrap();
    let cell2 = wb2.sheets[0].get(0, 0).unwrap();
    assert_eq!(cell2.formula.as_deref(), Some("=SUM([1]First:Third!A1)"));
    assert_eq!(cell2.value, Value::Number(60.0));
    assert!(
        diag2
            .problems
            .iter()
            .any(|p| p.what == "兼容提示" && !p.fatal),
        "保存重开后兼容诊断仍应可见：{diag2:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn genuine_ref_and_value_errors_are_not_treated_as_unsupported_cache() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Other".into();
    wb.sheets[0].set(
        0,
        0,
        Cell {
            value: Value::Number(7.0),
            formula: Some("=SUM(NoSuchSheet!A1)".into()),
        },
    );
    wb.sheets[0].set(
        1,
        0,
        Cell {
            value: Value::Number(7.0),
            formula: Some("=1/0".into()),
        },
    );
    wb.sheets[0].set(
        2,
        0,
        Cell {
            value: Value::Number(7.0),
            formula: Some("=NOSUCHFUNC(1)".into()),
        },
    );
    // 让真实运算错误不受任何兼容缓存分支影响。
    recalc_all_force(&mut wb);
    assert_eq!(wb.sheets[0].value(0, 0), Value::Error(ErrKind::Ref));
    assert_eq!(wb.sheets[0].value(1, 0), Value::Error(ErrKind::Div0));
    assert_eq!(wb.sheets[0].value(2, 0), Value::Error(ErrKind::Name));
}

#[test]
fn compatibility_detector_is_limited_to_external_three_d() {
    assert!(sheetcore::formula::unsupported_reference_combo("=SUM([1]First:Third!A1)").is_some());
    assert!(sheetcore::formula::unsupported_reference_combo("=SUM('[b.xlsx]First:Third'!A1)").is_some());
    assert!(sheetcore::formula::unsupported_reference_combo("=SUM([1]First!A1)").is_none());
    assert!(sheetcore::formula::unsupported_reference_combo("=SUM(First:Third!A1)").is_none());
    assert!(sheetcore::formula::unsupported_reference_combo("=FOO(1)").is_none());
}
