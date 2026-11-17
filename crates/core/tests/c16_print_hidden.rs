//! QA-09：隐藏行/列（含筛选隐藏）不参与打印，也不占分页高度/宽度。

use sheetcore::model::{Cell, Sheet, Value, Workbook};
use sheetcore::print;

fn set(sh: &mut Sheet, r: u32, c: u32, s: &str) {
    sh.set(r, c, Cell { value: Value::Text(s.into()), formula: None });
}

#[test]
fn hidden_rows_and_cols_are_not_printed() {
    let mut wb = Workbook::new_blank();
    {
        let sh = wb.sheet_mut(0);
        set(sh, 0, 0, "标题A");
        set(sh, 0, 1, "隐藏列B");
        set(sh, 1, 0, "隐藏行R2");
        set(sh, 2, 0, "可见A3");
        set(sh, 3, 0, "筛选隐藏R4");
        set(sh, 4, 0, "可见A5");
        sh.hidden_cols.insert(1);
        sh.hidden_rows_user.insert(1);
        sh.hidden_rows.insert(3); // 筛选隐藏
        sh.page.print_area = Some((0, 0, 6, 3));
        sh.page.title_rows = Some((0, 0));
        sh.page.gridlines = true;
    }
    let sh = wb.sheet(0);
    let lay = print::paginate(sh, &sh.page);
    let ops = print::page_ops_with(&wb, 0, &lay, 0, None);
    let all: Vec<&str> = ops.texts.iter().map(|t| t.text.as_str()).collect();
    let joined = all.join("|");
    assert!(joined.contains("标题A"), "打印标题应在：{joined}");
    assert!(joined.contains("可见A3"), "可见行应打印：{joined}");
    assert!(joined.contains("可见A5"), "可见行应打印：{joined}");
    for hidden in ["隐藏列B", "隐藏行R2", "筛选隐藏R4"] {
        assert!(!joined.contains(hidden), "隐藏内容不应打印：{hidden} in {joined}");
    }
}
