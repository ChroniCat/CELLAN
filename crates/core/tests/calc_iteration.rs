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
use sheetcore::formula::deps;
use sheetcore::{Cell, Value, Workbook};

fn f(formula: &str) -> Cell {
    Cell { value: Value::Empty, formula: Some(formula.into()) }
}

#[test]
fn circular_pair_converges_like_excels_iteration() {
    // Excel：迭代开启、MaxIterations=100、MaxChange=0.001 时 A1=B1+1 / B1=A1/2 → 2 / 1
    let mut wb = Workbook::new_blank();
    {
        let s = &mut wb.sheets[0];
        s.set(0, 0, f("=B1+1"));
        s.set(0, 1, f("=A1/2"));
    }
    wb.calc_iterate = true;
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

#[test]
fn iteration_settings_survive_save_and_reload() {
    let dir = std::env::temp_dir().join("sheets_native_test");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("calc_iter_roundtrip.xlsx");
    let _ = std::fs::remove_file(&path);

    let mut wb = Workbook::new_blank();
    wb.calc_iterate = true;
    wb.calc_max_iterations = 250;
    wb.calc_max_change = 0.0005;
    wb.sheets[0].set(0, 0, f("=B1+1"));
    wb.sheets[0].set(0, 1, f("=A1/2"));
    sheetcore::xlsx::save(&path, &wb).unwrap();

    let back = sheetcore::xlsx::load(&path).unwrap();
    assert!(back.calc_iterate, "iterate 丢里");
    assert_eq!(back.calc_max_iterations, 250);
    assert!((back.calc_max_change - 0.0005).abs() < 1e-12);
}

#[test]
fn iteration_incremental_matches_full_recalc() {
    let build = || {
        let mut wb = Workbook::new_blank();
        wb.calc_iterate = true;
        wb.calc_max_iterations = 200;
        wb.calc_max_change = 1e-9;
        {
            let s = &mut wb.sheets[0];
            s.set(0, 0, f("=B1+1"));
            s.set(0, 1, f("=A1/2"));
            s.set(0, 2, f("=A1*2"));
            s.set(0, 3, f("=C1+1"));
        }
        wb
    };

    // 全量：把 B1 改成 =A1/4，然后整簿重算
    let mut full = build();
    formula::recalc_all(&mut full);
    full.sheets[0].set(0, 1, f("=A1/4"));
    formula::recalc_all(&mut full);

    // 增量：同样改 B1，只重算受影响格
    let mut inc = build();
    formula::recalc_all(&mut inc);
    inc.sheets[0].set(0, 1, f("=A1/4"));
    let graph = deps::Deps::build(&inc);
    formula::recalc_dirty(&mut inc, &graph, &[(0, 0, 1)]);

    let get = |wb: &Workbook, r: u32, c: u32| match wb.sheets[0].value(r, c) {
        Value::Number(n) => n,
        other => panic!("不是数字：{other:?}"),
    };
    for (r, c, name) in [(0u32, 0u32, "A1"), (0, 1, "B1"), (0, 2, "C1"), (0, 3, "D1")] {
        let fv = get(&full, r, c);
        let iv = get(&inc, r, c);
        assert!(
            (fv - iv).abs() <= 1e-6 * fv.abs().max(1.0),
            "{name}: 全量 {fv} != 增量 {iv}"
        );
    }
    eprintln!(
        "[calciter-inc] A1={} B1={} C1={} D1={}",
        get(&inc, 0, 0),
        get(&inc, 0, 1),
        get(&inc, 0, 2),
        get(&inc, 0, 3)
    );
}
