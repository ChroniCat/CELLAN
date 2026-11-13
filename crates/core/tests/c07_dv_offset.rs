//! C07：OFFSET 列表来源的真实文件往返。
//! Excel 16.0 语义（COM 实测）：
//! - 省略 height/width 时继承 reference 的行列数；
//! - 跨表 reference 不改变 COUNTA($B:$B) 这类未限定参数所在的表；
//! - 负数 height 向反方向展开。

use sheetcore::dv::{list_items_wb, DataValidation, DvKind};
use std::path::{Path, PathBuf};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/c07/offset.xlsx")
}

fn dv(f1: &str) -> DataValidation {
    DataValidation { kind: DvKind::List, f1: f1.trim_start_matches('=').into(), ..Default::default() }
}

#[test]
fn offset_lists_keep_excel_semantics_after_a_roundtrip() {
    let mut wb = sheetcore::xlsx::load(&fixture()).expect("加载 OFFSET 夹具");
    assert_eq!(wb.sheets.len(), 2);

    let three = vec!["甲".to_string(), "乙".to_string(), "丙".to_string()];
    let got = list_items_wb(&wb, 0, &dv("OFFSET($A$1:$A$3,0,0)"), 0, 2).expect("解析 C1");
    assert_eq!(got, three, "省略 height 必须继承 A1:A3 的 3 行");

    let two = vec!["100".to_string(), "200".to_string()];
    let got = list_items_wb(&wb, 0, &dv("OFFSET(数据!$A$1,0,0,COUNTA($B:$B),1)"), 0, 3)
        .expect("解析 D1");
    assert_eq!(got, two, "COUNTA 在主表（公式所在表）求值，高度 2 而不是数据表的 5");

    let up = vec!["乙".to_string(), "丙".to_string()];
    let got = list_items_wb(&wb, 0, &dv("OFFSET($A$3,0,0,-2,1)"), 0, 4).expect("解析 E1");
    assert_eq!(got, up, "height=-2 从 A3 向上");

    // 保存后再读：公式与语义都不能变
    let tmp = std::env::temp_dir().join("sc_c07_offset_rt.xlsx");
    let _ = std::fs::remove_file(&tmp);
    sheetcore::xlsx::save(&tmp, &wb).expect("保存");
    let again = sheetcore::xlsx::load(&tmp).expect("重开");
    assert_eq!(
        list_items_wb(&again, 0, &dv("OFFSET($A$1:$A$3,0,0)"), 0, 2).unwrap(),
        three
    );
    assert_eq!(
        list_items_wb(&again, 0, &dv("OFFSET(数据!$A$1,0,0,COUNTA($B:$B),1)"), 0, 3).unwrap(),
        two
    );
    assert_eq!(
        list_items_wb(&again, 0, &dv("OFFSET($A$3,0,0,-2,1)"), 0, 4).unwrap(),
        up
    );
}

#[test]
fn offset_source_as_a_whole_column_reference_stays_a_column() {
    let wb = sheetcore::xlsx::load(&fixture()).expect("加载 OFFSET 夹具");
    // A:A 是整列：省略 height 时继承 1048576 行（稀疏取值只回非空）
    let got = list_items_wb(&wb, 0, &dv("OFFSET($A:$A,0,0)"), 0, 2).expect("解析整列");
    assert_eq!(got.first().map(|s| s.as_str()), Some("甲"));
    assert!(got.len() <= 4, "只应取到列里的非空值：{got:?}");
}
