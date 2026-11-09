use sheetcore::{Cell, CellRef, Sheet, Value, Workbook};
use sheetcore::formula::{self, deps::Deps};

fn set(wb: &mut Workbook, r: u32, c: u32, value: f64) {
    wb.sheets[0].set(r, c, Cell { value: Value::Number(value), formula: None });
}

fn setf(wb: &mut Workbook, r: u32, c: u32, formula: &str) {
    wb.sheets[0].set(r, c, Cell { value: Value::Text(formula.into()), formula: Some(formula.into()) });
}

fn cached(wb: &Workbook, r: u32, c: u32) -> f64 {
    match wb.sheets[0].value(r, c) {
        Value::Number(n) => n,
        value => panic!("({}, {}) cached {:?}", r, c, value),
    }
}

fn assert_full_matches(wb: &Workbook) {
    let mut full = wb.clone();
    formula::recalc_all(&mut full);
    for (si, sheet) in wb.sheets.iter().enumerate() {
        let keys: std::collections::HashSet<_> = sheet.iter().map(|(k, _)| k)
            .chain(full.sheets[si].iter().map(|(k, _)| k)).collect();
        for key in keys {
            assert_eq!(sheet.value(key.row, key.col), full.sheets[si].value(key.row, key.col),
                "sheet {si}, {key:?}: incremental != full");
        }
        assert_eq!(sheet.spilled, full.sheets[si].spilled);
    }
}

#[test]
fn typed_and_pasted_formulas_update_their_own_cache_and_dependents() {
    let mut wb = Workbook::new_blank();
    setf(&mut wb, 0, 0, "=1+2");
    let mut deps = Deps::build(&wb);
    assert_eq!(formula::recalc_dirty(&mut wb, &deps, &[(0, 0, 0)]), 1);
    assert_eq!(cached(&wb, 0, 0), 3.0);
    set(&mut wb, 1, 0, 10.0);
    wb.sheets[0].set(1, 1, Cell { value: Value::Number(2.0), formula: Some("=A2*2".into()) });
    setf(&mut wb, 1, 2, "=B2+1");
    deps.update_cell(&wb, (0, 1, 1));
    deps.update_cell(&wb, (0, 1, 2));
    formula::recalc_dirty(&mut wb, &deps, &[(0, 1, 1), (0, 1, 2)]);
    assert_eq!(cached(&wb, 1, 1), 20.0);
    assert_eq!(cached(&wb, 1, 2), 21.0);
    assert_full_matches(&wb);
}

#[test]
fn whole_columns_names_and_dynamic_references_follow_edits() {
    for f in ["=SUM(A:A)", "=INDIRECT(\"A1\")", "=OFFSET(A2,-1,0)", "=Rate"] {
        let mut wb = Workbook::new_blank();
        wb.names.push(("Rate".into(), None, "Sheet1!$A$1".into()));
        set(&mut wb, 0, 0, 1.0);
        setf(&mut wb, 0, 1, f);
        formula::recalc_all(&mut wb);
        let deps = Deps::build(&wb);
        set(&mut wb, 0, 0, 10.0);
        formula::recalc_dirty(&mut wb, &deps, &[(0, 0, 0)]);
        assert_eq!(cached(&wb, 0, 1), 10.0, "{f}");
        assert_full_matches(&wb);
    }
}

#[test]
fn structured_references_share_evaluation_normalization() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].tables.push(sheetcore::table::Table {
        name: "Sales".into(), display: "Sales".into(), range: (0, 0, 2, 0),
        columns: vec!["Amount".into()], header_row: true, totals_row: false,
    });
    set(&mut wb, 1, 0, 1.0);
    set(&mut wb, 2, 0, 2.0);
    setf(&mut wb, 0, 1, "=SUM(Sales[Amount])");
    formula::recalc_all(&mut wb);
    let deps = Deps::build(&wb);
    set(&mut wb, 2, 0, 20.0);
    formula::recalc_dirty(&mut wb, &deps, &[(0, 2, 0)]);
    assert_eq!(cached(&wb, 0, 1), 21.0);
    assert_full_matches(&wb);
}

