use super::*;
use crate::model::Value;

fn formula(wb: &mut Workbook, row: u32, col: u32, src: &str) {
    set_formula(&mut wb.sheets[0], row, col, src, FValue::Empty);
}

fn number(wb: &mut Workbook, row: u32, col: u32, value: f64) {
    wb.sheets[0].set(
        row,
        col,
        Cell {
            value: Value::Number(value),
            formula: None,
        },
    );
}

#[test]
fn deep_forward_chain_preserves_lazy_reads_and_real_cycles() {
    let mut wb = Workbook::new_blank();
    for r in 0..3_000 {
        formula(&mut wb, r, 0, &format!("=IF(TRUE,A{}+1,A1)", r + 2));
    }
    number(&mut wb, 3_000, 0, 0.0);
    assert_eq!(eval_cell(&wb, 0, 0, 0), FValue::number(3_000.0));
    formula(&mut wb, 3_000, 0, "=A3000");
    assert_eq!(eval_cell(&wb, 0, 0, 0), FValue::Error(ErrKind::Circular));
    assert!(take_cycle_cells().contains(&(0, 2999, 0)));
}

#[test]
fn suspended_range_and_lookup_do_not_cache_incomplete_values() {
    let mut wb = Workbook::new_blank();
    for (col, src) in [
        "=B1",
        "=C1",
        "=D1",
        "=SUM(E1:E3)+VLOOKUP(2,E1:F3,2,0)+SUMIFS(F1:F3,E1:E3,2)",
    ]
    .iter()
    .enumerate()
    {
        formula(&mut wb, 0, col as u32, src);
    }
    for r in 0..3 {
        formula(&mut wb, r, 4, &format!("={}", r + 1));
        formula(&mut wb, r, 5, &format!("={}", (r + 1) * 10));
    }
    assert_eq!(eval_cell(&wb, 0, 0, 0), FValue::number(46.0));
}

#[test]
fn shared_prefix_reads_each_cell_once_in_any_order_and_preserves_errors() {
    let mut wb = Workbook::new_blank();
    for r in 0..512 {
        number(&mut wb, r, 0, 1.0);
    }
    wb.sheets[0].set(
        511,
        0,
        Cell {
            value: Value::Error(ErrKind::Na),
            formula: None,
        },
    );
    let mut calc = Calc::new(&wb);
    for r in [511, 200, 400, 512, 4] {
        let result = calc.eval_node(&parse(&format!("SUM($A$1:A{r})")).unwrap(), 0);
        let expected = if r == 512 {
            FValue::Error(ErrKind::Na)
        } else {
            FValue::number(r as f64)
        };
        assert_eq!(result, expected);
    }
    assert_eq!(calc.range_reads, 512);
}

#[test]
fn conditional_aggregates_reuse_only_matching_ranges_and_criteria() {
    let mut wb = Workbook::new_blank();
    for row in 0..5 {
        number(&mut wb, row, 0, (row + 1) as f64);
        number(&mut wb, row, 1, if row % 2 == 0 { 10.0 } else { 20.0 });
    }
    let mut calc = Calc::new(&wb);
    for (src, expected) in [
        ("SUMIFS(B1:B5,A1:A5,\">2\",B1:B5,10)", 20.0),
        ("SUMIFS(B1:B5,A1:A5,\">2\",B1:B5,20)", 20.0),
        ("COUNTIFS(B1:B5,10,A1:A5,\">2\")", 2.0),
        ("AVERAGEIF(B1:B5,10,A1:A5)", 3.0),
        ("AVERAGEIFS(A1:A5,B1:B5,10)", 3.0),
        ("MAXIFS(A1:A5,B1:B5,10)", 5.0),
        ("MINIFS(A1:A5,B1:B5,10)", 1.0),
        ("SUMIFS(A1:A5,B1:B5,10)", 9.0),
    ] {
        let node = parse(src).unwrap();
        assert_eq!(calc.eval_node(&node, 0), FValue::number(expected));
        assert_eq!(calc.eval_node(&node, 0), FValue::number(expected));
    }
    assert_eq!(calc.agg_hits, 8);
    assert_eq!(calc.agg_miss, 8);
}

