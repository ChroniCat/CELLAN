//! 增量重算：改一格之后，只有依赖链上的格子更新
use sheetcore::formula::{self, deps::Deps};
use sheetcore::{Cell, Value, Workbook};

fn num(wb: &Workbook, si: usize, r: u32, c: u32) -> f64 {
    match formula::eval_cell(wb, si, r, c) {
        formula::FValue::Number(n) => n,
        other => panic!("({si},{r},{c}) 期望数字，得到 [{}]", other.display()),
    }
}

fn set(wb: &mut Workbook, r: u32, c: u32, v: f64) {
    wb.sheets[0].set(r, c, Cell { value: Value::Number(v), formula: None });
}

fn setf(wb: &mut Workbook, r: u32, c: u32, f: &str) {
    wb.sheets[0].set(r, c, Cell { value: Value::Number(0.0), formula: Some(f.into()) });
}

#[test]
fn only_dependents_are_recalculated() {
    let mut wb = Workbook::new_blank();
    set(&mut wb, 0, 0, 1.0); // A1
    setf(&mut wb, 0, 1, "=A1*2"); // B1 依赖 A1
    setf(&mut wb, 0, 2, "=B1+1"); // C1 依赖 B1
    set(&mut wb, 1, 0, 100.0); // A2 独立
    setf(&mut wb, 1, 1, "=A2*3"); // B2 只依赖 A2
    formula::recalc_all(&mut wb);
    assert_eq!(num(&wb, 0, 0, 1), 2.0);
    assert_eq!(num(&wb, 0, 0, 2), 3.0);
    assert_eq!(num(&wb, 0, 1, 1), 300.0);

    let deps = Deps::build(&wb);
    // 改 A1：应只重算 B1、C1
    set(&mut wb, 0, 0, 10.0);
    let n = formula::recalc_dirty(&mut wb, &deps, &[(0, 0, 0)]);
    assert_eq!(n, 2, "只应重算 B1 和 C1，实际 {n}");
    assert_eq!(num(&wb, 0, 0, 1), 20.0);
    assert_eq!(num(&wb, 0, 0, 2), 21.0);
    assert_eq!(num(&wb, 0, 1, 1), 300.0, "B2 不该被碰");
}

#[test]
fn range_dependencies_propagate() {
    let mut wb = Workbook::new_blank();
    for r in 0..5u32 {
        set(&mut wb, r, 0, r as f64 + 1.0);
    }
    setf(&mut wb, 0, 2, "=SUM(A1:A5)");
    setf(&mut wb, 0, 3, "=C1*10");
    formula::recalc_all(&mut wb);
    assert_eq!(num(&wb, 0, 0, 2), 15.0);

    let deps = Deps::build(&wb);
    set(&mut wb, 2, 0, 30.0); // 改 A3
    let n = formula::recalc_dirty(&mut wb, &deps, &[(0, 2, 0)]);
    assert_eq!(n, 2);
    assert_eq!(num(&wb, 0, 0, 2), 42.0);
    assert_eq!(num(&wb, 0, 0, 3), 420.0);
}

#[test]
fn cross_sheet_dependency() {
    let mut wb = Workbook::new_blank();
    wb.sheets.push(sheetcore::Sheet::new("汇总"));
    set(&mut wb, 0, 0, 5.0);
    wb.sheets[1].set(0, 0, Cell { value: Value::Number(0.0), formula: Some("=Sheet1!A1*7".into()) });
    formula::recalc_all(&mut wb);
    assert_eq!(num(&wb, 1, 0, 0), 35.0);

    let deps = Deps::build(&wb);
    set(&mut wb, 0, 0, 6.0);
    let n = formula::recalc_dirty(&mut wb, &deps, &[(0, 0, 0)]);
    assert_eq!(n, 1);
    assert_eq!(num(&wb, 1, 0, 0), 42.0);
}

