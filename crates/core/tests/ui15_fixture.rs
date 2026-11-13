//! 读 Excel 造的双下划线/竖排夹具：必须原样读出，不能和单线/90° 旋转混淆。

use sheetcore::xlsx;

#[test]
fn excel_double_underline_and_vertical_are_read_distinctly() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/ui15/style.xlsx");
    let wb = xlsx::load(&path).expect("load fixture");
    let sh = wb.sheet(0);
    let a1 = wb.styles.get(sh.style_of(0, 0)).clone();
    assert!(a1.underline, "A1 应识别为有下划线");
    assert!(a1.double_underline, "A1 是双下划线，不能读成单线");
    assert!(a1.vertical_text, "A1 是竖排（xlVertical），不能读成 90° 旋转");
    assert_eq!(a1.rotation, 0);

    let b2 = wb.styles.get(sh.style_of(1, 1)).clone();
    assert!(b2.underline, "B2 单下划线");
    assert!(!b2.double_underline, "B2 是单线，不能读成双线");

    let c3 = wb.styles.get(sh.style_of(2, 2)).clone();
    assert_eq!(c3.rotation, 90, "C3 是 90° 旋转");
    assert!(!c3.vertical_text, "90° 旋转不能读成竖排");
}