#[test]
fn approximate_lookup_uses_last_duplicate_and_match_default_depends_on_function() {
    let mut wb = Workbook::new_blank();
    for (r, v) in [1.0, 3.0, 3.0, 7.0].into_iter().enumerate() {
        number(&mut wb, r as u32, 0, v);
        number(&mut wb, r as u32, 1, r as f64);
    }
    for (f, expected) in [
        ("MATCH(5,A1:A4)", 3.0),
        ("MATCH(3,A1:A4,1)", 3.0),
        ("MATCH(3,A1:A4,0)", 2.0),
        ("VLOOKUP(3,A1:B4,2)", 2.0),
        ("MATCH(4,{9,6,3},-1)", 2.0),
    ] {
        assert_eq!(eval_formula(&wb, 0, f), FValue::number(expected), "{f}");
    }
    assert_eq!(
        eval_formula(&wb, 0, "XMATCH(5,A1:A4)"),
        FValue::Error(ErrKind::Na)
    );
    assert_eq!(
        eval_formula(&wb, 0, "XMATCH(5,{1,3,7})"),
        FValue::Error(ErrKind::Na)
    );
}

#[test]
fn relative_compilation_matches_reference_parser_after_copy_across_rows_and_columns() {
    for (row, col, src) in [
        (0, 0, "SUM($A$1:B2)+B$7+$F2"),
        (3, 2, "SUM($A$1:D5)+D$7+$F5"),
        (9, 1, "SUM($A$1:C11)+C$7+$F11"),
        (5, 0, "1e3+A6"),
        (6, 0, "1e4+A7"),
        (0, 0, "SUM(1:1)+A1"),
        (2, 1, "SUM(3:3)+B3"),
        (0, 1, "Sheet1!A1+LOG10(A1)"),
    ] {
        assert_eq!(
            *compile_at(src, row, col).unwrap(),
            parse(&crate::normalize_formula(src)).unwrap()
        );
    }
}

#[test]
fn defined_name_cycles_are_reported_with_local_and_global_scopes() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Data".to_string();
    wb.names = vec![
        ("LeftAlias".into(), None, "=Data!RightAlias".into()),
        ("RightAlias".into(), Some(0), "=LeftAlias".into()),
    ];
    assert_eq!(
        eval_formula(&wb, 0, "LeftAlias"),
        FValue::Error(ErrKind::Name)
    );
    assert_eq!(
        eval_formula(&wb, 0, "Data!RightAlias"),
        FValue::Error(ErrKind::Name)
    );
}

#[test]
fn deep_range_of_independent_formulas_is_materialized_in_linear_reads() {
    let mut wb = Workbook::new_blank();
    for (col, src) in ["=B1", "=C1", "=SUM(D1:D1000)"].iter().enumerate() {
        formula(&mut wb, 0, col as u32, src);
    }
    for r in 0..1000 {
        formula(&mut wb, r, 3, "=2");
    }
    let mut calc = Calc::new(&wb);
    assert_eq!(calc.cell_value(0, 0, 0), FValue::number(2000.0));
    assert!(
        calc.range_reads <= 1001,
        "unexpected range replays: {}",
        calc.range_reads
    );
}

#[test]
fn range_cache_hit_promotes_the_entry() {
    let wb = Workbook::new_blank();
    let mut calc = Calc::new(&wb);
    let keys = [(0, 0, 0, 2, 0), (0, 1, 0, 2, 0)];
    for key in keys {
        calc.cache_put(key, Default::default());
    }
    calc.cache_get(keys[0]).unwrap();
    assert_eq!(calc.cache_order.first().unwrap().1, keys[1]);
}

#[test]
fn prefix_cache_handles_horizontal_and_unanchored_running_sums() {
    let mut wb = Workbook::new_blank();
    for c in 0..200 {
        number(&mut wb, 0, c, 1.0);
    }
    let mut calc = Calc::new(&wb);
    for (src, expected) in [
        ("SUM(A1:C1)", 3.0),
        ("SUM(A1:Z1)", 26.0),
        ("SUM(A1:B1)", 2.0),
    ] {
        assert_eq!(
            calc.eval_node(&parse(src).unwrap(), 0),
            FValue::number(expected)
        );
    }
    assert_eq!(calc.range_reads, 29);
}

