//! 数组公式（CSE）：读得到 `t="array" ref`，存得回去。

use sheetcore::arr;
use std::path::PathBuf;

fn fixture() -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/qa02/array.xlsx")
}

#[test]
fn reads_array_formula_from_file() {
    let path = fixture();
    assert!(path.exists(), "required test fixture is missing: {}", path.display());
    let found = arr::scan(&path);
    let list = found.get("数组").expect("没找到「数组」表里的数组公式");
    assert_eq!(list.len(), 1, "{list:?}");
    assert_eq!(list[0].anchor, (3, 1), "锚点应该是 B4");
    assert_eq!(list[0].range, (3, 1, 3, 3), "区域应该是 B4:D4");
}

#[test]
fn load_keeps_array_and_save_writes_it_back() {
    let src = fixture();
    assert!(src.exists(), "required test fixture is missing: {}", src.display());
    let wb = sheetcore::xlsx::load(&src).expect("加载失败");
    let sheet = &wb.sheets[0];
    assert_eq!(sheet.arrays.len(), 1, "加载后应记住 1 条数组公式");
    assert_eq!(sheet.arrays[0].1, (3, 1, 3, 3));
    // 公式本身也要在
    let cell = sheet.get(3, 1).expect("B4 应该有内容");
    let f = cell.formula.as_ref().expect("B4 应该有公式");
    assert!(f.contains("SUM"), "公式文本丢了: {f}");

    let out = std::env::current_exe()
        .expect("test executable path")
        .parent()
        .expect("test executable directory")
        .join(format!("array_roundtrip_{}.xlsx", std::process::id()));
    let _ = std::fs::remove_file(&out);
    sheetcore::xlsx::save(&out, &wb).expect("保存失败");

    let again = arr::scan(&out);
    let list = again.get("数组").expect("保存后数组标记丢了");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].range, (3, 1, 3, 3), "区域写回不一致");

    // 原始 XML 里必须真有 t="array"
    let targets = sheetcore::patch::sheet_targets(&out).unwrap();
    let part = targets.get("数组").unwrap();
    let file = std::fs::File::open(&out).unwrap();
    let mut zip = zip::ZipArchive::new(file).unwrap();
    use std::io::Read;
    let mut xml = String::new();
    zip.by_name(part).unwrap().read_to_string(&mut xml).unwrap();
    assert!(xml.contains("t=\"array\""), "XML 里没有 array 标记:\n{xml}");
    assert!(xml.contains("ref=\"B4:D4\""), "XML 里没有 ref:\n{xml}");
    let _ = std::fs::remove_file(&out);
}
