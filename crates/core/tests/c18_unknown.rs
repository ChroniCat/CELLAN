//! C18.3：未建模的 xf 属性（quotePrefix 等）在保存后不能丢。
//! 夹具 `unknown.xlsx`：A1 用前导单引号输入「编号001」，Excel 在它的 xf 上写
//! `quotePrefix="1"`；B1 数字 12.5，格式 0.0。

use std::io::Read;
use std::path::{Path, PathBuf};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/c18/unknown.xlsx")
}

fn styles_xml(path: &Path) -> String {
    let file = std::fs::File::open(path).unwrap();
    let mut zip = zip::ZipArchive::new(file).unwrap();
    let mut s = String::new();
    zip.by_name("xl/styles.xml").unwrap().read_to_string(&mut s).unwrap();
    s
}

fn roundtrip(tag: &str, edit: bool) -> PathBuf {
    let mut wb = sheetcore::xlsx::load(&fixture()).expect("加载夹具");
    if edit {
        // 只改 B1 的数字格式，A1 的 quotePrefix 不能受影响
        let b1 = wb.sheets[0].style_of(0, 1);
        let mut st = wb.styles.get(b1).clone();
        st.format = "0.000".to_string();
        let id = wb.styles.intern(st);
        wb.sheets[0].set_style(0, 1, id);
    }
    let tmp = std::env::temp_dir().join(format!("sc_c18_unknown_{tag}.xlsx"));
    let _ = std::fs::remove_file(&tmp);
    sheetcore::xlsx::save(&tmp, &wb).expect("保存");
    tmp
}

#[test]
fn quote_flag_is_loaded() {
    let wb = sheetcore::xlsx::load(&fixture()).expect("加载夹具");
    let sid = wb.sheets[0].style_of(0, 0);
    assert!(wb.styles.quote_of(sid), "A1 的样式应带 quotePrefix，sid={sid}");
}

#[test]
fn quote_prefix_survives_plain_roundtrip() {
    let tmp = roundtrip("plain", false);
    let xml = styles_xml(&tmp);
    assert!(xml.contains("quotePrefix=\"1\""), "quotePrefix 丢了：{xml}");
}

#[test]
fn quote_prefix_survives_unrelated_edit() {
    let tmp = roundtrip("edit", true);
    let xml = styles_xml(&tmp);
    assert!(xml.contains("quotePrefix=\"1\""), "改 B1 后 A1 的 quotePrefix 丢了");
    // 再开一次：文本仍然是「编号001」，不是 "001"
    let again = sheetcore::xlsx::load(&tmp).expect("重开");
    let cell = again.sheets[0].get(0, 0).expect("A1 仍在");
    assert_eq!(cell.value, sheetcore::Value::Text("编号001".into()));
}
