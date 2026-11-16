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

use sheetcore::formula;
use sheetcore::{Cell, Value, Workbook};

fn f(formula: &str) -> Cell {
    Cell { value: Value::Empty, formula: Some(formula.into()) }
}

#[test]
#[ignore = "迭代求解器未实现：引擎当前对环返回 Error(Circular)，Excel 在迭代开启时收敛到 2/1"]
fn circular_pair_converges_like_excels_iteration() {
    // Excel：迭代开启、MaxIterations=100、MaxChange=0.001 时 A1=B1+1 / B1=A1/2 → 2 / 1
    let mut wb = Workbook::new_blank();
    {
        let s = &mut wb.sheets[0];
        s.set(0, 0, f("=B1+1"));
        s.set(0, 1, f("=A1/2"));
    }
    formula::recalc_all(&mut wb);
    let num = |v: Value| match v {
        Value::Number(n) => n,
        other => panic!("不是数字：{other:?}"),
    };
    let a1 = num(wb.sheets[0].value(0, 0));
    let b1 = num(wb.sheets[0].value(0, 1));
    eprintln!("[calciter] A1={a1} B1={b1}");
    assert!((a1 - 2.0).abs() < 1e-3, "A1={a1}");
    assert!((b1 - 1.0).abs() < 1e-3, "B1={b1}");
}
