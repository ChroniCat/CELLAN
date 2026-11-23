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
fn ordered_range_criteria_match_fresh_scans_when_mixed_with_fractional_sums() {
    let mut wb = Workbook::new_blank();
    for row in 0..640 {
        number(&mut wb, row, 0, (row % 71) as f64 - 30.0);
        number(&mut wb, row, 1, (row % 3) as f64 - 1.0);
        number(&mut wb, row, 2, (row % 7) as f64 / 10.0);
    }
    let mut warm = Calc::new(&wb);
    for condition in [">7", "<4", "<>2", ">=19", "<=-12"] {
        for query in [
            format!("COUNTIFS(A1:A640,\"{condition}\")"),
            format!("SUMIFS(B1:B640,A1:A640,\"{condition}\")"),
            format!("SUMIFS(C1:C640,A1:A640,\"{condition}\")"),
        ] {
            let node = parse(&query).unwrap();
            assert_eq!(warm.eval_node(&node, 0), Calc::new(&wb).eval_node(&node, 0), "{query}");
        }
    }
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
fn grouped_conditional_aggregates_match_the_scan_across_distinct_keys() {
    let mut wb = Workbook::new_blank();
    for r in 0..600 {
        number(&mut wb, r, 0, (r % 7) as f64);
        wb.sheets[0].set(r, 1, Cell {
            value: Value::Text((if r % 2 == 0 { "March" } else { "APRIL" }).into()),
            formula: None,
        });
        number(&mut wb, r, 2, r as f64);
    }
    let mut calc = Calc::new(&wb);
    for k in 0..7 {
        for name in ["SUMIFS", "COUNTIFS"] {
            let amounts = if name == "SUMIFS" { "C1:C600," } else { "" };
            let src = format!("{name}({amounts}A1:A600,{k},B1:B600,\"mArCh\")");
            let Node::Func(_, nodes) = parse(&src).unwrap() else { unreachable!() };
            let slow = calc.call_generic(name, &nodes, 0);
            assert_eq!(calc.eval_node(&parse(&src).unwrap(), 0), slow, "{src}");
        }
    }
    assert!(calc.grouped_criteria.iter().all(|g| g.built_ranges() == 1));
    drop(calc);
    // Fractional numeric data within epsilon of 1 must retain scan equality semantics.
    number(&mut wb, 0, 0, 1.0 + f64::EPSILON);
    let mut calc = Calc::new(&wb);
    for k in [0, 1, 2] {
        let src = format!("COUNTIFS(A1:A600,{k})");
        let Node::Func(name, nodes) = parse(&src).unwrap() else { unreachable!() };
        let slow = calc.call_generic(&name, &nodes, 0);
        assert_eq!(calc.eval_node(&parse(&src).unwrap(), 0), slow);
    }
    assert_eq!(calc.grouped_criteria[1].built_ranges(), 0);
}

#[test]
fn alternating_criteria_groups_survive_other_ranges_and_remain_bounded() {
    let mut wb = Workbook::new_blank();
    for r in 0..600 {
        for col in 0..6 { number(&mut wb, r, col, ((r + col) % 19) as f64); }
    }
    let mut calc = Calc::new(&wb);
    for (columns, keys) in [(&["A","B","C"][..], 0..4), (&["A","B","C","D","E"][..], 4..9)] {
        for key in keys {
            for col in columns {
                for (name, amount) in [("SUMIFS","F1:F600,"), ("COUNTIFS","")] {
                    let src = format!("{name}({amount}{col}1:{col}600,{key})");
                    let node = parse(&src).unwrap();
                    let Node::Func(_, nodes) = &node else { unreachable!() };
                    let slow = calc.call_generic(name, nodes, 0);
                    assert_eq!(calc.eval_node(&node, 0), slow, "{src}");
                }
            }
        }
        if columns.len() == 3 {
            assert!(calc.grouped_criteria.iter().all(|cache| cache.built_ranges() == 3));
        } else {
            assert!(calc.grouped_criteria.iter().all(|cache| cache.entries_len() <= 4));
        }
    }
}

#[test]
fn alternating_single_criterion_ranges_keep_indexes_and_sum_in_source_order() {
    let mut wb = Workbook::new_blank();
    for r in 0..480 {
        number(&mut wb, r, 0, (r % 9) as f64);
        number(&mut wb, r, 1, (r % 11) as f64);
        number(&mut wb, r, 2, r as f64 + 0.5);
    }
    let mut calc = Calc::new(&wb);
    for k in 0..8 {
        for (column, modulus) in [("A", 9), ("B", 11)] {
            let rows: Vec<_> = (0..480).filter(|r| r % modulus == k).collect();
            let sum: f64 = rows.iter().map(|&r| r as f64 + 0.5).sum();
            let range = format!("{column}1:{column}480");
            assert_eq!(calc.eval_node(&parse(&format!("SUMIF({range},{k},C1:C480)")).unwrap(), 0), FValue::number(sum));
            assert_eq!(calc.eval_node(&parse(&format!("COUNTIF({range},{k})")).unwrap(), 0), FValue::number(rows.len() as f64));
        }
    }
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
fn named_lambda_dynamic_updates_dirty_save_and_reopen() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Data".into();
    wb.sheets.push(Sheet::new("Report"));
    wb.names.extend([
        ("ReadTarget".into(), None, "_xlfn.LAMBDA(_xlpm.addr,INDIRECT(_xlpm.addr))".into()),
        ("ReadAlias".into(), None, "ReadTarget".into()),
        ("ReadTotal".into(), Some(1), "LAMBDA(addr,ReadAlias(addr)+1)".into()),
    ]);
    wb.names_dirty = true;
    number(&mut wb, 0, 0, 3.0);
    // Simulate an imported file whose cached values are already up to date.
    set_formula(&mut wb.sheets[0], 0, 1, "=A1*2", FValue::number(6.0));
    set_formula(&mut wb.sheets[1], 0, 0, "=ReadTotal(\"Data!B1\")", FValue::number(7.0));
    set_formula(&mut wb.sheets[1], 0, 1, "=A1*10", FValue::number(70.0));
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("named-lambda.xlsx");
    crate::xlsx::save_atomic_with_receipt(&path, &wb).unwrap().apply_to(&mut wb);
    let mut wb = crate::xlsx::load(&path).unwrap();
    assert_eq!(wb.names.len(), 3, "{:?}", wb.names);
    assert_eq!(wb.sheets[1].value(0, 1), Value::Number(70.0));
    let deps = deps::Deps::build(&wb);
    number(&mut wb, 0, 0, 8.0);
    assert_eq!(recalc_dirty(&mut wb, &deps, &[(0, 0, 0)]), 3);
    assert_eq!(wb.sheets[1].value(0, 1), Value::Number(170.0));
    crate::xlsx::save_atomic_with_receipt(&path, &wb).unwrap().apply_to(&mut wb);
    let mut reopened = crate::xlsx::load(&path).unwrap();
    assert_eq!(reopened.sheets[1].value(0, 1), Value::Number(170.0));
    recalc_all_force(&mut reopened);
    assert_eq!(reopened.sheets[1].value(0, 1), Value::Number(170.0));
}

#[test]
fn named_lambda_respects_scope_builtins_shadowing_and_recursion() {
    let mut wb = Workbook::new_blank();
    wb.sheets.push(Sheet::new("Report"));
    number(&mut wb, 0, 0, 2.0);
    wb.sheets[1].set(0, 0, Cell { value: Value::Number(5.0), formula: None });
    wb.names.extend([
        ("AddRate".into(), None, "LAMBDA(x,x+A1)".into()),
        ("AddRate".into(), Some(1), "LAMBDA(x,x+A1+10)".into()),
        ("BaseRate".into(), None, "Sheet1!A1".into()),
        ("WithRate".into(), None, "LAMBDA(x,x+BaseRate)".into()),
        ("LoopFunc".into(), None, "LAMBDA(x,OtherLoop(x))".into()),
        ("OtherLoop".into(), None, "LAMBDA(x,LoopFunc(x))".into()),
        ("CountDown".into(), None, "LAMBDA(x,IF(x>0,CountDown(x-1)+1,0))".into()),
        ("ABS".into(), None, "LAMBDA(x,999)".into()),
    ]);
    let mut calc = Calc::new(&wb);
    for (sheet, src, expected) in [
        (0, "AddRate(1)", 3.0),
        (1, "AddRate(1)", 16.0),
        (0, "Report!AddRate(A1)", 17.0),
        (1, "LET(BaseRate,99,WithRate(1))", 3.0),
        (0, "LET(AddRate,LAMBDA(x,x*10),AddRate(3))", 30.0),
        (0, "ABS(-5)", 5.0),
        (0, "CountDown(3)", 3.0),
    ] {
        assert_eq!(calc.eval_node(&parse(src).unwrap(), sheet), FValue::number(expected), "{src}");
    }
    assert_eq!(calc.eval_node(&parse("LoopFunc(5)").unwrap(), 0), FValue::Error(ErrKind::Num));
    drop(calc);
    formula(&mut wb, 0, 1, "=Report!AddRate(A1)");
    formula(&mut wb, 0, 2, "=LET(BaseRate,99,WithRate(1))");
    let deps = deps::Deps::build(&wb);
    assert!(!deps.requires_full_recalc());
    assert_eq!(deps.closure(&[(1, 0, 0)]), HashSet::from([(0, 0, 1)]));
    assert_eq!(deps.closure(&[(0, 0, 0)]), HashSet::from([(0, 0, 1), (0, 0, 2)]));
    formula(&mut wb, 0, 3, "=LoopFunc(5)");
    assert!(deps::Deps::build(&wb).requires_full_recalc());
}

#[test]
fn single_cell_legacy_scalar_keeps_unrelated_edits_incremental() {
    let mut wb = Workbook::new_blank();
    number(&mut wb, 0, 0, 2.0);
    formula(&mut wb, 0, 1, "=IF(A1>2,SEQUENCE(2),SUM(A1:A1))");
    wb.sheets[0].arrays.push((CellRef::new(0, 1), (0, 1, 0, 1)));
    formula(&mut wb, 0, 2, "=D1+1");
    formula(&mut wb, 0, 4, "=B2+1");
    recalc_all_force(&mut wb);
    assert!(wb.sheets[0].spill_shapes.is_empty());
    let deps = deps::Deps::build(&wb);
    number(&mut wb, 0, 3, 9.0);
    assert_eq!(recalc_dirty(&mut wb, &deps, &[(0, 0, 3)]), 1);
    assert_eq!(wb.sheets[0].value(0, 2), Value::Number(10.0));
    number(&mut wb, 0, 0, 4.0);
    let mut oracle = wb.clone();
    recalc_all_force(&mut oracle);
    recalc_dirty(&mut wb, &deps, &[(0, 0, 0)]);
    assert_eq!(wb.sheets[0].value(0, 4), Value::Number(3.0));
    assert_eq!(wb.sheets[0].spill_shapes, oracle.sheets[0].spill_shapes);
    assert_eq!(wb.sheets[0].value(1, 1), oracle.sheets[0].value(1, 1));
}

#[test]
fn runtime_references_recalculate_changed_target_and_dependents_only() {
    let mut wb = Workbook::new_blank();
    number(&mut wb, 0, 0, 3.0);
    formula(&mut wb, 0, 1, "=A1*2");
    formula(&mut wb, 0, 2, "=INDIRECT(\"B1\")+OFFSET(A1,0,1)");
    formula(&mut wb, 0, 3, "=C1+1");
    formula(&mut wb, 0, 4, "=42");
    recalc_all_force(&mut wb);
    let deps = deps::Deps::build(&wb);
    assert!(!deps.requires_full_recalc());
    number(&mut wb, 0, 0, 8.0);
    assert_eq!(recalc_dirty(&mut wb, &deps, &[(0, 0, 0)]), 3);
    assert_eq!(wb.sheets[0].value(0, 3), Value::Number(33.0));
    assert_eq!(recalc_dirty(&mut wb, &deps, &[(0, 9, 9)]), 2);
}

#[test]
fn one_cell_dynamic_arrays_recalculate_in_scope_and_expand_with_tail_dependents() {
    // Exercise both the immediate interactive budget and worker calculation.
    for padding in [0, 1250] {
        let mut wb = Workbook::new_blank();
        number(&mut wb, 0, 0, 7.0);
        number(&mut wb, 1, 0, 8.0);
        number(&mut wb, 0, 1, 1.0);
        wb.sheets[0].set(0, 2, Cell { value: Value::Text("A1:A1".into()), formula: None });
        for (col, src) in [(3, "=OFFSET(A1,0,0,B1,1)"), (4, "=INDIRECT(C1)"),
                           (5, "=SUM(D1#)+SUM(E1#)"), (6, "=D2+E2"), (7, "=999")] {
            formula(&mut wb, 0, col, src);
        }
        for r in 0..padding { number(&mut wb, r, 10, 1.0); }
        recalc_all_force(&mut wb);
        let deps = deps::Deps::build(&wb);
        assert!(!deps.requires_full_recalc());
        number(&mut wb, 0, 0, 12.0);
        assert_eq!(recalc_dirty(&mut wb, &deps, &[(0,0,0)]), 3);
        assert_eq!(wb.sheets[0].value(0,5), Value::Number(24.0));
        number(&mut wb, 0, 0, 13.0);
        assert_eq!(recalc_interactive_plan(&mut wb, &deps, &[(0,0,0)], true), InteractiveRecalc::Complete);
        assert_eq!(wb.sheets[0].value(0,5), Value::Number(26.0));
        for col in [3,4] {
            assert_eq!(wb.sheets[0].spill_shapes.get(&CellRef::new(0,col)), Some(&(1,1)));
        }
        // A bare INDIRECT address returns a scalar. Its displayed amount is unchanged,
        // but E1# must stop resolving as soon as the array shape disappears.
        wb.sheets[0].set(0,2, Cell { value: Value::Text("A1".into()), formula: None });
        recalc_dirty(&mut wb, &deps, &[(0,0,2)]);
        assert!(!wb.sheets[0].spill_shapes.contains_key(&CellRef::new(0,4)));
        assert_eq!(wb.sheets[0].value(0,5), Value::Error(ErrKind::Ref));
        number(&mut wb, 0, 1, 2.0);
        wb.sheets[0].set(0,2, Cell { value: Value::Text("A1:A2".into()), formula: None });
        let seeds = [(0,0,1), (0,0,2)];
        let mut oracle = wb.clone();
        recalc_all_force(&mut oracle);
        match recalc_interactive_plan(&mut wb, &deps, &seeds, true) {
            InteractiveRecalc::Complete => {},
            InteractiveRecalc::BackgroundFull => recalc_all_force(&mut wb),
            InteractiveRecalc::BackgroundDirty => { recalc_dirty(&mut wb, &deps, &seeds); }
        }
        assert_eq!(wb.sheets[0].spill_shapes, oracle.sheets[0].spill_shapes);
        assert_eq!(wb.sheets[0].value(0,5), Value::Number(42.0));
        assert_eq!(wb.sheets[0].value(0,6), Value::Number(16.0));
    }
}

#[test]
fn editing_a_large_spill_anchor_defers_evaluation_as_well_as_writeback() {
    let mut wb = Workbook::new_blank();
    number(&mut wb, 0, 0, 10.0);
    number(&mut wb, 0, 1, 1.0);
    formula(&mut wb, 0, 2, "=SEQUENCE(5000,1,A1)");
    formula(&mut wb, 0, 4, "=B1+1");
    recalc_all_force(&mut wb);
    let deps = deps::Deps::build(&wb);
    number(&mut wb, 0, 1, 8.0);
    let seeds = [(0,0,1), (0,0,2), (0,0,4)];
    assert!(!eval_formula_seeds_immediate(&mut wb, &seeds));
    assert_eq!(recalc_interactive_plan(&mut wb, &deps, &seeds, true), InteractiveRecalc::BackgroundFull);
    // If immediate evaluation ran before deferral, E1 would already contain 9.
    assert_eq!(wb.sheets[0].value(0, 4), Value::Number(2.0));
    recalc_all_force(&mut wb);
    assert_eq!(wb.sheets[0].value(0, 4), Value::Number(9.0));
    assert_eq!(wb.sheets[0].value(4999, 2), Value::Number(5009.0));
}

#[test]
fn blank_tails_do_not_make_a_large_indirect_spill_an_interactive_small_sheet() {
    let mut wb = Workbook::new_blank();
    number(&mut wb, 0, 0, 1.0);
    number(&mut wb, 9999, 0, 42.0);
    formula(&mut wb, 0, 3, r#"=INDIRECT("A1:A10000")"#);
    recalc_all_force(&mut wb);
    assert!(wb.sheets[0].len() <= 1200);
    assert_eq!(wb.sheets[0].spill_shapes.get(&CellRef::new(0,3)), Some(&(10000,1)));
    let deps = deps::Deps::build(&wb);
    assert_eq!(recalc_interactive_plan(&mut wb, &deps, &[(0,0,3)], true), InteractiveRecalc::BackgroundFull);
    assert_eq!(recalc_interactive_plan(&mut wb, &deps, &[(0,0,8)], true), InteractiveRecalc::BackgroundDirty);
}

#[test]
fn unchanged_indirect_rectangle_stays_incremental_and_changed_tails_refresh_readers() {
    let mut wb = Workbook::new_blank();
    for (r, c, v) in [(0, 0, 2.0), (1, 0, 3.0), (0, 1, 5.0), (1, 1, 7.0)] {
        number(&mut wb, r, c, v);
    }
    formula(&mut wb, 0, 3, r#"=INDIRECT("A1:B2")"#);
    formula(&mut wb, 0, 6, "=SUM(D1#)");
    formula(&mut wb, 0, 7, "=E2+1");
    for r in 0..1400 { formula(&mut wb, r, 9, "=1+1"); }
    recalc_all_force(&mut wb);
    let deps = deps::Deps::build(&wb);
    number(&mut wb, 5, 2, 11.0);
    assert!(recalc_dirty(&mut wb, &deps, &[(0, 5, 2)]) < 10);
    assert_eq!(recalc_interactive_plan(&mut wb, &deps, &[(0, 5, 2)], true), InteractiveRecalc::Complete);
    assert_eq!(wb.sheets[0].value(0, 6), Value::Number(17.0));
    assert_eq!(wb.sheets[0].value(0, 7), Value::Number(8.0));
    number(&mut wb, 1, 1, 20.0);
    recalc_dirty(&mut wb, &deps, &[(0, 1, 1)]);
    assert_eq!(wb.sheets[0].value(0, 6), Value::Number(30.0));
    assert_eq!(wb.sheets[0].value(0, 7), Value::Number(21.0));
}

#[test]
fn unrelated_edit_preserves_materialized_spill_and_recalculates_its_reader() {
    for rows in [1, 3] {
        let mut wb = Workbook::new_blank();
        number(&mut wb, 0, 0, rows as f64);
        formula(&mut wb, 0, 1, "=SEQUENCE(A1,1)");
        formula(&mut wb, 0, 2, "=D1+SUM(B1#)");
        formula(&mut wb, 0, 4, "=B2+1");
        recalc_all_force(&mut wb);
        let deps = deps::Deps::build(&wb);
        assert!(!deps.requires_full_recalc());
        number(&mut wb, 0, 3, 10.0);
        assert_eq!(recalc_dirty(&mut wb, &deps, &[(0, 0, 3)]), 1);
        assert_eq!(wb.sheets[0].value(0, 2), Value::Number(10.0 + (rows * (rows + 1) / 2) as f64));
        number(&mut wb, 0, 0, 2.0);
        recalc_dirty(&mut wb, &deps, &[(0, 0, 0)]);
        assert_eq!(wb.sheets[0].value(0, 4), Value::Number(3.0));
        assert_eq!(wb.sheets[0].value(0, 2), Value::Number(13.0));
    }
}

#[test]
fn empty_operand_outside_used_bounds_does_not_create_a_spill() {
    let mut wb = Workbook::new_blank();
    for (row, text) in ["=-B2:B1048576", "=B2:B1048576+1", "=B2:B1048576%"].into_iter().enumerate() {
        formula(&mut wb, row as u32, 0, text);
    }
    recalc_all_force(&mut wb);
    assert!(wb.sheets[0].spill_shapes.is_empty());
    for row in 0..3 {
        assert_eq!(wb.sheets[0].value(row, 0), Value::Error(ErrKind::Value));
    }
}

#[test]
fn filter_visibility_seeds_include_cross_sheet_summary_and_its_dependents() {
    let mut wb = Workbook::new_blank();
    wb.sheets.push(crate::Sheet::new("Summary"));
    number(&mut wb, 0, 0, 2.0);
    number(&mut wb, 1, 0, 3.0);
    set_formula(&mut wb.sheets[1], 0, 0, "=SUBTOTAL(9,Sheet1!A1:A2)", FValue::Empty);
    set_formula(&mut wb.sheets[1], 0, 1, "=A1*2", FValue::Empty);
    set_formula(&mut wb.sheets[1], 0, 2, "=SUM(A5:A8)", FValue::Empty);
    recalc_all_force(&mut wb);
    let deps = deps::Deps::build(&wb);
    let seeds = deps.visibility_seeds(0).unwrap();
    assert_eq!(seeds, vec![(1, 0, 0)]);
    assert!(deps.visibility_seeds(1).unwrap().is_empty());
    wb.sheets[0].hidden_rows.insert(1);
    assert_eq!(recalc_dirty(&mut wb, &deps, &seeds), 2);
    assert_eq!(wb.sheets[1].value(0, 1), Value::Number(4.0));
}

#[test]
fn repeated_exact_lookup_shares_only_literal_keys_and_rejects_divergent_snapshots() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Data".into();
    wb.sheets.push(Sheet::new("Report"));
    for r in 0..1200 { number(&mut wb,r,0,f64::from(r)); number(&mut wb,r,1,f64::from(r + 7)); }
    set_formula(&mut wb.sheets[1], 0, 0, "=VLOOKUP(750,Data!A1:B1200,2,FALSE)", FValue::Empty);
    let read = |wb: &Workbook| {
        let mut calc = Calc::new(wb);
        let value = calc.cell_value(1,0,0);
        (value.to_number(), calc.range_reads)
    };
    assert_eq!(read(&wb), (Ok(757.0),1200));
    assert_eq!(std::thread::scope(|s| s.spawn(|| read(&wb)).join().unwrap()), (Ok(757.0),0));
    let mut first = wb.clone();
    let mut second = wb.clone();
    number(&mut first,0,0,750.0);
    number(&mut second,1,0,750.0);
    assert_eq!(read(&first), (Ok(7.0),1200));
    assert_eq!(read(&second), (Ok(8.0),1200));
    assert_eq!(read(&first), (Ok(7.0),0));
    formula(&mut first,0,0,"=749+1");
    assert_eq!(read(&first), (Ok(7.0),1200));
    assert_eq!(read(&first), (Ok(7.0),1200));
}

#[test]
fn range_bindings_and_array_values_share_materialized_storage() {
    let mut wb = Workbook::new_blank();
    wb.names.push(("RangeName".into(), None, "A1:A300".into()));
    for r in 0..300 { number(&mut wb,r,0,r as f64); }
    let mut calc = Calc::new(&wb);
    let mut results = Vec::new();
    for src in ["A1:A300", "LET(x,A1:A300,x)", "RangeName", "INDIRECT(\"A1:A300\")"] {
        let FValue::Array(array) = calc.eval_node(&parse(src).unwrap(),0) else { panic!("{src}"); };
        assert_eq!(array.get(299,0),FValue::Number(299.0));
        results.push(array.data.clone());
    }
    assert!(results[1..].iter().all(|r| std::rc::Rc::ptr_eq(&results[0],r)));
    assert_eq!(calc.range_reads,300);
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
fn plain_named_lookup_table_uses_key_column_and_respects_let_shadowing() {
    let mut wb = Workbook::new_blank();
    wb.names.push(("TableData".into(), None, "Sheet1!$A$1:$C$300".into()));
    for row in 0..300 {
        number(&mut wb, row, 0, row as f64);
        number(&mut wb, row, 1, 10.0 + row as f64);
    }
    let mut calc = Calc::new(&wb);
    assert_eq!(calc.eval_node(&parse("VLOOKUP(9,TableData,2,0)").unwrap(), 0), FValue::number(19.0));
    assert_eq!(calc.range_reads(), 300);
    let shadowed = parse("LET(TableData,{9,85},VLOOKUP(9,TableData,2,0))").unwrap();
    assert_eq!(calc.eval_node(&shadowed, 0), FValue::number(85.0));
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
