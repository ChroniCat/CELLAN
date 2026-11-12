//! 批注：读、改、存
use std::path::Path;

fn load() -> Option<sheetcore::model::Workbook> {
    let p = Path::new("tests/test-work/notes.xlsx");
    assert!(p.exists(), "required test fixture is missing: {}", p.display());
    Some(sheetcore::xlsx::load(p).expect("读入"))
}

#[test]
#[ignore = "requires optional synthetic fixtures under tests/test-work"]
fn reads_comments() {
    let Some(wb) = load() else { return };
    let sh = wb.sheet(0);
    assert_eq!(sh.notes.len(), 2, "应读到两条批注: {}", sh.notes.len());
    let b2 = sh
        .notes
        .get(&sheetcore::model::CellRef::new(1, 1))
        .expect("B2 应有批注");
    assert_eq!(b2.text, "这行数据要复核");
    assert_eq!(b2.author, "张三");
    let a3 = sh
        .notes
        .get(&sheetcore::model::CellRef::new(2, 0))
        .expect("A3 应有批注");
    assert_eq!(a3.text, "客户确认过\n金额无误");
    assert_eq!(a3.author, "李四");
}

#[test]
#[ignore = "requires optional synthetic fixtures under tests/test-work"]
fn add_edit_delete_and_save() {
    let Some(mut wb) = load() else { return };
    let out = Path::new("tests/test-work/notes_edit.xlsx");
    let _ = std::fs::remove_file(out);

    // 新增 C5、修改 B2、删除 A3
    wb.sheets[0].notes.insert(
        sheetcore::model::CellRef::new(4, 2),
        sheetcore::notes::Note { r: 4, c: 2, text: "新增的批注".into(), author: "Hermes".into() },
    );
    if let Some(n) = wb.sheets[0].notes.get_mut(&sheetcore::model::CellRef::new(1, 1)) {
        n.text = "改过的内容".into();
    }
    wb.sheets[0].notes.remove(&sheetcore::model::CellRef::new(2, 0));
    wb.sheets[0].notes_dirty = true;

    sheetcore::xlsx::save(out, &wb).expect("保存");

    let back = sheetcore::xlsx::load(out).expect("重新读入");
    let sh = back.sheet(0);
    let get = |r: u32, c: u32| sh.notes.get(&sheetcore::model::CellRef::new(r, c)).map(|n| n.text.clone());
    assert_eq!(get(4, 2), Some("新增的批注".to_string()), "新增的批注应保存");
    assert_eq!(get(1, 1), Some("改过的内容".to_string()), "修改应保存");
    assert_eq!(get(2, 0), None, "删除的批注不应残留");
}
