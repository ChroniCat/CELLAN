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
    for (text, want) in cases {
        let got = sheetcore::display::excel_autofit_chars(text);
        assert!(
            (got - want).abs() < 0.011,
            "{text:?}: got {got:.2}, want {want:.2}"
        );
    }
}
