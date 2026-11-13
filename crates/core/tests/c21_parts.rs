//! C21：改普通格并保存后，宏 / 查询 / 线程批注等部件必须保留。
//!
//! 夹具 `threaded_real.xlsx` 由 Excel 16.0 的 AddCommentThreaded 生成，
//! 含 xl/comments1.xml、xl/threadedComments/threadedComment1.xml、xl/persons/person.xml。

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/c21").join(name)
}

fn parts(path: &Path) -> HashMap<String, Vec<u8>> {
    let file = std::fs::File::open(path).unwrap();
    let mut zip = zip::ZipArchive::new(file).unwrap();
    let mut out = HashMap::new();
    for i in 0..zip.len() {
        let mut f = zip.by_index(i).unwrap();
        let name = f.name().to_string();
        let mut buf = Vec::new();
        f.read_to_end(&mut buf).unwrap();
        out.insert(name, buf);
    }
    out
}

#[test]
fn threaded_comment_parts_survive_editing_a_normal_cell() {
    let src = fixture("threaded_real.xlsx");
    let before = parts(&src);
    for need in [
        "xl/threadedComments/threadedComment1.xml",
        "xl/persons/person.xml",
        "xl/comments1.xml",
    ] {
        assert!(before.contains_key(need), "夹具缺 {need}");
    }
    let mut wb = sheetcore::xlsx::load(&src).expect("加载线程批注夹具");
    // 只改 B1 的数字格式
    let b1 = wb.sheets[0].style_of(0, 1);
    let mut st = wb.styles.get(b1).clone();
    st.format = "0.000".to_string();
    let id = wb.styles.intern(st);
    wb.sheets[0].set_style(0, 1, id);
    let tmp = std::env::temp_dir().join("sc_c21_threaded.xlsx");
    let _ = std::fs::remove_file(&tmp);
    sheetcore::xlsx::save(&tmp, &wb).expect("保存");

    let after = parts(&tmp);
    for need in [
        "xl/threadedComments/threadedComment1.xml",
        "xl/persons/person.xml",
        "xl/comments1.xml",
    ] {
        let a = after.get(need).unwrap_or_else(|| panic!("保存后丢了 {need}"));
        let b = before.get(need).unwrap();
        assert_eq!(a, b, "{need} 内容被改写");
    }
    // 关系也要还在，否则 Excel 认不到线程批注
    let rels = String::from_utf8(after["xl/worksheets/_rels/sheet1.xml.rels"].clone()).unwrap();
    assert!(rels.contains("threadedComment1.xml"), "工作表 rels 丢了线程批注关系：{rels}");
}

#[test]
fn vba_project_survives_editing_a_normal_cell() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/real_macro.xlsm");
    let before = parts(&src);
    let vba_before = before.get("xl/vbaProject.bin").expect("夹具应含 vbaProject.bin").clone();
    let mut wb = sheetcore::xlsx::load(&src).expect("加载宏夹具");
    let sid = wb.sheets[0].style_of(1, 1);
    let mut st = wb.styles.get(sid).clone();
    st.bold = !st.bold;
    let id = wb.styles.intern(st);
    wb.sheets[0].set_style(1, 1, id);
    let tmp = std::env::temp_dir().join("sc_c21_macro.xlsm");
    let _ = std::fs::remove_file(&tmp);
    sheetcore::xlsx::save(&tmp, &wb).expect("保存");
    let after = parts(&tmp);
    let vba_after = after.get("xl/vbaProject.bin").expect("保存后丢了 vbaProject.bin");
    assert_eq!(vba_after, &vba_before, "vbaProject.bin 被改写");
}
