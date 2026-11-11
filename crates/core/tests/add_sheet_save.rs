//! 新增工作表后存回原文件：原来的表不能被挤掉。
//!
//! 这是方案摘要、复制表、以及界面上「+ 新建工作表」共用的路径，
//! 之前新表会占用 sheet1.xml，把第一张表整份覆盖掉。

use sheetcore::model::{Cell, Sheet, Value};

fn tmpdir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("sn_addsheet_{}_{}", tag, std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn cell_num(v: f64) -> Cell {
    Cell {
        value: Value::Number(v),
        formula: None,
    }
}

#[test]
fn adding_a_sheet_and_saving_over_the_original_keeps_every_sheet() {
    let dir = tmpdir("basic");
    let path = dir.join("t.xlsx");

    // 1) 两张表的新文件
    let mut wb = sheetcore::model::Workbook::default();
    let mut s1 = Sheet::new("Alpha");
    s1.set(0, 0, cell_num(1.0));
    wb.sheets.push(s1);
    let mut s2 = Sheet::new("Beta");
    s2.set(0, 0, cell_num(2.0));
    wb.sheets.push(s2);
    sheetcore::xlsx::save(&path, &wb).unwrap();

    // 2) 打开、再加一张表、原地保存
    let mut live = sheetcore::xlsx::load(&path).unwrap();
    live.path = Some(path.clone());
    let mut s3 = Sheet::new("Gamma");
    s3.set(0, 0, cell_num(3.0));
    assert!(sheetcore::sheet_ops::insert_sheet(&mut live, 2, s3));
    sheetcore::xlsx::save(&path, &live).unwrap();

    // 3) 重新打开：三张表都在，值也对
    let back = sheetcore::xlsx::load(&path).unwrap();
    let names: Vec<&str> = back.sheets.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["Alpha", "Beta", "Gamma"], "加表后存回原文件丢了表");
    assert_eq!(back.sheets[0].get(0, 0).unwrap().value.display(), "1");
    assert_eq!(back.sheets[1].get(0, 0).unwrap().value.display(), "2");
    assert_eq!(back.sheets[2].get(0, 0).unwrap().value.display(), "3");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn adding_a_sheet_in_the_middle_keeps_order_and_content() {
    let dir = tmpdir("middle");
    let path = dir.join("t.xlsx");

    let mut wb = sheetcore::model::Workbook::default();
    for (i, name) in ["A", "B"].iter().enumerate() {
        let mut s = Sheet::new(*name);
        s.set(0, 0, cell_num(i as f64 + 1.0));
        wb.sheets.push(s);
    }
    sheetcore::xlsx::save(&path, &wb).unwrap();

    let mut live = sheetcore::xlsx::load(&path).unwrap();
    live.path = Some(path.clone());
    let mut mid = Sheet::new("Mid");
    mid.set(0, 0, cell_num(99.0));
    assert!(sheetcore::sheet_ops::insert_sheet(&mut live, 1, mid));
    sheetcore::xlsx::save(&path, &live).unwrap();

    let back = sheetcore::xlsx::load(&path).unwrap();
    let names: Vec<&str> = back.sheets.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["A", "Mid", "B"], "插入位置不对");
    assert_eq!(back.sheets[0].get(0, 0).unwrap().value.display(), "1");
    assert_eq!(back.sheets[1].get(0, 0).unwrap().value.display(), "99");
    assert_eq!(back.sheets[2].get(0, 0).unwrap().value.display(), "2");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn adding_a_sheet_then_save_as_keeps_every_sheet() {
    let dir = tmpdir("saveas");
    let path = dir.join("t.xlsx");
    let out = dir.join("out.xlsx");

    let mut wb = sheetcore::model::Workbook::default();
    let mut s1 = Sheet::new("Alpha");
    s1.set(0, 0, cell_num(1.0));
    wb.sheets.push(s1);
    let mut s2 = Sheet::new("Beta");
    s2.set(0, 0, cell_num(2.0));
    wb.sheets.push(s2);
    sheetcore::xlsx::save(&path, &wb).unwrap();

    let mut live = sheetcore::xlsx::load(&path).unwrap();
    live.path = Some(path.clone());
    let mut s3 = Sheet::new("Gamma");
    s3.set(0, 0, cell_num(3.0));
    assert!(sheetcore::sheet_ops::insert_sheet(&mut live, 2, s3));
    // 另存到新路径（基准仍是 path）
    sheetcore::xlsx::save(&out, &live).unwrap();

    let back = sheetcore::xlsx::load(&out).unwrap();
    let names: Vec<&str> = back.sheets.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["Alpha", "Beta", "Gamma"], "另存时丢了表");
    assert_eq!(back.sheets[0].get(0, 0).unwrap().value.display(), "1");
    assert_eq!(back.sheets[2].get(0, 0).unwrap().value.display(), "3");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn adding_a_sheet_then_save_atomic_keeps_every_sheet() {
    let dir = tmpdir("atomic");
    let path = dir.join("t.xlsx");
    let out = dir.join("out.xlsx");

    let mut wb = sheetcore::model::Workbook::default();
    let mut s1 = Sheet::new("Alpha");
    s1.set(0, 0, cell_num(1.0));
    wb.sheets.push(s1);
    sheetcore::xlsx::save(&path, &wb).unwrap();

    let mut live = sheetcore::xlsx::load(&path).unwrap();
    live.path = Some(path.clone());
    let mut s2 = Sheet::new("Gamma");
    s2.set(0, 0, cell_num(3.0));
    assert!(sheetcore::sheet_ops::insert_sheet(&mut live, 1, s2));
    sheetcore::xlsx::save_atomic(&out, &live).unwrap();

    let back = sheetcore::xlsx::load(&out).unwrap();
    let names: Vec<&str> = back.sheets.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["Alpha", "Gamma"], "save_atomic 时丢了表");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 基准文件是 Excel 存的（含 calcChain、x14 扩展等）时，加表再存不能丢表
#[test]
fn excel_authored_base_keeps_sheets_after_adding_one() {
    let dir = tmpdir("excelbase");
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/two_sheets.xlsx");
    let path = dir.join("t.xlsx");
    std::fs::copy(&src, &path).unwrap();

    let mut live = sheetcore::xlsx::load(&path).unwrap();
    live.path = Some(path.clone());
    assert_eq!(live.sheets.len(), 2, "基准应有两张表");
    let mut s3 = Sheet::new("Gamma");
    s3.set(0, 0, cell_num(3.0));
    assert!(sheetcore::sheet_ops::insert_sheet(&mut live, 2, s3));
    sheetcore::xlsx::save_atomic(&path, &live).unwrap();

    let back = sheetcore::xlsx::load(&path).unwrap();
    let names: Vec<&str> = back.sheets.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names, vec!["Alpha", "Beta", "Gamma"], "Excel 基准下加表丢了表");
    let _ = std::fs::remove_dir_all(&dir);
}
