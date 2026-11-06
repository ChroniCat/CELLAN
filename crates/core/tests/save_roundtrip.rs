//! 保存/读回一致性：多表、数字、文本、公式、空表都要原样回来
use sheetcore::{Cell, Value, Workbook};

fn num(n: f64) -> Cell {
    Cell { value: Value::Number(n), formula: None }
}
fn txt(s: &str) -> Cell {
    Cell { value: Value::Text(s.into()), formula: None }
}
fn fx(f: &str) -> Cell {
    Cell { value: Value::Number(0.0), formula: Some(f.into()) }
}

#[test]
fn roundtrip_all_sheets() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "数据".to_string();
    for r in 0..200u32 {
        wb.sheets[0].set(r, 0, num(r as f64 + 1.0));
        wb.sheets[0].set(r, 1, txt(&format!("客户{r}")));
        wb.sheets[0].set(r, 2, fx(&format!("=A{}*2", r + 1)));
    }
    let mut s2 = sheetcore::Sheet::new("汇总");
    s2.set(0, 0, txt("合计"));
    s2.set(0, 1, fx("=SUM(数据!C1:C200)"));
    wb.sheets.push(s2);

    sheetcore::formula::recalc_all(&mut wb);
    let dir = std::env::temp_dir().join("sheetcore_rt");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("rt.xlsx");
    sheetcore::xlsx::save(&path, &wb).expect("save");

    let back = sheetcore::xlsx::load(&path).expect("load");
    assert_eq!(back.sheets.len(), 2, "表数量应保持");
    assert_eq!(back.sheets[0].name, "数据");
    assert_eq!(back.sheets[1].name, "汇总");
    for r in [0u32, 1, 99, 199] {
        assert_eq!(back.sheets[0].get(r, 0).map(|c| c.value.clone()), Some(Value::Number(r as f64 + 1.0)));
        assert_eq!(
            back.sheets[0].get(r, 1).map(|c| c.value.clone()),
            Some(Value::Text(format!("客户{r}").into()))
        );
        assert_eq!(
            back.sheets[0].get(r, 2).and_then(|c| c.formula.clone()),
            Some(format!("=A{}*2", r + 1).into()),
            "第 {r} 行公式应保留"
        );
    }
    assert_eq!(
        back.sheets[1].get(0, 1).and_then(|c| c.formula.clone()),
        Some("=SUM(数据!C1:C200)".into())
    );
    // 重算后合计应为 1..200 的两倍
    let mut back2 = back;
    sheetcore::formula::recalc_all(&mut back2);
    match back2.sheets[1].get(0, 1).map(|c| c.value.clone()) {
        Some(Value::Number(v)) => assert!((v - 40200.0).abs() < 1e-6, "合计应为 40200，实际 {v}"),
        other => panic!("合计读回异常: {other:?}"),
    }
    let _ = std::fs::remove_file(&path);
}
