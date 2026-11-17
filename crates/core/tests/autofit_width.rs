//! Excel 16.0 Build 20326 AutoFit 实测：列宽（字符单位）与字符串的对应。
//!
//! 数据来自 `work/probe/excel_autofit_calib2.out`：Excel 对每列执行
//! `Columns(i).AutoFit()` 后读 `ColumnWidth`。目标 Normal 字体是中文版
//! Excel 的「等线 11」；不是字形 advance 的和，而是「ASCII 1 / CJK 2 +
//! 0.88 内边距」，超过 9 个字符后每字符再加 0.125 的像素量化。

#[test]
fn excel_autofit_chars_matches_measured_widths() {
    let cases = [
        ("1", 1.88),
        ("11", 2.88),
        ("111111111", 9.88),
        ("1111111111", 11.0),
        ("11111111111", 12.13),
        ("111111111111", 13.25),
        ("a", 1.88),
        ("aaaaaaaaaa", 11.0),
        (".", 1.88),
        ("...", 3.88),
        ("1.2", 3.88),
        ("1.23", 4.88),
        ("1.234", 5.88),
        ("123.456", 7.88),
        ("1234.5678", 9.88),
        ("0.1", 3.88),
        ("0.01", 4.88),
        ("12345678901", 12.13),
        ("123456789012345", 16.63),
        ("1.23457E+14", 12.13),
        ("-123456789012", 14.38),
        ("中", 2.75),
        ("中文", 4.63),
        ("中文字", 6.5),
        ("中文文字", 8.38),
        ("中文文字中", 10.38),
        ("中文文字中文", 12.38),
        ("ABCDEFGHIJ", 11.0),
        ("abcdefghij", 11.0),
        ("AbCdEfGhIj", 11.0),
        ("Hello World", 12.13),
    ];
    for (s, want) in cases {
        let got = sheetcore::display::excel_autofit_chars(s);
        assert!(
            (got - want).abs() < 0.02,
            "{s:?}: got {got}, want {want}"
        );
    }
}

#[test]
fn styled_autofit_matches_excel_indent_and_bold() {
    // work/probe/excel_autofit_indent2.out（Excel 16.0，Normal=等线 11）
    let cases = [
        ("1", false, 0, 1.88),
        ("1", true, 0, 2.25),
        ("1", false, 1, 4.88),
        ("1", false, 2, 7.88),
        ("1", true, 1, 5.25),
    ];
    for (s, bold, indent, want) in cases {
        let got = sheetcore::display::excel_autofit_chars_styled(s, bold, indent);
        assert!(
            (got - want).abs() < 0.02,
            "{s:?} bold={bold} indent={indent}: got {got}, want {want}"
        );
    }
}

#[test]
fn wrapped_autofit_uses_longest_line() {
    // work/probe/excel_autofit_wrap3.out：Excel 自动换行取最长一行
    let cases = [
        ("1\n22", 2.88),
        ("中文\n中文文字", 8.38),
        ("中文文字中文", 12.38),
    ];
    for (s, want) in cases {
        let got = sheetcore::display::excel_autofit_chars_wrapped(s, false, 0, true);
        assert!(
            (got - want).abs() < 0.02,
            "{s:?}: got {got}, want {want}"
        );
    }
    assert_eq!(
        sheetcore::display::excel_autofit_chars_wrapped("1\n22", false, 0, false),
        sheetcore::display::excel_autofit_chars_styled("1\n22", false, 0)
    );
}
