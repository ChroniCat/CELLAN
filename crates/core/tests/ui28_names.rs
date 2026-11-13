//! UI-28：定义名称的作用域与重算。
use sheetcore::formula::recalc_all;
use sheetcore::model::{Cell, Sheet, Value, Workbook};

fn text(wb: &Workbook, si: usize, r: u32, c: u32) -> String {
    wb.sheets[si].get(r, c).map(|x| x.value.display()).unwrap_or_default()
}

#[test]
fn local_name_beats_workbook_name_in_same_sheet() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "S1".into();
    let mut s2 = Sheet::new("S2");
    wb.sheets.push(s2.clone());
    wb.sheets[1].name = "S2".into();
    wb.sheets[0].set(0, 0, Cell { value: Value::Number(1.0), formula: None });
    wb.sheets[1].set(0, 0, Cell { value: Value::Number(2.0), formula: None });
    wb.names.push(("值".into(), Some(0), "S1!$A$1".into()));
    wb.names.push(("值".into(), None, "S2!$A$1".into()));
    wb.sheets[0].set(0, 1, Cell { value: Value::Empty, formula: Some("=值".into()) });
    wb.sheets[1].set(0, 1, Cell { value: Value::Empty, formula: Some("=值".into()) });
    recalc_all(&mut wb);
    assert_eq!(text(&wb, 0, 0, 1), "1", "S1 用局部名称");
    assert_eq!(text(&wb, 1, 0, 1), "2", "S2 没有局部名称，用工作簿名称");
    let _ = s2;
}

#[test]
fn names_survive_save_and_load() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(0, 0, Cell { value: Value::Number(100.0), formula: None });
    wb.names.push(("税率".into(), None, "=0.13".into()));
    wb.names_dirty = true;
    wb.sheets[0].set(0, 1, Cell { value: Value::Empty, formula: Some("=A1*税率".into()) });
    recalc_all(&mut wb);
    let p = std::env::temp_dir().join("sc_ui28_names.xlsx");
    let _ = std::fs::remove_file(&p);
    sheetcore::xlsx::save(&p, &wb).unwrap();
    let got = sheetcore::xlsx::load(&p).unwrap();
    assert!(got.names.iter().any(|(n, sh, _)| n == "税率" && sh.is_none()));
    assert_eq!(text(&got, 0, 0, 1), "13");
    let _ = std::fs::remove_file(&p);
}
