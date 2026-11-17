//! UI-15.2：.xls（BIFF8）工作表保护 + 旧式口令哈希往返。

use sheetcore::model::{Sheet, Value, Workbook};
use sheetcore::protection::{legacy_hash, SheetProtection};

#[test]
fn xls_protect_and_password_hash_roundtrip() {
    let mut wb = Workbook::new_blank();
    let mut sh = Sheet::new("Protected");
    sh.set(0, 0, sheetcore::model::Cell { value: Value::Text("锁定".into()), formula: None });
    let hash = legacy_hash("s3cret");
    let mut p = SheetProtection::default();
    p.sheet = true;
    p.legacy = Some(hash);
    sh.protection = Some(p);
    wb.sheets.clear();
    wb.sheets.push(sh);

    let path = std::env::temp_dir().join(format!("ui152_xls_{}.xls", std::process::id()));
    sheetcore::xls_write::write(&path, &wb).expect("写 .xls");
    let back = sheetcore::xls_read::load(&path).expect("读 .xls");
    let _ = std::fs::remove_file(&path);

    let p = back.sheet(0).protection.as_ref().expect(".xls 保护应保留");
    assert!(p.sheet, "PROTECT 的 fLocked 位要回读");
    assert_eq!(p.legacy, Some(hash), "PASSWORD 记录要保留 16 位哈希");
}

#[test]
fn xls_xf_lock_bit_roundtrip_allows_unlocked_cell() {
    let mut wb = Workbook::new_blank();
    wb.sheets.clear();
    let mut sh = Sheet::new("Mix");
    sh.set(0, 0, sheetcore::model::Cell { value: Value::Text("可编辑".into()), formula: None });
    sh.set(0, 1, sheetcore::model::Cell { value: Value::Text("锁定".into()), formula: None });
    let mut unlocked = sheetcore::style::CellStyle::default();
    unlocked.locked = false;
    let id = wb.styles.intern(unlocked);
    sh.set_style(0, 0, id);
    let mut p = SheetProtection::default();
    p.sheet = true;
    sh.protection = Some(p);
    wb.sheets.push(sh);

    let path = std::env::temp_dir().join(format!("ui152_lock_{}.xls", std::process::id()));
    sheetcore::xls_write::write(&path, &wb).expect("写 .xls");
    let back = sheetcore::xlsx::load(&path).expect("读 .xls");
    let _ = std::fs::remove_file(&path);
    let a1 = back.sheet(0).style_of(0, 0);
    let b1 = back.sheet(0).style_of(0, 1);
    assert!(!back.styles.get(a1).locked, "未锁定格的 XF bit0 要写 0");
    assert!(back.styles.get(b1).locked, "默认格仍锁定");
}

#[test]
fn xls_unprotected_sheet_has_no_protection() {
    let mut wb = Workbook::new_blank();
    wb.sheets.clear();
    wb.sheets.push(Sheet::new("Open"));
    let path = std::env::temp_dir().join(format!("ui152_open_{}.xls", std::process::id()));
    sheetcore::xls_write::write(&path, &wb).expect("写 .xls");
    let back = sheetcore::xls_read::load(&path).expect("读 .xls");
    let _ = std::fs::remove_file(&path);
    assert!(back.sheet(0).protection.is_none());
}
