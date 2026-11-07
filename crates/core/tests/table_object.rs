//! Excel 表格对象：读得出来、存得回去、结构化引用能算。

use sheetcore::table;

#[test]
fn reads_table_object() {
    let path = std::path::Path::new("F:/Finance/work/table_obj.xlsx");
    if !path.exists() {
        return;
    }
    let wb = sheetcore::xlsx::load(path).expect("加载失败");
    let sheet = wb.sheets.iter().find(|s| s.name == "报价").expect("没有「报价」表");
    assert_eq!(sheet.tables.len(), 1, "{:?}", sheet.tables);
    let t = &sheet.tables[0];
    assert_eq!(t.name, "Table1");
    assert_eq!(t.range, (0, 0, 3, 2));
    assert_eq!(t.columns, vec!["姓名", "部门", "金额"]);
}

#[test]
fn structured_reference_resolves() {
    let path = std::path::Path::new("F:/Finance/work/table_obj.xlsx");
    if !path.exists() {
        return;
    }
    let wb = sheetcore::xlsx::load(path).expect("加载失败");
    let si = wb.sheets.iter().position(|s| s.name == "报价").unwrap();
    // 展开结果应该是数值列（C 列）且跳过标题行
    let expanded = table::expand_structured(&wb, si, "SUM(Table1[金额])").expect("没展开");
    assert!(expanded.contains("C2:C4"), "{expanded}");
    // 真的能算：500+300+50
    let v = sheetcore::formula::eval_formula(&wb, si, "=SUM(Table1[金额])");
    assert_eq!(format!("{v:?}").contains("850"), true, "{v:?}");
    let n = sheetcore::formula::eval_formula(&wb, si, "=COUNTA(Table1[姓名])");
    assert_eq!(format!("{n:?}").contains('3'), true, "{n:?}");
}

#[test]
fn save_keeps_table_parts() {
    let path = std::path::Path::new("F:/Finance/work/table_obj.xlsx");
    if !path.exists() {
        return;
    }
    let wb = sheetcore::xlsx::load(path).expect("加载失败");
    let out = std::env::temp_dir().join("table_obj_out.xlsx");
    let _ = std::fs::remove_file(&out);
    sheetcore::xlsx::save(&out, &wb).expect("保存失败");

    let found = table::load(&out);
    let list = found.get("报价").expect("保存后表格对象丢了");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].name, "Table1");
    assert_eq!(list[0].columns.len(), 3);

    // 工作表里要有 tableParts
    let targets = sheetcore::patch::sheet_targets(&out).unwrap();
    let part = targets.get("报价").unwrap();
    let f = std::fs::File::open(&out).unwrap();
    let mut zip = zip::ZipArchive::new(f).unwrap();
    use std::io::Read;
    let mut xml = String::new();
    zip.by_name(part).unwrap().read_to_string(&mut xml).unwrap();
    assert!(xml.contains("<tableParts"), "工作表里没有 tableParts:\n{xml}");
    let _ = std::fs::remove_file(&out);
}
