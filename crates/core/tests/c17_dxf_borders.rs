//! C17：差异格式（dxf）的四边边框必须逐边保真；未建模的对角/内部边框要显式提示。

use sheetcore::cf::CfKind;
use std::path::{Path, PathBuf};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/c17/borders.xlsx")
}

#[test]
fn dxf_four_sides_keep_their_own_style_and_color() {
    let wb = sheetcore::xlsx::load(&fixture()).expect("加载边框夹具");
    let cf = &wb.sheet(0).cf;
    assert_eq!(cf.dxfs.len(), 2, "两条规则各自的 dxf");
    let b = cf.dxfs[0].border.as_ref().expect("第一条规则有边框");
    assert_eq!(b.sides[0].as_ref().unwrap().0, "thick");
    assert_eq!(b.sides[0].as_ref().unwrap().1, [255, 0, 0]);
    assert_eq!(b.sides[1].as_ref().unwrap().0, "thin");
    assert_eq!(b.sides[1].as_ref().unwrap().1, [0, 0, 255]);
    assert_eq!(b.sides[2].as_ref().unwrap().0, "medium");
    assert_eq!(b.sides[2].as_ref().unwrap().1, [0, 255, 0]);
    assert_eq!(b.sides[3].as_ref().unwrap().0, "double");
    assert_eq!(b.sides[3].as_ref().unwrap().1, [255, 255, 0]);
    let diag = cf.dxfs[1].border.as_ref().map(|b| b.sides.clone());
    assert!(
        diag.is_none() || diag.as_ref().unwrap().iter().all(|s| s.is_none()),
        "对角边框不是四边之一，不能被塞进任何一边：{diag:?}"
    );
    assert!(cf.dxf_unsupported, "对角边框必须标成未建模，界面要提示");
}

#[test]
fn dxf_four_sides_survive_a_save_roundtrip() {
    let mut wb = sheetcore::xlsx::load(&fixture()).expect("加载边框夹具");
    let tmp = std::env::temp_dir().join("sc_c17_dxf_sides.xlsx");
    let _ = std::fs::remove_file(&tmp);
    sheetcore::xlsx::save(&tmp, &wb).expect("保存");
    let again = sheetcore::xlsx::load(&tmp).expect("重开");
    let b = again.sheet(0).cf.dxfs[0].border.as_ref().expect("边框仍在");
    let got: Vec<(&str, [u8; 3])> = b
        .sides
        .iter()
        .map(|s| {
            let (st, c) = s.as_ref().expect("四边都要有");
            (st.as_str(), *c)
        })
        .collect();
    assert_eq!(
        got,
        vec![
            ("thick", [255, 0, 0]),
            ("thin", [0, 0, 255]),
            ("medium", [0, 255, 0]),
            ("double", [255, 255, 0]),
        ]
    );
    // 规则仍然引用 dxfId 0/1，色阶/表达式求值不受影响
    let rules: Vec<&CfKind> = again
        .sheet(0)
        .cf
        .blocks
        .iter()
        .flat_map(|bl| bl.rules.iter().map(|r| &r.kind))
        .collect();
    assert_eq!(rules.len(), 2);
    let _ = wb;
}

#[test]
fn dxf_xml_writes_each_side_separately() {
    let wb = sheetcore::xlsx::load(&fixture()).expect("加载边框夹具");
    let d = &wb.sheet(0).cf.dxfs[0];
    let xml = sheetcore::patch::dxf_xml(d);
    assert!(xml.contains("<left style=\"thick\"><color rgb=\"FFFF0000\"/>"), "{xml}");
    assert!(xml.contains("<right style=\"thin\"><color rgb=\"FF0000FF\"/>"), "{xml}");
    assert!(xml.contains("<top style=\"medium\"><color rgb=\"FF00FF00\"/>"), "{xml}");
    assert!(xml.contains("<bottom style=\"double\"><color rgb=\"FFFFFF00\"/>"), "{xml}");
}
