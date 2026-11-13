//! UI-15.3 双下划线与竖排文字：样式模型、xlsx 往返、不被单线/旋转替代。

use sheetcore::model::{Cell, Value, Workbook};
use sheetcore::style::CellStyle;

fn temp(name: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("sc_ui153_{name}_{}.xlsx", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

#[test]
fn double_underline_is_a_separate_style_not_single() {
    let mut wb = Workbook::new_blank();
    let mut st = CellStyle::default();
    st.underline = true;
    st.double_underline = true;
    let id = wb.styles.intern(st);
    wb.sheet_mut(0).set(0, 0, Cell { value: Value::Text("财务报表".into()), formula: None });
    wb.sheet_mut(0).set_style(0, 0, id);
    assert!(wb.styles.get(id).double_underline);
    let single = wb.styles.intern(CellStyle { underline: true, ..Default::default() });
    assert_ne!(single, id, "双线不能和单线共用同一个 xf");
}

#[test]
fn vertical_text_is_not_rotation_90() {
    let mut wb = Workbook::new_blank();
    let mut st = CellStyle::default();
    st.vertical_text = true;
    let id = wb.styles.intern(st);
    assert_eq!(wb.styles.get(id).rotation, 0, "竖排不能写成 rotation=90");
    let rot = wb.styles.intern(CellStyle { rotation: 90, ..Default::default() });
    assert_ne!(id, rot, "竖排与 90° 旋转是不同的样式");
}

#[test]
fn double_underline_and_vertical_text_survive_xlsx_roundtrip() {
    let path = temp("rt");
    let mut wb = Workbook::new_blank();
    wb.sheet_mut(0).set(0, 0, Cell { value: Value::Text("财务报表".into()), formula: None });
    let mut st = CellStyle::default();
    st.underline = true;
    st.double_underline = true;
    st.vertical_text = true;
    let id = wb.styles.intern(st);
    wb.sheet_mut(0).set_style(0, 0, id);
    sheetcore::xlsx::save(&path, &wb).expect("save");

    let back = sheetcore::xlsx::load(&path).expect("load");
    let st = back.styles.get(back.sheet(0).style_of(0, 0)).clone();
    assert!(st.underline, "下划线丢失");
    assert!(st.double_underline, "双下划线丢失（被降级成单线）");
    assert!(st.vertical_text, "竖排标记丢失");
    assert_eq!(st.rotation, 0, "竖排不应变成旋转");
    let xml = {
        let f = std::fs::File::open(&path).unwrap();
        let mut zip = zip::ZipArchive::new(f).unwrap();
        let mut s = String::new();
        use std::io::Read;
        zip.by_name("xl/styles.xml").unwrap().read_to_string(&mut s).unwrap();
        s
    };
    assert!(xml.contains(r#"val="double""#), "styles.xml 应有 <u val=\"double\"/>");
    assert!(xml.contains(r#"textRotation="255""#), "styles.xml 应有 textRotation=255");
    let _ = std::fs::remove_file(&path);
}
