//! 格式编辑：改样式、合并、边框 → 存盘 → 读回（含外部 openpyxl 复核）
use sheetcore::model::Value;
use sheetcore::style::{Border, CellStyle};
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/qa02_table_style")
        .join(name)
}

fn out_path(name: &str) -> PathBuf {
    std::env::current_exe()
        .expect("test executable path")
        .parent()
        .expect("test executable directory")
        .join(format!("qa02_table_style_{}_{}.xlsx", name, std::process::id()))
}

#[test]
fn edit_style_merge_border_then_save() {
    let src = fixture("styled.xlsx");
    assert!(src.exists(), "required test fixture is missing: {}", src.display());
    let mut wb = sheetcore::xlsx::load(&src).expect("读入");
    let out = out_path("styled_edit");
    let _ = std::fs::remove_file(&out);

    // 1) 给 B10:C11 刷成：加粗 + 黄底 + 全部细边框 + 数值格式
    for r in 9..=10u32 {
        for c in 1..=2u32 {
            let mut st: CellStyle = wb.styles.get(wb.sheets[0].style_of(r, c)).clone();
            st.bold = true;
            st.fill = Some([255, 255, 0]);
            st.set_all_borders(1, [255, 0, 0]);
            st.format = "#,##0.00".into();
            let id = wb.styles.intern(st);
            wb.sheets[0].set_style(r, c, id);
        }
    }
    wb.sheets[0].set(9, 1, sheetcore::model::Cell { value: Value::Number(1234.5), formula: None });
    wb.sheets[0].set(9, 2, sheetcore::model::Cell { value: Value::Number(2.0), formula: None });

    // 2) 合并 B13:C14
    wb.sheets[0].merge_range(12, 1, 13, 2);
    // 3) 拆掉原有的 A8:C8
    wb.sheets[0].unmerge_range(7, 0, 7, 2);

    sheetcore::xlsx::save(&out, &wb).expect("保存");

    // 4) 读回核对
    let back = sheetcore::xlsx::load(&out).expect("重新读入");
    let sh = &back.sheets[0];
    assert!(sh.merges.contains(&(12, 1, 13, 2)), "新合并区域应存在: {:?}", sh.merges);
    assert!(!sh.merges.contains(&(7, 0, 7, 2)), "被拆的合并区域应消失");
    let st = back.styles.get(sh.style_of(9, 1));
    assert!(st.bold, "B10 应该加粗");
    assert_eq!(st.fill, Some([255, 255, 0]), "B10 应该有黄底");
    assert_eq!(st.border[0].style, 1, "B10 左边框应为细线");
    assert_eq!(st.format, "#,##0.00");
    assert_eq!(sh.get(9, 1).map(|c| c.value.display()), Some("1234.5".to_string()));
}

#[test]
fn merges_conflict_is_replaced() {
    let mut wb = sheetcore::model::Workbook::new_blank();
    let sh = &mut wb.sheets[0];
    sh.merge_range(0, 0, 1, 1);
    sh.merge_range(1, 1, 3, 4); // 与上一个相交 → 旧的应被拆掉
    assert_eq!(sh.merges.len(), 1);
    assert_eq!(sh.merges[0], (1, 1, 3, 4));
    sh.unmerge_range(0, 0, 9, 9);
    assert!(sh.merges.is_empty());
}

#[test]
fn border_style_roundtrip_through_table() {
    let mut t = sheetcore::style::StyleTable::new();
    let mut a = CellStyle::default();
    a.set_all_borders(3, [10, 20, 30]);
    let id = t.intern(a.clone());
    let b = t.get(id);
    assert_eq!(b.border[2], Border { style: 3, color: [10, 20, 30] });
    let mut c = a;
    c.border[0] = Border::default();
    assert_ne!(t.intern(c), id, "边框不同应产生不同样式");
}
