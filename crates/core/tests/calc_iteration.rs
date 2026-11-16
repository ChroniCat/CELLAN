//! calcPr 迭代计算属性读写（QT 迭代计算的第一块：设置往返）。
use sheetcore::patch::set_calc_iteration;

#[test]
fn writes_iteration_attrs_and_removes_them_again() {
    let xml = r#"<?xml version="1.0"?><workbook><sheets/></workbook>"#;
    let on = set_calc_iteration(xml, true, 250, 0.0005);
    assert!(on.contains(r#"iterate="1""#), "{on}");
    assert!(on.contains(r#"maxIterations="250""#), "{on}");
    assert!(on.contains(r#"maxChange="0.0005""#), "{on}");
    // 再关掉：三个属性都要消失
    let off = set_calc_iteration(&on, false, 250, 0.0005);
    assert!(!off.contains("iterate"), "{off}");
    assert!(!off.contains("maxIterations"), "{off}");
    assert!(!off.contains("maxChange"), "{off}");
}

#[test]
fn keeps_calc_mode_and_other_calcpr_attrs() {
    let xml = r#"<workbook><calcPr calcId="191029" calcMode="manual"/></workbook>"#;
    let out = set_calc_iteration(xml, true, 100, 0.001);
    assert!(out.contains(r#"calcMode="manual""#), "{out}");
    assert!(out.contains(r#"iterate="1""#), "{out}");
    assert!(out.ends_with("</workbook>"), "{out}");
}
