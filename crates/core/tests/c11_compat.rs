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
fn external_three_d_reference_keeps_formula_and_recalculates_after_open_save_reload() {
    let (mut wb, diag) = xlsx::load_diag(&fixture()).unwrap();
    let cell = wb.sheets[0].get(0, 0).expect("fixture A1");
    assert_eq!(cell.formula.as_deref(), Some("=SUM([1]First:Third!A1)"));
    assert_eq!(cell.value, Value::Number(60.0));

    // 原生已支持外部三维引用，不应再弹出任何兼容告警
    assert!(
        diag.is_complete(),
        "外部工作簿三维引用已原生支持，不应产生诊断告警：{diag:?}"
    );

    // 清空缓存重算：必须依靠外部链接三维求和主动算回 60
    if let Some(c) = wb.sheets[0].get_mut(0, 0) {
        c.value = Value::Number(0.0);
    }
    recalc_all_force(&mut wb);
    let cell = wb.sheets[0].get(0, 0).unwrap();
    assert_eq!(cell.formula.as_deref(), Some("=SUM([1]First:Third!A1)"));
    assert_eq!(cell.value, Value::Number(60.0), "主动重算必须算出 10+20+30=60");

    // 外部链接缓存变化时，重算必须算出新和值（例如 100+200+300=600），且标记已刷新+编辑后存盘重开保持 600
    wb.links[0].cache.insert((0, 0, 0), Value::Number(100.0));
    wb.links[0].cache.insert((1, 0, 0), Value::Number(200.0));
    wb.links[0].cache.insert((2, 0, 0), Value::Number(300.0));
    wb.links[0].refreshed = true;
    wb.sheets[0].edited = true;
    recalc_all_force(&mut wb);
    assert_eq!(wb.sheets[0].value(0, 0), Value::Number(600.0));

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
    assert_eq!(cell2.value, Value::Number(600.0));
    assert!(
        diag2.is_complete(),
        "保存重开后仍无兼容告警：{diag2:?}"
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
fn external_three_d_text_inside_strings_is_not_cache_preserved() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Other".into();
    wb.sheets[0].set(
        0,
        0,
        Cell {
            value: Value::Number(7.0),
            formula: Some("=\"[1]First:Third!A1\"".into()),
        },
    );
    wb.sheets[0].set(
        1,
        0,
        Cell {
            value: Value::Number(7.0),
            formula: Some("=IF(TRUE,1/0,\"[1]First:Third!A1\")".into()),
        },
    );
    recalc_all_force(&mut wb);
    assert_eq!(
        wb.sheets[0].value(0, 0),
        Value::Text("[1]First:Third!A1".into()),
        "字符串里的外部三维文本必须按普通文本求值"
    );
    assert_eq!(
        wb.sheets[0].value(1, 0),
        Value::Error(ErrKind::Div0),
        "字符串参数不能阻止 IF 分支的真实 #DIV/0!"
    );
}

#[test]
fn external_three_d_is_natively_supported_across_syntax_variants() {
    let (wb, _) = xlsx::load_diag(&fixture()).unwrap();
    assert!(sheetcore::formula::unsupported_reference_combo("=SUM([1]First:Third!A1)").is_none());
    assert!(sheetcore::formula::unsupported_reference_combo("=SUM('[b.xlsx]First:Third'!A1)").is_none());
    assert_eq!(sheetcore::formula::eval_formula(&wb, 0, "=SUM([1]First:Third!A1)").display(), "60");
    assert_eq!(sheetcore::formula::eval_formula(&wb, 0, "=SUM('[1]First:Third'!A1)").display(), "60");
    assert_eq!(sheetcore::formula::eval_formula(&wb, 0, "=SUM([1]Third:First!A1)").display(), "60");
    assert_eq!(sheetcore::formula::eval_formula(&wb, 0, "=AVERAGE([1]First:Third!A1)").display(), "20");
    assert_eq!(sheetcore::formula::eval_formula(&wb, 0, "=SHEETS([1]First:Third!A1)").display(), "3");
    assert_eq!(sheetcore::formula::eval_formula(&wb, 0, "=SHEET([1]Second!A1)").display(), "2");
}
