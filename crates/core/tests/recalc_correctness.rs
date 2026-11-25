use sheetcore::{Cell, CellRef, Sheet, Value, Workbook};
use sheetcore::formula::{self, deps::Deps};

#[test]
fn clipped_large_range_does_not_reverse_into_used_rows_or_columns() {
    let mut wb = Workbook::new_blank();
    set(&mut wb, 0, 0, 100.0);
    for expression in ["=SUM(A100:A300100)", "=SUM(Z1:ZZ10000)"] {
        assert_eq!(formula::eval_formula(&wb, 0, expression), formula::value::FValue::Number(0.0));
    }
}

#[test]
fn sumif_uses_criteria_dimensions_for_amounts_and_dependencies() {
    let mut wb = Workbook::new_blank();
    wb.sheets.push(Sheet::new("Amounts"));
    for r in 0..2 {
        for c in 0..2 {
            set(&mut wb, r, c, 1.0);
            wb.sheets[1].set(r, c, Cell { value: Value::Number(10.0), formula: None });
        }
    }
    setf(&mut wb, 0, 3, "=SUMIF(A1:B2,1,Amounts!A1)");
    formula::recalc_all(&mut wb);
    assert_eq!(cached(&wb, 0, 3), 40.0);
    let deps = Deps::build(&wb);
    wb.sheets[1].set(1, 1, Cell { value: Value::Number(20.0), formula: None });
    formula::recalc_dirty(&mut wb, &deps, &[(1, 1, 1)]);
    assert_eq!(cached(&wb, 0, 3), 50.0);
    wb.sheets[1].set(1_048_575, 0, Cell { value: Value::Number(8.0), formula: None });
    assert_eq!(formula::eval_formula(&wb, 0, "=SUMIF(A1:A2,1,Amounts!A1048576)"),
               formula::FValue::Number(8.0));
}

#[test]
fn range_averages_and_extrema_propagate_cell_errors() {
    let mut wb = Workbook::new_blank();
    set(&mut wb, 0, 0, 100.0);
    setf(&mut wb, 1, 0, "=1/0");
    for name in ["AVERAGE", "MAX", "MIN"] {
        assert_eq!(formula::eval_formula(&wb, 0, &format!("={name}(A1:A2)")),
                   formula::value::FValue::Error(formula::ErrKind::Div0));
    }
}

#[test]
fn subtotals_use_visibility_of_the_source_sheet() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Data".into();
    wb.sheets.push(Sheet::new("Report"));
    for (r, n) in [100.0, 200.0, 300.0].into_iter().enumerate() { set(&mut wb, r as u32, 0, n); }
    wb.sheets[0].hidden_rows.insert(1);
    wb.sheets[0].hidden_rows_user.insert(2);
    for (expression, expected) in [("=SUBTOTAL(9,Data!A1:A3)", 400.0),
        ("=SUBTOTAL(109,Data!A1:A3)", 100.0), ("=SUBTOTAL(9,Data!A2)", 0.0)] {
        assert_eq!(formula::eval_formula(&wb, 1, expression), formula::value::FValue::Number(expected));
    }
}

#[test]
fn yearfrac_uses_the_selected_day_count_basis() {
    let wb = Workbook::new_blank();
    for (basis, expected) in [(0, 1.0), (1, 1.0), (2, 366.0 / 360.0), (3, 366.0 / 365.0), (4, 1.0)] {
        let expr = format!("=YEARFRAC(DATE(2024,1,1),DATE(2025,1,1),{basis})");
        let actual = formula::eval_formula(&wb, 0, &expr).to_number().unwrap();
        assert!((actual - expected).abs() < 1e-12, "{expr} = {actual}");
    }
    for (basis, expected_days) in [(0, 31.0), (4, 32.0)] {
        let expr = format!("=YEARFRAC(DATE(2023,2,28),DATE(2023,3,31),{basis})");
        let actual = formula::eval_formula(&wb, 0, &expr).to_number().unwrap();
        assert!((actual * 360.0 - expected_days).abs() < 1e-10);
    }
    assert_eq!(formula::eval_formula(&wb, 0, "=YEARFRAC(1,2,5)"),
               formula::value::FValue::Error(formula::ErrKind::Num));
    // Excel 1900 serial 60 affects elapsed days, while 1900 has a 365-day basis.
    let actual = formula::eval_formula(&wb, 0, "=YEARFRAC(59,61,1)").to_number().unwrap();
    assert!((actual - 2.0 / 365.0).abs() < 1e-12);
}

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
                column_ids: Vec::new(),
        name: "Sales".into(), display: "Sales".into(), range: (0, 0, 2, 0),
        columns: vec!["Amount".into()], header_row: true, totals_row: false,
        ..Default::default()
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
