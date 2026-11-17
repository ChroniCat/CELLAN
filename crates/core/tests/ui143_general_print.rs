//! UI-14.3：打印/PDF 的 General 显示也要按列宽决定，不能屏幕上科学计数、
//! 打印出来却是完整数字。

use sheetcore::model::{Cell, Value, Workbook};
use sheetcore::print;

#[test]
fn print_general_respects_column_width() {
    let mut wb = Workbook::new_blank();
    {
        let sh = wb.sheet_mut(0);
        sh.set(0, 0, Cell { value: Value::Number(1234567890.0), formula: None });
        sh.set(1, 0, Cell { value: Value::Number(12345678901.0), formula: None });
        // 宽列：11 位整数原样显示（Excel 宽列口径）
        sh.cols.set(1, 40.0 * 8.0 + 5.0);
        sh.set(0, 1, Cell { value: Value::Number(12345678901.0), formula: None });
        sh.page.print_area = Some((0, 0, 1, 1));
    }
    let sh = wb.sheet(0);
    let lay = print::paginate(sh, &sh.page);
    let ops = print::page_ops_with(&wb, 0, &lay, 0, None);
    let texts: Vec<&str> = ops.texts.iter().map(|t| t.text.as_str()).collect();
    let joined = texts.join("|");
    assert!(joined.contains("1.23E+09"), "默认列宽 10 位整数应科学计数：{joined}");
    assert!(joined.contains("1.23E+10"), "默认列宽 11 位整数应科学计数：{joined}");
    assert!(joined.contains("12345678901"), "宽列 11 位整数应原样：{joined}");
    assert!(!joined.contains("1234567890|"), "不应打印未压缩的 10 位整数：{joined}");
}
