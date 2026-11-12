//! 样式与合并单元格：读进来、改一格、存出去，再读回来还在不在
use sheetcore::model::Value;
use std::path::Path;

#[test]
#[ignore = "requires optional synthetic fixtures under tests/test-work"]
fn style_and_merge_survive_save() {
    let src = Path::new("tests/test-work/styled.xlsx");
    assert!(src.exists(), "required test fixture is missing: {}", src.display());
    let mut wb = sheetcore::xlsx::load(src).expect("读入");

    // 原始状态
    let merges_before = wb.sheets[0].merges.clone();
    assert!(merges_before.contains(&(0, 0, 0, 5)), "A1:F1 应被识别为合并");
    assert!(merges_before.contains(&(7, 0, 7, 2)), "A8:C8 应被识别为合并");
    assert!(wb.styles.len() > 5, "样式表应有多条样式");
    let a1_style = wb.sheets[0].style_of(0, 0);
    assert_ne!(a1_style, 0, "A1 应有样式");
    let s = wb.styles.get(a1_style);
    assert!(s.bold && s.size > 16.0, "A1 应为大号加粗");
    assert_eq!(s.fill, Some([31, 78, 120]), "A1 应为深蓝底");

    // 改一格再存
    wb.sheets[0].set(2, 0, sheetcore::model::Cell {
        value: Value::Text("改过的产品A".into()),
        formula: None,
    });
    let out = Path::new("tests/test-work/styled_out.xlsx");
    let _ = std::fs::remove_file(out);
    sheetcore::xlsx::save(out, &wb).expect("保存");

    // 读回来
    let back = sheetcore::xlsx::load(out).expect("重新读入");
    assert_eq!(back.sheets[0].merges, merges_before, "合并区域应保留");
    assert_eq!(back.sheets[0].style_of(0, 0), a1_style, "A1 样式应保留");
    assert_eq!(back.styles.get(a1_style).fill, Some([31, 78, 120]));
    assert_eq!(
        back.sheets[0].get(2, 0).map(|c| c.value.display()),
        Some("改过的产品A".to_string()),
        "改动应写入"
    );
    // 合并后的第二张表也应完好
    assert!(back.sheets[1].merges.len() >= 3, "第二张表三个合并区域都应保留");
}

#[test]
fn number_format_display() {
    use sheetcore::model::Value;
    let n = Value::Number(1234.567);
    assert_eq!(sheetcore::style::format_value(&n, "#,##0.00"), "1,234.57");
    assert_eq!(sheetcore::style::format_value(&n, "0.0%"), "123456.7%");
    assert_eq!(sheetcore::style::format_value(&Value::Text("abc".into()), "#,##0.00"), "abc");
}