#[test]
fn clearing_a_spill_obstruction_recalculates_evaluated_and_loaded_anchors() {
    for cached in [false, true] {
        let mut wb = Workbook::new_blank();
        for r in 0..1210 {
            number(&mut wb, r, 2, 1.0);
        }
        set_formula(
            &mut wb.sheets[0],
            0,
            0,
            "=SEQUENCE(3,1)",
            FValue::Error(ErrKind::Spill),
        );
        number(&mut wb, 1, 0, 9.0);
        // Build before evaluating to exercise live writeback as well as file caches.
        if !cached {
            wb.sheets[0].get_mut(0, 0).unwrap().value = Value::Empty;
        }
        let deps = deps::Deps::build(&wb);
        if !cached {
            recalc_all_force(&mut wb);
            assert_eq!(
                wb.sheets[0].spill_shapes.get(&CellRef::new(0, 0)),
                Some(&(0, 0))
            );
        }
        wb.sheets[0].remove(1, 0);
        assert_eq!(
            recalc_interactive_plan(&mut wb, &deps, &[(0, 1, 0)], true),
            InteractiveRecalc::BackgroundFull
        );
        recalc_all_force(&mut wb);
        assert_eq!(wb.sheets[0].value(2, 0), Value::Number(3.0));
    }
}

#[test]
fn lookup_indexes_keep_first_row_and_first_column_distinct() {
    let mut wb = Workbook::new_blank();
    for (r, c, n) in [
        (0, 0, 1.0),
        (0, 1, 9.0),
        (0, 2, 3.0),
        (1, 0, 3.0),
        (1, 1, 4.0),
        (1, 2, 8.0),
        (2, 0, 9.0),
        (2, 1, 5.0),
    ] {
        number(&mut wb, r, c, n);
    }
    let mut calc = Calc::new(&wb);
    for (f, expected) in [
        ("VLOOKUP(3,A1:C3,2,FALSE)", 4.0),
        ("HLOOKUP(3,A1:C3,2,FALSE)", 8.0),
        ("VLOOKUP(9,A1:C3,2,FALSE)", 5.0),
        ("HLOOKUP(9,A1:C3,2,FALSE)", 4.0),
    ] {
        assert_eq!(
            calc.eval_node(&parse(f).unwrap(), 0),
            FValue::number(expected)
        );
    }
}

#[test]
fn approximate_lookups_skip_holes_and_use_only_keys_of_the_lookup_type() {
    let empty = Workbook::new_blank();
    for (key, result) in [(3, 2.0), (2, 3.0), (4, 1.0)] {
        let f = format!("MATCH({key},{{5,3,3,1}},-1)");
        assert_eq!(eval_formula(&empty, 0, &f), FValue::number(result));
    }

    let mut wb = Workbook::new_blank();
    for (r, value) in [
        (0, Value::Number(1.0)),
        (1, Value::Number(3.0)),
        (3, Value::Number(5.0)),
        (4, Value::Text("a".into())),
        (5, Value::Text("c".into())),
        (6, Value::Bool(false)),
        (7, Value::Bool(true)),
    ] {
        wb.sheets[0].set(
            r,
            0,
            Cell {
                value: value.clone(),
                formula: None,
            },
        );
        wb.sheets[0].set(
            0,
            r + 3,
            Cell {
                value,
                formula: None,
            },
        );
    }
    for r in 0..10 {
        number(&mut wb, r, 1, (r + 1) as f64);
        number(&mut wb, 1, r + 3, (r + 1) as f64);
    }
    for (key, result) in [
        ("2", 1.0),
        ("3", 2.0),
        ("6", 4.0),
        ("\"b\"", 5.0),
        ("FALSE", 7.0),
        ("TRUE", 8.0),
    ] {
        for f in [
            format!("VLOOKUP({key},A1:B10,2)"),
            format!("HLOOKUP({key},D1:M2,2)"),
            format!("MATCH({key},A1:A10)"),
        ] {
            assert_eq!(eval_formula(&wb, 0, &f), FValue::number(result), "{f}");
        }
    }
    assert_eq!(
        eval_formula(&wb, 0, "MATCH(X1,A1:A10)"),
        FValue::Error(ErrKind::Na)
    );
    number(&mut wb, 0, 0, 0.0);
    assert_eq!(
        eval_formula(&wb, 0, "MATCH(X1,A1:A10)"),
        FValue::number(1.0)
    );
    for (r, value) in [(0, 5.0), (2, 3.0), (3, 1.0)] {
        number(&mut wb, r, 14, value);
    }
    assert_eq!(
        eval_formula(&wb, 0, "MATCH(2,O1:O10,-1)"),
        FValue::number(3.0)
    );
}