#[test]
fn wide_ranges_and_long_dependency_chains_are_not_truncated() {
    let mut wb = Workbook::new_blank();
    set(&mut wb, 0, 599, 1.0);
    setf(&mut wb, 1, 0, "=SUM(A1:WB1)");
    formula::recalc_all(&mut wb);
    let deps = Deps::build(&wb);
    set(&mut wb, 0, 599, 10.0);
    assert_eq!(formula::recalc_dirty(&mut wb, &deps, &[(0, 0, 599)]), 1);
    assert_eq!(cached(&wb, 1, 0), 10.0);
    assert_full_matches(&wb);

    let mut wb = Workbook::new_blank();
    set(&mut wb, 0, 0, 1.0);
    for r in 1..140 { setf(&mut wb, r, 0, &format!("=A{}+1", r)); }
    formula::recalc_all(&mut wb);
    let deps = Deps::build(&wb);
    set(&mut wb, 0, 0, 10.0);
    assert_eq!(formula::recalc_dirty(&mut wb, &deps, &[(0, 0, 0)]), 139);
    assert_eq!(cached(&wb, 139, 0), 149.0);
    assert_full_matches(&wb);
}

#[test]
fn replacing_cross_sheet_reference_removes_the_old_index() {
    let mut wb = Workbook::new_blank();
    wb.sheets.push(Sheet::new("Other"));
    wb.sheets[1].set(0, 0, Cell { value: Value::Number(2.0), formula: None });
    setf(&mut wb, 0, 0, "=Other!A1");
    let mut deps = Deps::build(&wb);
    setf(&mut wb, 0, 0, "=Other!B1");
    deps.update_cell(&wb, (0, 0, 0));
    assert!(deps.closure(&[(1, 0, 0)]).is_empty());
    assert_eq!(deps.closure(&[(1, 0, 1)]).len(), 1);
    assert_eq!(deps.rect_count, 1);
}

#[test]
fn spill_dependents_and_multiple_array_stages_stabilize() {
    let mut wb = Workbook::new_blank();
    setf(&mut wb, 0, 0, "=SEQUENCE(2)");
    setf(&mut wb, 0, 1, "=A2+10");
    setf(&mut wb, 0, 2, "=SEQUENCE(2,1,A2+10)");
    setf(&mut wb, 0, 3, "=SEQUENCE(2,1,C2+10)");
    setf(&mut wb, 0, 4, "=D2+10");
    formula::recalc_all(&mut wb);
    assert_eq!(cached(&wb, 0, 1), 12.0);
    assert_eq!(cached(&wb, 1, 2), 13.0);
    assert_eq!(cached(&wb, 1, 3), 24.0);
    assert_eq!(cached(&wb, 0, 4), 34.0);
    assert_full_matches(&wb);
}

#[test]
fn incremental_array_creation_shrink_and_replacement_clear_old_spills() {
    let mut wb = Workbook::new_blank();
    set(&mut wb, 0, 2, 3.0);
    setf(&mut wb, 0, 0, "=SEQUENCE(C1)");
    setf(&mut wb, 0, 1, "=SUM(A1:A5)");
    let mut deps = Deps::build(&wb);
    formula::recalc_dirty(&mut wb, &deps, &[(0, 0, 0), (0, 0, 1)]);
    assert_eq!(cached(&wb, 0, 1), 6.0);
    set(&mut wb, 0, 2, 2.0);
    formula::recalc_dirty(&mut wb, &deps, &[(0, 0, 2)]);
    assert_eq!(cached(&wb, 0, 1), 3.0);
    assert!(wb.sheets[0].get(2, 0).is_none());
    assert!(wb.sheets[0].cleared.contains(&CellRef::new(2, 0)));
    assert_full_matches(&wb);
    setf(&mut wb, 0, 0, "=7");
    deps.update_cell(&wb, (0, 0, 0));
    formula::recalc_dirty(&mut wb, &deps, &[(0, 0, 0)]);
    assert_eq!(cached(&wb, 0, 1), 7.0);
    assert!(wb.sheets[0].spilled.is_empty());
    assert!(wb.sheets[0].get(1, 0).is_none());
    assert_full_matches(&wb);
    setf(&mut wb, 0, 0, "=SEQUENCE(2)");
    deps.update_cell(&wb, (0, 0, 0));
    formula::recalc_dirty(&mut wb, &deps, &[(0, 0, 0)]);
    wb.sheets[0].set(0, 0, Cell::default());
    deps.update_cell(&wb, (0, 0, 0));
    formula::recalc_dirty(&mut wb, &deps, &[(0, 0, 0)]);
    assert_eq!(cached(&wb, 0, 1), 0.0);
    assert!(wb.sheets[0].spilled.is_empty());
    assert!(wb.sheets[0].get(1, 0).is_none());
    assert_full_matches(&wb);
}

