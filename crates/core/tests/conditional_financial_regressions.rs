use sheetcore::formula::{self, FValue, deps::Deps};
use sheetcore::{Cell, Sheet, Value, Workbook};

fn number(wb: &mut Workbook, sheet: usize, row: u32, col: u32, n: f64) {
    wb.sheets[sheet].set(row, col, Cell { value: Value::Number(n), formula: None });
}

fn check_numbers(wb: &mut Workbook, sheet: usize, cases: &[(&str, f64)]) {
    for (row, &(expr, _)) in cases.iter().enumerate() {
        formula::set_formula(&mut wb.sheets[sheet], row as u32, 3, expr, FValue::Empty);
    }
    formula::recalc_all_force(wb);
    for (row, &(expr, expected)) in cases.iter().enumerate() {
        let cell = wb.sheets[sheet].get(row as u32, 3).unwrap();
        let Value::Number(value) = cell.value else { panic!("{expr}: {:?}", cell.value) };
        assert!((value - expected).abs() < 1e-10 * expected.abs().max(1.0),
                "{expr}: expected {expected}, got {value}");
    }
}

#[test]
fn conditional_totals_cover_trailing_blanks_without_expanding_a_sparse_grid() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Flags".into();
    wb.sheets.push(Sheet::new("Amounts"));
    wb.sheets.push(Sheet::new("Report"));
    number(&mut wb, 0, 0, 0, 1.0);
    number(&mut wb, 1, 0, 0, 10.0);
    number(&mut wb, 1, 1, 0, 900.0);
    number(&mut wb, 1, 1_048_575, 0, -100.0);
    wb.names.push(("FlagRange".into(), None, "Flags!$A$1:$A$200001".into()));
    formula::set_formula(&mut wb.sheets[0], 3, 0, "=\"\"", FValue::Empty);
    check_numbers(&mut wb, 2, &[
        ("=COUNTBLANK(Flags!A1:A200000)", 199_999.0),
        ("=COUNTBLANK(Flags!A1:A200001)", 200_000.0),
        ("=COUNTBLANK(Flags!A:A)", 1_048_575.0),
        ("=COUNTBLANK(OFFSET(Flags!A1,0,0,1048576,1))", 1_048_575.0),
        ("=COUNTIF(Flags!A:A,\"\")", 1_048_575.0),
        ("=COUNTIF(Flags!A:A,\"<>1\")", 1_048_575.0),
        ("=SUMIF(Flags!A:A,\"\",Amounts!A:A)", 800.0),
        ("=SUMIF(FlagRange,\"\",Amounts!A:A)", 900.0),
        ("=AVERAGEIF(Flags!A:A,\"\",Amounts!A:A)", 400.0),
        ("=COUNTBLANK(Flags!A1:XFD1)", 16_383.0),
        ("=SUMIF(Flags!A1:XFD1048576,\"\",Amounts!A1)", 800.0),
        ("=AVERAGEIF(Flags!A1:XFD1048576,\"\",Amounts!A1)", 400.0),
    ]);
    let (v, reads) = formula::eval_formula_reads(&wb, 2, "=COUNTBLANK(Flags!A1:XFD1048576)");
    assert_eq!(v, FValue::Number(17_179_869_183.0));
    assert!(reads < 20, "read {reads} cells in a sparse 17-billion-cell rectangle");
}

#[test]
fn averageif_extends_anchor_in_two_dimensions_and_tracks_the_expanded_dependencies() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Data".into();
    wb.sheets.push(Sheet::new("Report"));
    for r in 0..2 {
        for c in 0..2 { number(&mut wb, 0, r, c, 1.0); }
    }
    for (r, c, v) in [(2, 1, 10.0), (2, 2, 20.0), (3, 1, 30.0), (3, 2, 40.0)] {
        number(&mut wb, 0, r, c, v);
    }
    number(&mut wb, 0, 1_048_575, 0, 16.0);
    check_numbers(&mut wb, 1, &[
        ("=AVERAGEIF(Data!A1:B2,1,Data!B3)", 25.0),
        ("=AVERAGEIF(Data!A1:B2,1,Data!B3:D6)", 25.0),
        ("=AVERAGEIF(Data!A1:B2,1,Data!A1048576)", 16.0),
        ("=AVERAGEIF(OFFSET(Data!A1,0,0,2,2),1,OFFSET(Data!A1,2,1))", 25.0),
    ]);
    let deps = Deps::build(&wb);
    number(&mut wb, 0, 3, 2, 60.0);
    formula::recalc_dirty(&mut wb, &deps, &[(0, 3, 2)]);
    for r in [0, 1, 3] { assert_eq!(wb.sheets[1].get(r, 3).unwrap().value, Value::Number(30.0)); }
}

#[test]
fn cumulative_payments_match_individual_periods_for_both_payment_timings() {
    let mut wb = Workbook::new_blank();
    check_numbers(&mut wb, 0, &[
        ("=CUMPRINC(0.1,1,1000,1,1,0)", -1000.0),
        ("=CUMIPMT(0.1,1,1000,1,1,0)", -100.0),
        ("=CUMPRINC(0.1,1,1000,1,1,1)", -1000.0),
        ("=CUMIPMT(0.1,1,1000,1,1,1)", 0.0),
        ("=CUMPRINC(0.1,3,1000,1.2,2.9,0)", -332.326283987915),
        ("=CUMIPMT(0.1,5,1000,2,3,1)-IPMT(0.1,2,5,1000,0,1)-IPMT(0.1,3,5,1000,0,1)", 0.0),
        ("=CUMPRINC(0.1,5,1000,2,3,1)-PPMT(0.1,2,5,1000,0,1)-PPMT(0.1,3,5,1000,0,1)", 0.0),
    ]);
    for expr in ["=CUMIPMT(0.1,1,1000,1,2,0)", "=CUMPRINC(0.1,2,1000,1,2,0.9)",
                 "=CUMIPMT(0.1,2,1000,0.5,1,1)"] {
        assert_eq!(formula::eval_formula(&wb, 0, expr), FValue::Error(formula::ErrKind::Num), "{expr}");
    }
}

#[test]
fn odd_and_math_rounding_handle_sign_step_and_mode() {
    let mut wb = Workbook::new_blank();
    check_numbers(&mut wb, 0, &[
        ("=ODD(0)", 1.0), ("=ODD(2)", 3.0), ("=ODD(-2)", -3.0),
        ("=ODD(3)", 3.0), ("=ODD(-3.01)", -5.0),
        ("=CEILING.MATH(-2.3,1,1)", -3.0), ("=FLOOR.MATH(-2.3,1,1)", -2.0),
        ("=CEILING.MATH(-2.3,-1,0)", -2.0), ("=FLOOR.MATH(-2.3,-1,0)", -3.0),
        ("=CEILING.MATH(2.3,-1,1)", 3.0), ("=FLOOR.MATH(2.3,-1,1)", 2.0),
        ("=FLOOR.MATH(-2.3,0)", 0.0), ("=CEILING.MATH(0.3,0.1)", 0.3),
        ("=FLOOR.MATH(0.3,0.1)", 0.3),
    ]);
}
