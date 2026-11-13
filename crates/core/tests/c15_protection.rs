//! C15：Excel 生成的真实保护夹具 —— 单元格锁定/隐藏、15 个权限位、保存写回。
//!
//! `fixtures/c15/protected.xlsx` 由本机 Excel 16.0 创建：
//! A1 是隐藏公式（锁定），B1 未锁定，工作表保护只允许「设置单元格格式」，
//! 存盘时 Excel 写的是 `<sheetProtection sheet="1" formatCells="0"/>`。

use sheetcore::protection::{ProtOp, SheetProtection};
use sheetcore::xlsx;
use std::path::PathBuf;

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/c15/protected.xlsx")
}

fn temp_copy(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sc_c15_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(format!("{tag}.xlsx"));
    std::fs::copy(fixture(), &p).unwrap();
    p
}

fn sheet_xml(path: &std::path::Path) -> String {
    let f = std::fs::File::open(path).unwrap();
    let mut zip = zip::ZipArchive::new(f).unwrap();
    let mut s = String::new();
    use std::io::Read;
    zip.by_name("xl/worksheets/sheet1.xml").unwrap().read_to_string(&mut s).unwrap();
    s
}

fn part_with(xml: &str, tag: &str) -> String {
    let i = xml.find(tag).unwrap();
    let j = xml[i..].find('>').unwrap() + i + 1;
    xml[i..j].to_string()
}

#[test]
fn excel_authored_protection_is_read_with_cell_level_flags() {
    let wb = xlsx::load(&fixture()).unwrap();
    let sh = &wb.sheets[0];
    let p = sh.protection.as_ref().expect("应有工作表保护");
    assert!(p.sheet, "sheet=1");
    assert!(p.perms.format_cells, "formatCells=\"0\" 表示允许设置单元格格式");
    assert!(!p.perms.sort, "缺省禁止排序");
    assert!(!p.perms.insert_rows && !p.perms.delete_rows);
    assert!(p.perms.select_locked && p.perms.select_unlocked, "选择默认允许");

    // 单元格级：A1 锁定 + 隐藏公式，B1 未锁定
    let a1 = wb.styles.get(sh.style_of(0, 0));
    assert!(a1.locked, "A1 应锁定");
    assert!(a1.hidden, "A1 应隐藏公式");
    let b1 = wb.styles.get(sh.style_of(0, 1));
    assert!(!b1.locked, "B1 是未锁定的输入区");
    assert_eq!(
        sh.get(0, 1).map(|c| c.value.display()),
        Some("输入区".into())
    );
}

#[test]
fn saving_keeps_excel_attribute_encoding_and_does_not_widen_permissions() {
    let path = temp_copy("roundtrip");
    let mut wb = xlsx::load(&path).unwrap();
    // 改一处，让保护标签走我们的写回路径
    wb.sheets[0].protect_dirty = true;
    xlsx::save(&path, &wb).unwrap();

    let xml = sheet_xml(&path);
    let tag = part_with(&xml, "<sheetProtection");
    assert!(tag.contains("formatCells=\"0\""), "允许项应原样写回：{tag}");
    assert!(!tag.contains("sort="), "禁止项不能凭空放宽：{tag}");
    assert!(!tag.contains("insertRows="), "{tag}");
    assert!(!tag.contains("selectLockedCells="), "没有禁止选择就不要写：{tag}");

    // 重新打开：语义不变
    let back = xlsx::load(&path).unwrap();
    let p = back.sheets[0].protection.as_ref().unwrap();
    assert!(p.perms.format_cells);
    assert!(!p.perms.sort);
    assert!(p.perms.select_locked);
    assert!(back.styles.get(back.sheets[0].style_of(0, 0)).hidden);
    assert!(!back.styles.get(back.sheets[0].style_of(0, 1)).locked);
}

#[test]
fn writing_a_fresh_protection_uses_excel_wording() {
    let path = temp_copy("fresh");
    let mut wb = xlsx::load(&path).unwrap();
    let mut p = SheetProtection::new_with_password("pw");
    p.perms.sort = true;
    p.perms.format_cells = false;
    p.perms.select_locked = false;
    wb.sheets[0].protection = Some(p);
    wb.sheets[0].protect_dirty = true;
    xlsx::save(&path, &wb).unwrap();

    let tag = part_with(&sheet_xml(&path), "<sheetProtection");
    assert!(tag.contains("sort=\"0\""), "允许排序要写成 sort=\"0\"：{tag}");
    assert!(tag.contains("selectLockedCells=\"1\""), "禁止选择锁定格写 1：{tag}");
    assert!(!tag.contains("formatCells="), "未允许的不能写：{tag}");
    assert!(tag.contains("algorithmName=\"SHA-512\""), "带密码要写 SHA-512：{tag}");

    let back = xlsx::load(&path).unwrap();
    let p = back.sheets[0].protection.as_ref().unwrap();
    assert!(p.verify("pw"), "密码校验仍然通过");
    assert!(p.perms.sort && !p.perms.format_cells && !p.perms.select_locked);
    assert!(p.can(ProtOp::Sort) && !p.can(ProtOp::FormatCells));
}
