//! C01.2 XLS/XLSX 缓存类型与保存重开。
//!
//! 重点不是「能不能存」，而是「读回来的东西类型对不对」：
//! 错误值在 BIFF8 里是 BOOLERR 记录（fError=1 + 错误码），
//! 在 OOXML 里是 `t="e"`；两条路径都必须还原成 `Value::Error`，
//! 而不是空值、0、或一段 `#Div0` 这样的 Debug 文本。

use sheetcore::formula::ErrKind;
use sheetcore::{Cell, Value, Workbook};

fn dir(tag: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("c01-2-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn book() -> Workbook {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "错误".into();
    wb.sheets[0].set(0, 0, Cell { value: Value::Error(ErrKind::Div0), formula: None });
    wb.sheets[0].set(1, 0, Cell { value: Value::Error(ErrKind::Na), formula: None });
    wb.sheets[0].set(2, 0, Cell { value: Value::Error(ErrKind::Ref), formula: None });
    wb.sheets[0].set(3, 0, Cell { value: Value::Bool(true), formula: None });
    wb.sheets[0].set(4, 0, Cell { value: Value::Number(42.5), formula: None });
    wb.sheets[0].set(5, 0, Cell { value: Value::Text("#DIV/0!".into()), formula: None });
    wb
}

#[test]
fn xls_roundtrip_keeps_error_and_bool_types() {
    let path = dir("xls").join("errors.xls");
    let wb = book();
    sheetcore::xls_write::write(&path, &wb).expect("写 .xls 失败");
    let back = sheetcore::xls_read::load(&path).expect("读 .xls 失败");
    let s = &back.sheets[0];
    assert_eq!(s.value(0, 0), Value::Error(ErrKind::Div0), "#DIV/0! 读回来不是错误值");
    assert_eq!(s.value(1, 0), Value::Error(ErrKind::Na), "#N/A 读回来不是错误值");
    assert_eq!(s.value(2, 0), Value::Error(ErrKind::Ref), "#REF! 读回来不是错误值");
    assert_eq!(s.value(3, 0), Value::Bool(true), "布尔读回来类型不对");
    assert_eq!(s.value(4, 0), Value::Number(42.5));
    assert_eq!(
        s.value(5, 0),
        Value::Text("#DIV/0!".into()),
        "文本 #DIV/0! 不能变成错误值"
    );
}

#[test]
fn xlsx_roundtrip_keeps_error_type_after_a_second_save() {
    let path = dir("xlsx").join("errors.xlsx");
    let wb = book();
    sheetcore::xlsx::save_atomic(&path, &wb).expect("保存失败");
    let once = sheetcore::xlsx::load(&path).expect("载入失败");
    assert_eq!(once.sheets[0].value(0, 0), Value::Error(ErrKind::Div0));
    // 改一格再存：错误值不能在这一轮里退化成文本
    let mut twice = once.clone();
    twice.sheets[0].set(9, 0, Cell { value: Value::Number(1.0), formula: None });
    sheetcore::xlsx::save_atomic(&path, &twice).expect("二次保存失败");
    let back = sheetcore::xlsx::load(&path).expect("二次载入失败");
    assert_eq!(back.sheets[0].value(0, 0), Value::Error(ErrKind::Div0));
    assert_eq!(back.sheets[0].value(1, 0), Value::Error(ErrKind::Na));
    assert_eq!(back.sheets[0].value(5, 0), Value::Text("#DIV/0!".into()));
}

#[test]
fn xlsx_writes_errors_as_type_e_not_text() {
    let path = dir("raw").join("errors.xlsx");
    sheetcore::xlsx::save_atomic(&path, &book()).expect("保存失败");
    // 直接看包里的 sheet xml：错误格必须是 t="e"
    let bytes = sheetcore::fixups::read_part(&path, "xl/worksheets/sheet1.xml")
        .or_else(|| sheetcore::fixups::read_part(&path, "xl/worksheets/sheet2.xml"))
        .expect("找不到工作表部件");
    let xml = String::from_utf8_lossy(&bytes);
    assert!(xml.contains("t=\"e\""), "错误值没有写成 t=\"e\"：{xml}");
    assert!(
        xml.contains("<v>#DIV/0!</v>"),
        "错误值没有写成 #DIV/0! 文本：{xml}"
    );
    // 文本 #DIV/0! 必须是 t="str"/inlineStr，不能也是 t="e"
    let count_e = xml.matches("t=\"e\"").count();
    assert_eq!(count_e, 3, "只有三个真错误值应该是 t=\"e\"，实际 {count_e}");
}

#[test]
fn external_link_cache_keeps_error_type() {
    // 外链缓存部件里 t="e" 的格子，读回模型要是错误值
    let mut link = sheetcore::xlinks::ExternalBook::default();
    link.index = 1;
    link.part = "xl/externalLinks/externalLink1.xml".into();
    link.target = "预算.xlsx".into();
    link.sheets = vec!["明细".into()];
    link.cache.insert((0, 0, 0), Value::Error(ErrKind::Div0));
    link.cache.insert((0, 1, 0), Value::Number(5.0));
    let path = dir("xlink").join("book.xlsx");
    let mut wb = Workbook::new_blank();
    wb.links.push(link);
    sheetcore::xlsx::save_atomic(&path, &wb).expect("保存失败");
    let back = sheetcore::xlsx::load(&path).expect("载入失败");
    let b = back.links.first().expect("没有读回外链");
    assert_eq!(
        b.cache.get(&(0, 0, 0)),
        Some(&Value::Error(ErrKind::Div0)),
        "外链缓存里的错误值变成了 {:?}",
        b.cache.get(&(0, 0, 0))
    );
    assert_eq!(b.cache.get(&(0, 1, 0)), Some(&Value::Number(5.0)));
}
