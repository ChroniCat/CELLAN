//! 冻结窗格：读、改、存
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/qa02").join(name)
}

#[test]
fn freeze_roundtrip() {
    let src = fixture("freeze.xlsx");
    assert!(src.exists(), "required test fixture is missing: {}", src.display());
    let mut wb = sheetcore::xlsx::load(&src).expect("读入");
    assert_eq!(wb.sheets[0].freeze, Some((1, 2)), "应读出冻结首行 + 前两列");

    // 改成冻结 3 行 1 列
    wb.sheets[0].freeze = Some((3, 1));
    wb.sheets[0].freeze_dirty = true;
    let out = std::env::current_exe().expect("test exe").parent().expect("test dir").join(format!("freeze_edit_{}.xlsx", std::process::id()));
    let _ = std::fs::remove_file(&out);
    sheetcore::xlsx::save(&out, &wb).expect("保存");

    let back = sheetcore::xlsx::load(&out).expect("重新读入");
    assert_eq!(back.sheets[0].freeze, Some((3, 1)), "改后应保存");

    // 取消冻结
    let mut wb2 = back;
    wb2.sheets[0].freeze = None;
    wb2.sheets[0].freeze_dirty = true;
    let out2 = std::env::current_exe().expect("test exe").parent().expect("test dir").join(format!("freeze_none_{}.xlsx", std::process::id()));
    let _ = std::fs::remove_file(&out2);
    sheetcore::xlsx::save(&out2, &wb2).expect("保存取消");
    let back2 = sheetcore::xlsx::load(&out2).expect("再读");
    assert_eq!(back2.sheets[0].freeze, None, "取消后应无冻结");
}