#[test]
fn replacing_a_spilled_cell_keeps_the_user_input() {
    let mut wb = Workbook::new_blank();
    setf(&mut wb, 0, 0, "=SEQUENCE(3)");
    formula::recalc_all(&mut wb);
    let deps = Deps::build(&wb);
    set(&mut wb, 1, 0, 99.0);
    formula::recalc_dirty(&mut wb, &deps, &[(0, 1, 0)]);
    assert_eq!(cached(&wb, 1, 0), 99.0);
    assert_eq!(wb.sheets[0].value(0, 0).display(), "#SPILL!");
    assert!(wb.sheets[0].get(2, 0).is_none());
    assert_full_matches(&wb);
}

#[test]
fn blocked_array_retries_when_obstruction_is_cleared() {
    let mut wb = Workbook::new_blank();
    setf(&mut wb, 0, 0, "=SEQUENCE(2)");
    set(&mut wb, 1, 0, 99.0);
    setf(&mut wb, 0, 1, "=A1+10");
    setf(&mut wb, 0, 2, "=SUM(A1:A2)");
    let deps = Deps::build(&wb);
    formula::recalc_all(&mut wb);
    assert_eq!(wb.sheets[0].value(0, 0).display(), "#SPILL!");
    assert_eq!(wb.sheets[0].value(0, 1).display(), "#SPILL!");
    assert_eq!(wb.sheets[0].value(0, 2).display(), "#SPILL!");
    wb.sheets[0].set(1, 0, Cell::default());
    formula::recalc_dirty(&mut wb, &deps, &[(0, 1, 0)]);
    assert_eq!(cached(&wb, 1, 0), 2.0);
    assert_eq!(cached(&wb, 0, 1), 11.0);
    assert_eq!(cached(&wb, 0, 2), 3.0);
    assert_full_matches(&wb);
}

#[test]
fn random_arrays_reuse_samples_during_spill_stabilization() {
    let mut wb = Workbook::new_blank();
    setf(&mut wb, 0, 0, "=RANDARRAY(2)");
    setf(&mut wb, 0, 1, "=A2+10");
    formula::recalc_all(&mut wb);
    assert_eq!(cached(&wb, 0, 1), cached(&wb, 1, 0) + 10.0);
    assert!(cached(&wb, 1, 0) >= 0.0 && cached(&wb, 1, 0) <= 1.0);
}

#[test]
fn divergent_spill_cycle_returns_an_error_instead_of_partial_numbers() {
    let mut wb = Workbook::new_blank();
    setf(&mut wb, 0, 0, "=SEQUENCE(2,1,A2+1)");
    setf(&mut wb, 0, 1, "=A2+10");
    setf(&mut wb, 0, 2, "=1+2");
    setf(&mut wb, 0, 3, "=SEQUENCE(2,1,7)");
    setf(&mut wb, 0, 4, "=RAND()");
    formula::recalc_all(&mut wb);
    assert_eq!(wb.sheets[0].value(0, 0).display(), "#REF!");
    assert_eq!(wb.sheets[0].value(0, 1).display(), "#REF!");
    assert_eq!(cached(&wb, 0, 2), 3.0);
    assert_eq!(cached(&wb, 0, 3), 7.0);
    assert_eq!(cached(&wb, 1, 3), 8.0);
    assert!((0.0..=1.0).contains(&cached(&wb, 0, 4)));
    assert!(!wb.sheets[0].spilled.contains(&CellRef::new(1, 0)));
}
