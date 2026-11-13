//! C18.2：图案填充与对角线边框保真
//!
//! 夹具由 Excel 16.0 生成：A1 = gray25(lightGray) 图案、fgColor indexed=8；
//! B1 = 对角线向下，thin，红色；C1 = 0.00% 数字格式。

use std::io::Read;
use std::path::{Path, PathBuf};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/c18/pattern.xlsx")
}

fn styles_xml(path: &Path) -> String {
    let file = std::fs::File::open(path).unwrap();
    let mut zip = zip::ZipArchive::new(file).unwrap();
    let mut s = String::new();
    zip.by_name("xl/styles.xml").unwrap().read_to_string(&mut s).unwrap();
    s
}

#[test]
fn pattern_and_diagonal_are_loaded() {
    let wb = sheetcore::xlsx::load(&fixture()).expect("加载夹具");
    let sh = &wb.sheets[0];
    let a1 = sh.style_of(0, 0);
    let p = wb.styles.pattern_of(a1).expect("A1 有图案填充");
    assert_eq!(p.kind, "lightGray");
    assert_eq!(p.fg_indexed, Some(8), "前景索引色");
    let b1 = sh.style_of(0, 1);
    let d = wb.styles.diag_of(b1).expect("B1 有对角线");
    assert_eq!(d.style, "thin");
    assert_eq!(d.color, [0xFF, 0x00, 0x00]);
    assert!(d.down, "对角线向下");
    assert!(!d.up);
    // C1 是普通数字格式，不该带图案/对角线
    assert!(wb.styles.pattern_of(sh.style_of(0, 2)).is_none());
}

#[test]
fn pattern_and_diagonal_survive_editing_number_format() {
    let mut wb = sheetcore::xlsx::load(&fixture()).expect("加载夹具");
    // 只改 C1 的数字格式，A1/B1 的图案与对角线不能动
    let c1 = wb.sheets[0].style_of(0, 2);
    let mut st = wb.styles.get(c1).clone();
    st.format = "0.000".to_string();
    let new_id = wb.styles.intern(st);
    wb.sheets[0].set_style(0, 2, new_id);

    let tmp = std::env::temp_dir().join("sc_c18_pattern_rt.xlsx");
    let _ = std::fs::remove_file(&tmp);
    sheetcore::xlsx::save(&tmp, &wb).expect("保存");
    let xml = styles_xml(&tmp);
    assert!(xml.contains("patternType=\"lightGray\""), "图案类型丢了：{xml}");
    assert!(xml.contains("indexed=\"8\""), "图案前景索引色丢了");
    assert!(xml.contains("diagonalDown=\"1\""), "对角线方向丢了");
    assert!(xml.contains("diagonal style=\"thin\""), "对角线线型丢了");
    assert!(xml.contains("FFFF0000"), "对角线颜色丢了");

    // 连续两次保存仍合法
    let wb2 = sheetcore::xlsx::load(&tmp).expect("第一次保存后重开");
    let tmp2 = std::env::temp_dir().join("sc_c18_pattern_rt2.xlsx");
    let _ = std::fs::remove_file(&tmp2);
    sheetcore::xlsx::save(&tmp2, &wb2).expect("第二次保存");
    let xml2 = styles_xml(&tmp2);
    assert!(xml2.contains("patternType=\"lightGray\"") && xml2.contains("diagonalDown=\"1\""));

    let again = sheetcore::xlsx::load(&tmp2).expect("第二次保存后重开");
    let p = again.styles.pattern_of(again.sheets[0].style_of(0, 0)).expect("图案仍在");
    assert_eq!(p.kind, "lightGray");
    assert_eq!(p.fg_indexed, Some(8));
    let d = again.styles.diag_of(again.sheets[0].style_of(0, 1)).expect("对角线仍在");
    assert_eq!(d.style, "thin");
    assert_eq!(d.color, [0xFF, 0x00, 0x00]);
}