#[test]
fn remote_batch_uses_incremental_recalc() {
    use sheetcore::sync::{Op, Peer, ValueWire};

    let mut wb = Workbook::new_blank();
    for r in 0..20u32 {
        wb.sheets[0].set(r, 0, Cell { value: Value::Number(r as f64 + 1.0), formula: None });
        wb.sheets[0].set(r, 1, Cell { value: Value::Number(0.0), formula: Some(format!("=A{}*10", r + 1).into()) });
    }
    wb.sheets[0].set(0, 2, Cell { value: Value::Number(0.0), formula: Some("=SUM(B1:B20)".into()) });
    formula::recalc_all(&mut wb);
    let mut deps = Deps::build(&wb);
    assert_eq!(num(&wb, 0, 0, 2), 2100.0);

    // 模拟远端改了 A5 和 A6
    let mut peer = Peer::new(1);
    let mut touched = Vec::new();
    for (i, r) in [4u32, 5u32].iter().enumerate() {
        let op = Op {
            sheet: 0,
            row: *r,
            col: 0,
            value: ValueWire::Number(100.0 + i as f64),
            formula: None,
            ts: i as u64 + 1,
            site: 7,
        };
        if peer.apply(op) {
            touched.push((*r, 0u32));
        }
    }
    let keys: Vec<(usize, u32, u32)> = touched.iter().map(|(r, c)| (0usize, *r, *c)).collect();
    peer.apply_list_to_workbook(&mut wb, &keys);
    let seeds: Vec<(usize, u32, u32)> = keys.clone();
    for k in &seeds {
        deps.update_cell(&wb, *k);
    }
    let n = formula::recalc_dirty(&mut wb, &deps, &seeds);
    assert_eq!(n, 3, "应只重算 B5、B6 和合计 C1，实际 {n}");
    assert_eq!(num(&wb, 0, 4, 1), 1000.0);
    assert_eq!(num(&wb, 0, 5, 1), 1010.0);
    // B 是 A 的 10 倍：A5 5→100 多 950，A6 6→101 多 950，合计 2100+1900=4000
    assert_eq!(num(&wb, 0, 0, 2), 4000.0);
}

#[test]
fn remote_formula_change_updates_deps() {
    use sheetcore::sync::{Op, Peer, ValueWire};

    let mut wb = Workbook::new_blank();
    set(&mut wb, 0, 0, 1.0);
    set(&mut wb, 1, 0, 2.0);
    setf(&mut wb, 0, 2, "=A1");
    formula::recalc_all(&mut wb);
    let mut deps = Deps::build(&wb);
    assert_eq!(num(&wb, 0, 0, 2), 1.0);

    // 远端把 C1 的公式改成引用 A2
    let mut peer = Peer::new(1);
    let op = Op {
        sheet: 0,
        row: 0,
        col: 2,
        value: ValueWire::Number(0.0),
        formula: Some("=A2".into()),
        ts: 9,
        site: 7,
    };
    assert!(peer.apply(op));
    peer.apply_list_to_workbook(&mut wb, &[(0, 0, 2)]);
    deps.update_cell(&wb, (0, 0, 2));
    formula::recalc_dirty(&mut wb, &deps, &[(0, 0, 2)]);
    assert_eq!(num(&wb, 0, 0, 2), 2.0);

    // 之后改 A2 必须能触发 C1 重算（说明依赖已经更新）
    set(&mut wb, 1, 0, 50.0);
    let n = formula::recalc_dirty(&mut wb, &deps, &[(0, 1, 0)]);
    assert_eq!(n, 1);
    assert_eq!(num(&wb, 0, 0, 2), 50.0);
}

#[test]
fn multi_sheet_ops_do_not_collide() {
    use sheetcore::sync::{Op, Peer, ValueWire};

    let mut wb = Workbook::new_blank();
    wb.sheets.push(sheetcore::Sheet::new("第二张"));
    let mut a = Peer::new(1);
    let mut b = Peer::new(2);

    // 同样的 (row,col)，但在不同工作表上，必须各自独立
    let o1 = a.local_set(0, 3, 3, &Cell { value: Value::Number(111.0), formula: None });
    let o2 = b.local_set(1, 3, 3, &Cell { value: Value::Number(222.0), formula: None });
    assert!(a.apply(o2.clone()));
    assert!(b.apply(o1.clone()));

    assert_eq!(a.cells.len(), 2);
    assert_eq!(b.cells.len(), 2);

    a.apply_all_to_workbook(&mut wb);
    assert_eq!(num(&wb, 0, 3, 3), 111.0);
    assert_eq!(num(&wb, 1, 3, 3), 222.0);

    // 同表同格的冲突仍然按 (ts, site) 收敛
    let o3 = a.local_set(1, 9, 9, &Cell { value: Value::Number(1.0), formula: None });
    let o4 = b.local_set(1, 9, 9, &Cell { value: Value::Number(2.0), formula: None });
    a.apply(o4.clone());
    b.apply(o3.clone());
    assert_eq!(a.cells[&(1, 9, 9)].value, b.cells[&(1, 9, 9)].value);
    let _: Option<Op> = None;
}
