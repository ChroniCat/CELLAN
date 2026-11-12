//! 工作表隐藏/取消隐藏：`<sheet state="hidden">` 的往返。

use sheetcore::model::Workbook;

fn tmp(name: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("sheets-native-hide-{}-{name}", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

fn workbook_xml(path: &std::path::Path) -> String {
    let file = std::fs::File::open(path).unwrap();
    let mut zip = zip::ZipArchive::new(file).unwrap();
    let mut f = zip.by_name("xl/workbook.xml").unwrap();
    let mut s = String::new();
    use std::io::Read;
    f.read_to_string(&mut s).unwrap();
    s
}

fn fixture() -> Workbook {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "可见".into();
    let mut h = sheetcore::Sheet::new("隐藏的");
    h.set(
        0,
        0,
        sheetcore::Cell {
            value: sheetcore::Value::Number(1.0),
            formula: None,
        },
    );
    wb.sheets.push(h);
    wb
}

#[test]
fn hides_and_restores_a_sheet() {
    let mut wb = fixture();
    wb.sheets[1].hidden = true;
    wb.sheets[1].hidden_dirty = true;
    let out = tmp("hide.xlsx");
    sheetcore::xlsx::save(&out, &wb).unwrap();
    let xml = workbook_xml(&out);
    assert!(
        xml.contains("state=\"hidden\""),
        "workbook.xml 里要有 hidden：{xml}"
    );
    // 回读
    let wb2 = sheetcore::xlsx::load(&out).unwrap();
    assert!(!wb2.sheet(0).hidden);
    assert!(wb2.sheet(1).hidden, "第二张表应是隐藏的");
    // 再显示回来
    let mut wb3 = wb2;
    wb3.sheets[1].hidden = false;
    wb3.sheets[1].hidden_dirty = true;
    sheetcore::xlsx::save(&out, &wb3).unwrap();
    let xml2 = workbook_xml(&out);
    assert!(!xml2.contains("state=\"hidden\""), "state 要摘掉：{xml2}");
    let wb4 = sheetcore::xlsx::load(&out).unwrap();
    assert!(!wb4.sheet(1).hidden);
    let _ = std::fs::remove_file(&out);
}

#[test]
fn refuses_to_hide_every_sheet() {
    let mut wb = fixture();
    for s in wb.sheets.iter_mut() {
        s.hidden = true;
        s.hidden_dirty = true;
    }
    let out = tmp("all.xlsx");
    // 保存本身成功（写回那步会报错并打日志），但文件里至少要留一张可见表
    let _ = sheetcore::xlsx::save(&out, &wb);
    let xml = workbook_xml(&out);
    let hidden = xml.matches("state=\"hidden\"").count();
    assert!(hidden < 2, "不能两张都隐藏：{xml}");
    let _ = std::fs::remove_file(&out);
}
