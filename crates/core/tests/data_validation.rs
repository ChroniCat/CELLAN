//! 数据验证：读、校验、改、存
use sheetcore::dv::{self, DvKind, DvOp};
use sheetcore::model::Value;
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/qa02_cf_dv")
        .join(name)
}

/// 保存输出写到 test executable 旁边，不再写固定输出目录
fn out_path(tag: &str) -> PathBuf {
    std::env::current_exe()
        .expect("test exe")
        .parent()
        .expect("test dir")
        .join(format!("{}_{}.xlsx", tag, std::process::id()))
}

fn load() -> Option<sheetcore::model::Workbook> {
    let p = fixture("dv.xlsx");
    assert!(p.exists(), "required test fixture is missing: {}", p.display());
    Some(sheetcore::xlsx::load(&p).expect("读入"))
}

#[test]
fn parses_from_file() {
    let Some(wb) = load() else { return };
    let sh = wb.sheet(0);
    assert!(sh.dv.len() >= 2, "应解析出至少两条验证: {}", sh.dv.len());
    let list = dv::find(&sh.dv, 1, 2).expect("C2 应有序列验证");
    assert_eq!(list.kind, DvKind::List);
    assert_eq!(dv::list_items(list, sh), vec!["A", "B", "C", "D"]);
    let whole = dv::find(&sh.dv, 1, 1).expect("B2 应有整数验证");
    assert_eq!(whole.kind, DvKind::Whole);
    assert_eq!(whole.op, DvOp::Between);
    assert_eq!(whole.error_title, "人数超范围");
}

#[test]
fn validates_values() {
    let Some(wb) = load() else { return };
    let sh = wb.sheet(0);
    let whole = dv::find(&sh.dv, 1, 1).unwrap();
    let ok = |v: f64| dv::validate(whole, &Value::Number(v), sh, 1, 1, |_, _, _| sheetcore::formula::FValue::Bool(true));
    assert!(ok(50.0));
    assert!(!ok(0.0), "0 小于下限");
    assert!(!ok(101.0), "101 超过上限");
    assert!(!ok(3.5), "整数验证不接受小数");

    let list = dv::find(&sh.dv, 1, 2).unwrap();
    let ok_list = |s: &str| dv::validate(list, &Value::Text(s.into()), sh, 1, 2, |_, _, _| sheetcore::formula::FValue::Bool(true));
    assert!(ok_list("B"));
    assert!(!ok_list("Z"));
}

#[test]
fn add_and_save_roundtrip() {
    let Some(mut wb) = load() else { return };
    let out = out_path("qa02_dv_edit");
    let _ = std::fs::remove_file(&out);

    // 给 A2:A20 加一条文本长度验证
    wb.sheets[0].dv.push(dv::DataValidation {
        ranges: vec![(1, 0, 19, 0)],
        kind: DvKind::TextLength,
        op: DvOp::LessEq,
        f1: "10".into(),
        ..Default::default()
    });
    wb.sheets[0].dv_dirty = true;
    sheetcore::xlsx::save(&out, &wb).expect("保存");

    let back = sheetcore::xlsx::load(&out).expect("重新读入");
    let sh = back.sheet(0);
    let added = sh
        .dv
        .iter()
        .find(|d| d.kind == DvKind::TextLength)
        .expect("新增的文本长度验证应存在");
    assert_eq!(added.f1, "10");
    assert!(dv::find(&sh.dv, 1, 2).is_some(), "原有序列验证不能丢");
    assert!(dv::find(&sh.dv, 1, 1).is_some(), "原有整数验证不能丢");
}
