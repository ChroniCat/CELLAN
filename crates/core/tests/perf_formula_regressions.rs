//! Phase 2 comprehensive regression & performance invariant tests for `formula-calc`:
//! 1. Headless numeric `XLOOKUP` + all `match_mode`/`search_mode` combinations + zero `return_array` materialization
//! 2. Wide-table `INDEX` / `INDEX+MATCH` `O(1)` single-cell reads (`range_reads == 0`) & 1D row/col slices
//! 3. Multi-core `dependency_batch` on `> 32,768`-row ranges, Virtual Range Nodes, static defined names, and `recalc_sheet`
//! 4. Lazy `SUMIFS`/`COUNTIFS`/`AVERAGEIFS`/`MINIFS`/`MAXIFS` (`GroupedCriteria` fractional keys, `OrderedCriteria` bit-exact fractional sums, `MatchIndexes` sparse row probe) + 256-row page-delta `SharedLookup`
//! 5. Dynamic array spill local `recalc_dirty` convergence (expand/shrink/obstruct `#SPILL!`/recover without full-workbook recalc) & Pass-0 single-pass convergence
//! 6. `compiled` template generalization, `Deps` shared-area deduplication & `build_with_cached_baseline`, `REGEX*` cache, `arr::apply_xml` linear scan, and external `[book]` `VLOOKUP`/`XLOOKUP`/`INDEX`

use sheetcore::arr::ArrayFml;
use sheetcore::formula::deps::Deps;
use sheetcore::formula::value::FValue;
use sheetcore::formula::{
    self, ErrKind, InteractiveRecalc, eval_cell, eval_formula, eval_formula_reads, recalc_all_force,
    recalc_dirty, recalc_interactive_plan, recalc_sheet, set_formula,
};
use sheetcore::structural::Structural;
use sheetcore::xlinks::{ExternalBook, ExternalCache};
use sheetcore::{Cell, Sheet, Value, Workbook};
use std::collections::BTreeMap;

fn num_cell(n: f64) -> Cell {
    Cell {
        value: Value::Number(n),
        formula: None,
    }
}

fn txt_cell(s: &str) -> Cell {
    Cell {
        value: Value::Text(s.into()),
        formula: None,
    }
}

#[test]
fn xlookup_headless_numeric_and_all_modes_zero_return_materialization() {
    let mut wb = Workbook::new_blank();
    // 500 rows: headless numeric table where row 0 of return_array is -1.0!
    // Old AST fast path evaluated nodes[2] (C1:C500 -> -1.0) as match_mode!
    for r in 0..500u32 {
        wb.sheets[0].set(r, 0, num_cell((r as f64) * 10.0)); // A: ascending keys 0, 10, 20, ...
        wb.sheets[0].set(r, 1, txt_cell(&format!("ITEM-{r:03}"))); // B: text keys
        let val = if r == 0 { -1.0 } else { (r as f64) * 100.0 + 0.5 };
        wb.sheets[0].set(r, 2, num_cell(val)); // C: return col 1 (starts with -1.0!)
        wb.sheets[0].set(r, 3, num_cell((r as f64) * 1000.0)); // D: return col 2
        wb.sheets[0].set(r, 4, num_cell(((499 - r) as f64) * 10.0)); // E: descending keys
    }
    // Duplicate key at row 10 and row 410 in column F for reverse search_mode = -1
    for r in 0..500u32 {
        let dup = if r == 10 || r == 410 {
            "DUP-KEY".to_string()
        } else {
            format!("UNIQ-{r}")
        };
        wb.sheets[0].set(r, 5, txt_cell(&dup));
    }

    // 1. Exact match on headless numeric table: missing key 15 MUST return #N/A, NOT row 1's -1.0/100.5!
    // Must read ONLY lookup column A (500 cells), NEVER materializing C1:C500!
    let (v_miss, reads_cold) = eval_formula_reads(&wb, 0, "=XLOOKUP(15, A1:A500, C1:C500)");
    assert_eq!(v_miss, FValue::Error(ErrKind::Na));
    assert_eq!(reads_cold, 500);
    // Exact match hit on row 0 (-1.0) and row 25 (2500.5) — both hit warm SharedLookup in O(1) (0 reads)!
    let (v0, reads_hit0) = eval_formula_reads(&wb, 0, "=XLOOKUP(0, A1:A500, C1:C500)");
    assert_eq!(v0, FValue::number(-1.0));
    assert_eq!(reads_hit0, 0);
    let (v25, reads_warm) = eval_formula_reads(&wb, 0, "=XLOOKUP(250, A1:A500, C1:C500)");
    assert_eq!(v25, FValue::number(2500.5));
    assert_eq!(reads_warm, 0, "SharedLookup warm hit must be O(1) with 0 range reads");

    // 2. if_not_found short-circuiting: 1/0 must NOT be evaluated on hit, and MUST be returned on miss
    assert_eq!(
        eval_formula(&wb, 0, "=XLOOKUP(250, A1:A500, C1:C500, 1/0)"),
        FValue::number(2500.5)
    );
    assert_eq!(
        eval_formula(&wb, 0, "=XLOOKUP(255, A1:A500, C1:C500, \"MISSING\")"),
        FValue::Text("MISSING".into())
    );
    // Omitted empty 4th argument `, ,` defaults to #N/A when match_mode = 0
    assert_eq!(
        eval_formula(&wb, 0, "=XLOOKUP(255, A1:A500, C1:C500, , 0)"),
        FValue::Error(ErrKind::Na)
    );

    // 3. match_mode = -1 (next smaller) and 1 (next larger), linear and binary search (2 / -2)
    assert_eq!(
        eval_formula(&wb, 0, "=XLOOKUP(255, A1:A500, C1:C500, , -1, 1)"),
        FValue::number(2500.5)
    );
    assert_eq!(
        eval_formula(&wb, 0, "=XLOOKUP(255, A1:A500, C1:C500, , 1, 1)"),
        FValue::number(2600.5)
    );
    assert_eq!(
        eval_formula(&wb, 0, "=XLOOKUP(255, A1:A500, C1:C500, , -1, 2)"),
        FValue::number(2500.5)
    );
    assert_eq!(
        eval_formula(&wb, 0, "=XLOOKUP(255, A1:A500, C1:C500, , 1, 2)"),
        FValue::number(2600.5)
    );
    // Descending binary search (search_mode = -2) on column E:
    // E has (499 - r) * 10: row 473 -> 260, row 474 -> 250
    assert_eq!(
        eval_formula(&wb, 0, "=XLOOKUP(255, E1:E500, C1:C500, , -1, -2)"),
        FValue::number(47400.5)
    );
    assert_eq!(
        eval_formula(&wb, 0, "=XLOOKUP(255, E1:E500, C1:C500, , 1, -2)"),
        FValue::number(47300.5)
    );

    // 4. Reverse exact search (search_mode = -1) via O(1) LookupMap::get_last
    assert_eq!(
        eval_formula(&wb, 0, "=XLOOKUP(\"DUP-KEY\", F1:F500, C1:C500, , 0, 1)"),
        FValue::number(1000.5)
    );
    let (v_last, reads_last) =
        eval_formula_reads(&wb, 0, "=XLOOKUP(\"DUP-KEY\", F1:F500, C1:C500, , 0, -1)");
    assert_eq!(v_last, FValue::number(41000.5));
    assert_eq!(reads_last, 0, "Reverse exact XLOOKUP reuses SharedLookup in O(1)");
    assert_eq!(
        eval_formula(&wb, 0, "=XMATCH(\"DUP-KEY\", F1:F500, 0, -1)"),
        FValue::number(411.0)
    );

    // 5. Wildcard match_mode = 2 vs literal match_mode = 0
    wb.sheets[0].set(499, 1, txt_cell("STAR*LIT"));
    assert_eq!(
        eval_formula(&wb, 0, "=XLOOKUP(\"STAR*LIT\", B1:B500, C1:C500, , 0)"),
        FValue::number(49900.5)
    );
    assert_eq!(
        eval_formula(&wb, 0, "=XLOOKUP(\"item-12?\", B1:B500, C1:C500, , 2)"),
        FValue::number(12000.5)
    );

    // 6. Multi-column return_array spill slices ONLY the 2 matched cells, not 1000 cells!
    let (spill, spill_reads) = eval_formula_reads(&wb, 0, "=XLOOKUP(250, A1:A500, C1:D500)");
    let FValue::Array(arr) = spill else {
        panic!("expected 1x2 spill, got {spill:?}");
    };
    assert_eq!((arr.rows, arr.cols), (1, 2));
    assert_eq!(arr.get(0, 0), FValue::number(2500.5));
    assert_eq!(arr.get(0, 1), FValue::number(25000.0));
    assert_eq!(spill_reads, 2);
}

#[test]
fn wide_table_index_match_o1_single_cell_and_row_col_slices() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(0, 0, txt_cell("K-0"));
    wb.sheets[0].set(500, 0, txt_cell("K-500"));
    wb.sheets[0].set(500, 25, num_cell(9876.5));
    wb.names.push(("WideTbl".into(), None, "Sheet1!$A$1:$Z$100000".into()));

    // Single-cell INDEX on 2.6M-cell range reads 0 range cells!
    let (v, reads) = eval_formula_reads(&wb, 0, "=INDEX(A1:Z100000, 501, 26)");
    assert_eq!(v, FValue::number(9876.5));
    assert_eq!(reads, 0);

    // Unpopulated cell inside declared A1:Z100000 (beyond used_bounds row 500) returns Empty (0.0), NOT #REF!
    let (v_empty, reads_empty) = eval_formula_reads(&wb, 0, "=INDEX(A1:Z100000, 90000, 20)");
    assert_eq!(v_empty, FValue::Empty);
    assert_eq!(reads_empty, 0);

    // Named-range INDEX + whole-col MATCH(A:A) reads only used rows (501), then 0 on warm cache
    let (vm1, r1) = eval_formula_reads(&wb, 0, "=INDEX(WideTbl, MATCH(\"K-500\", A:A, 0), 26)");
    assert_eq!(vm1, FValue::number(9876.5));
    assert_eq!(r1, 501);
    let (vm2, r2) = eval_formula_reads(&wb, 0, "=INDEX(WideTbl, MATCH(\"K-500\", A:A, 0), 26)");
    assert_eq!(vm2, FValue::number(9876.5));
    assert_eq!(r2, 0);

    // 1D row/col slices via row=0 / col=0 and 1-row horizontal vector
    for r in 0..3u32 {
        for c in 0..4u32 {
            wb.sheets[0].set(10 + r, c, num_cell((r * 10 + c + 1) as f64));
        }
    }
    assert_eq!(
        eval_formula(&wb, 0, "=SUM(INDEX(A11:D13, 0, 3))"),
        FValue::number(3.0 + 13.0 + 23.0)
    );
    assert_eq!(
        eval_formula(&wb, 0, "=SUM(INDEX(A11:D13, 2, 0))"),
        FValue::number(11.0 + 12.0 + 13.0 + 14.0)
    );
    assert_eq!(
        eval_formula(&wb, 0, "=INDEX(A11:D11, 4)"),
        FValue::number(4.0)
    );
}

#[test]
fn multicore_dependency_batch_large_ranges_virtual_nodes_and_defined_names() {
    let mut wb = Workbook::new_blank();
    wb.names.push(("Rate".into(), None, "=0.25".into()));
    // 40,000 rows (> old 32,768 MAX_RANGE limit!) in column A
    for r in 0..40_000u32 {
        wb.sheets[0].set(r, 0, num_cell(1.0));
    }
    // 120 formula cells in B1:B120 (> 64 unresolved formulas -> triggers Virtual Range Node!)
    for r in 0..120u32 {
        set_formula(&mut wb.sheets[0], r, 1, &format!("=A{}*2", r + 1), FValue::Empty);
    }
    // 320 downstream formulas referencing both >32,768 range A1:A40000, B1:B120 virtual range node, and defined name `Rate`
    for r in 0..320u32 {
        set_formula(
            &mut wb.sheets[0],
            r,
            2,
            &format!("=SUM(A1:A40000)*Rate + SUM(B1:B120) + {}", r + 1),
            FValue::Empty,
        );
    }
    // Unrelated second sheet with volatile canary
    let mut other = Sheet::new("Other");
    other.set(
        0,
        0,
        Cell {
            value: Value::Number(777.0),
            formula: Some("=RAND()".into()),
        },
    );
    wb.sheets.push(other);

    // Test multi-core recalc_sheet on Sheet 0 first (selected.is_some())
    let n = recalc_sheet(&mut wb, 0);
    assert_eq!(n, 440);
    // Expected: 40000 * 0.25 + 120 * 2 + (r + 1) = 10000 + 240 + r + 1 = 10241 + r
    for r in [0u32, 63, 119, 250, 319] {
        assert_eq!(
            wb.sheets[0].value(r, 2),
            Value::Number(10241.0 + r as f64),
            "row {r}"
        );
    }
    assert_eq!(wb.sheets[1].value(0, 0), Value::Number(777.0));

    // Also test full recalc_all_force parity
    recalc_all_force(&mut wb);
    assert_eq!(wb.sheets[0].value(319, 2), Value::Number(10241.0 + 319.0));
}

#[test]
fn conditional_aggregates_lazy_fractional_sparse_and_incremental_page_delta() {
    let mut wb = Workbook::new_blank();
    // 4,096 rows (16 pages of 256 rows)
    for r in 0..4_096u32 {
        let region = match r % 4 {
            0 => "East",
            1 => "West",
            2 => "North",
            _ => "South",
        };
        let frac_key = ((r % 5) as f64) * 0.25; // fractional keys 0.0, 0.25, 0.5, 0.75, 1.0
        let unique_id = format!("ORD-{r:05}");
        let amount = ((r % 13) as f64) / 10.0 + 1.0; // fractional amounts
        wb.sheets[0].set(r, 0, txt_cell(region));
        wb.sheets[0].set(r, 1, num_cell(frac_key));
        wb.sheets[0].set(r, 2, txt_cell(&unique_id));
        wb.sheets[0].set(r, 3, num_cell(amount));
        wb.sheets[0].set(r, 4, num_cell(r as f64));
    }

    // 1. Fractional keys + all 6 aggregate functions (SUMIFS, COUNTIFS, AVERAGEIF, AVERAGEIFS, MINIFS, MAXIFS)
    for query in [
        "=SUMIFS(D1:D4096, A1:A4096, \"east\", B1:B4096, 0.25)",
        "=COUNTIFS(A1:A4096, \"WEST\", B1:B4096, 0.5)",
        "=AVERAGEIFS(D1:D4096, A1:A4096, \"North\", B1:B4096, 0.75)",
        "=MINIFS(D1:D4096, A1:A4096, \"South\", B1:B4096, 1)",
        "=MAXIFS(D1:D4096, A1:A4096, \"East\", B1:B4096, 0)",
        "=AVERAGEIF(E1:E4096, \">1000\", D1:D4096)",
        "=MINIFS(D1:D4096, E1:E4096, \">=3000\")",
        "=MAXIFS(D1:D4096, E1:E4096, \"<500\")",
        "=SUMIFS(D1:D4096, E1:E4096, \">2048\")",
    ] {
        // Warm twice to exercise both cold build and BuiltGroups / BuiltOrdered / criteria_cache hits
        let mut calc = formula::Calc::new(&wb);
        let node = formula::parse(query.trim_start_matches('=')).unwrap();
        let first = calc.eval_node(&node, 0);
        let second = calc.eval_node(&node, 0);
        assert_eq!(first, second, "{query}");
    }

    // 2. High-selectivity sparse row probe: C1:C4096 = "ORD-03500" matches ONLY row 3500!
    // Must NOT materialize A1:A4096 or D1:D4096 on warm SharedCondColumn!
    let _ = eval_formula(&wb, 0, "=SUMIFS(D1:D4096, C1:C4096, \"ORD-00001\", A1:A4096, \"West\")");
    let (sparse_v, sparse_reads) = eval_formula_reads(
        &wb,
        0,
        "=SUMIFS(D1:D4096, C1:C4096, \"ORD-03500\", A1:A4096, \"East\")",
    );
    let expected_amt = ((3500 % 13) as f64) / 10.0 + 1.0;
    assert_eq!(sparse_v, FValue::number(expected_amt));
    assert_eq!(sparse_reads, 0, "Warm SharedCondColumn + sparse row probe must do 0 full-column reads");

    // 3. 256-row page-delta incremental SharedLookup:
    // Warm VLOOKUP on C1:E4096 (16 pages), then mutate 1 cell in row 3500 (1 page)!
    let (v_before, _) = eval_formula_reads(&wb, 0, "=VLOOKUP(\"ORD-03500\", C1:E4096, 3, FALSE)");
    assert_eq!(v_before, FValue::number(3500.0));
    wb.sheets[0].set(3500, 2, txt_cell("ORD-RENAMED"));
    let (v_miss, reads_delta) =
        eval_formula_reads(&wb, 0, "=VLOOKUP(\"ORD-03500\", C1:E4096, 3, FALSE)");
    assert_eq!(v_miss, FValue::Error(ErrKind::Na));
    assert!(
        reads_delta <= 256,
        "Expected single 256-row page delta rebuild, got {reads_delta} reads"
    );
    let (v_hit, reads_after) =
        eval_formula_reads(&wb, 0, "=VLOOKUP(\"ORD-RENAMED\", C1:E4096, 3, FALSE)");
    assert_eq!(v_hit, FValue::number(3500.0));
    assert_eq!(reads_after, 0);
}

#[test]
fn spill_local_dirty_convergence_and_blocked_spill_recovery_without_full_recalc() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(0, 0, num_cell(3.0)); // A1 = 3
    set_formula(&mut wb.sheets[0], 0, 1, "=SEQUENCE(A1, 1)", FValue::Empty); // B1 spills B1:B3
    set_formula(&mut wb.sheets[0], 0, 2, "=SUM(B1:B6)", FValue::Empty); // C1 reads spill tails
    set_formula(&mut wb.sheets[0], 0, 3, "=SUM(B1#)", FValue::Empty); // D1 reads spill operator

    // Canary sheet (> 1,200 cells so `recalc_interactive_plan` takes the large-workbook path!):
    // After initial recalc, we plant a stale cached value 999.0 on Sheet 1.
    // Any whole-workbook fallback (`recalc_all_force`) would overwrite 999.0 with 2.0!
    let mut canary = Sheet::new("Canary");
    set_formula(&mut canary, 0, 0, "=1+1", FValue::number(2.0));
    for r in 1..=1_300u32 {
        canary.set(r, 0, num_cell(1.0));
    }
    wb.sheets.push(canary);
    recalc_all_force(&mut wb);
    set_formula(&mut wb.sheets[1], 0, 0, "=1+1", FValue::number(999.0));

    let mut deps = Deps::build_after_recalc(&wb);
    assert_eq!(wb.sheets[0].value(0, 2), Value::Number(6.0));
    assert_eq!(wb.sheets[0].value(0, 3), Value::Number(6.0));

    // 1. Expand spill 3 -> 5 locally in recalc_dirty
    wb.sheets[0].set(0, 0, num_cell(5.0));
    recalc_dirty(&mut wb, &deps, &[(0, 0, 0)]);
    deps.update_cell(&wb, (0, 0, 1));
    assert_eq!(wb.sheets[0].value(4, 1), Value::Number(5.0));
    assert_eq!(wb.sheets[0].value(0, 2), Value::Number(15.0));
    assert_eq!(wb.sheets[0].value(0, 3), Value::Number(15.0));
    assert_eq!(wb.sheets[1].value(0, 0), Value::Number(999.0), "must NOT trigger full recalc");

    // 2. Shrink spill 5 -> 2 locally: old tails B3:B5 cleared, C1 & D1 updated to 3.0!
    wb.sheets[0].set(0, 0, num_cell(2.0));
    recalc_dirty(&mut wb, &deps, &[(0, 0, 0)]);
    deps.update_cell(&wb, (0, 0, 1));
    assert_eq!(wb.sheets[0].value(2, 1), Value::Empty);
    assert_eq!(wb.sheets[0].value(0, 2), Value::Number(3.0));
    assert_eq!(wb.sheets[0].value(0, 3), Value::Number(3.0));
    assert_eq!(wb.sheets[1].value(0, 0), Value::Number(999.0), "must NOT trigger full recalc");

    // 3. Obstruct B2 with user value 100 -> B1 becomes #SPILL! locally!
    wb.sheets[0].set(1, 1, num_cell(100.0));
    recalc_dirty(&mut wb, &deps, &[(0, 1, 1)]);
    deps.update_cell(&wb, (0, 0, 1));
    assert!(!deps.requires_full_recalc(), "probed #SPILL! must NOT force full recalc");
    assert_eq!(wb.sheets[0].value(0, 1), Value::Error(ErrKind::Spill));
    assert_eq!(wb.sheets[0].value(0, 3), Value::Error(ErrKind::Spill));
    assert_eq!(wb.sheets[1].value(0, 0), Value::Number(999.0), "must NOT trigger full recalc");

    // 4. Clear obstruction at B2 -> B1 recovers from #SPILL! to 1..2 locally!
    wb.sheets[0].remove(1, 1);
    assert_eq!(
        recalc_interactive_plan(&mut wb, &deps, &[(0, 1, 1)], true),
        InteractiveRecalc::Complete
    );
    assert_eq!(wb.sheets[0].value(0, 1), Value::Number(1.0));
    assert_eq!(wb.sheets[0].value(1, 1), Value::Number(2.0));
    assert_eq!(wb.sheets[0].value(0, 2), Value::Number(3.0));
    assert_eq!(wb.sheets[0].value(0, 3), Value::Number(3.0));
    assert_eq!(wb.sheets[1].value(0, 0), Value::Number(999.0), "must NOT trigger full recalc");
}

#[test]
fn compiled_templates_deps_dedup_cached_baseline_regex_arr_xml_and_ext_lookup() {
    // 1. Generalized compiled template parity across strings, sheet refs, booleans, whole-col/row, _xlfn.
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Main".into();
    let mut s2 = Sheet::new("My Sheet");
    s2.set(0, 0, num_cell(42.0));
    s2.set(1, 0, num_cell(58.0));
    wb.sheets.push(s2);
    for r in 0..10u32 {
        wb.sheets[0].set(r, 0, txt_cell(if r % 2 == 0 { "OK" } else { "NO" }));
        set_formula(
            &mut wb.sheets[0],
            r,
            1,
            &format!(
                "=IF(A{}=\"OK\", TRUE, FALSE) * SUM('My Sheet'!$A$1:$A$2) + _xlfn.MAX(1e2, {})",
                r + 1,
                r
            ),
            FValue::Empty,
        );
    }
    recalc_all_force(&mut wb);
    assert_eq!(eval_cell(&wb, 0, 0, 1), FValue::number(200.0));
    assert_eq!(eval_cell(&wb, 0, 1, 1), FValue::number(100.0));

    // 2. Deps::build_with_cached_baseline certifies cached <v> values without recalc_all_force
    let certified = Deps::build_with_cached_baseline(&wb, true);
    assert!(certified.can_reuse_calculated_sheets(&wb));

    let mut wb_struct = Workbook::new_blank();
    wb_struct.sheets[0].set(0, 0, num_cell(10.0));
    wb_struct.sheets[0].set(0, 1, num_cell(20.0));
    set_formula(&mut wb_struct.sheets[0], 0, 3, "=INDEX(A1:B1, 1, 2)", FValue::number(20.0));
    let deps_struct = Deps::build_with_cached_baseline(&wb_struct, true);
    let updates = deps_struct
        .structural_updates(&wb_struct, 0, &Structural::InsertRows { at: 50, n: 1 }, 32)
        .expect("structural_updates must succeed");
    assert!(
        updates.is_empty(),
        "INDEX above inserted row 50 must NOT be marked structural_sensitive, got {updates:?}"
    );

    // 3. REGEXTEST / REGEXEXTRACT / REGEXREPLACE cache correctness + error handling
    assert_eq!(
        eval_formula(&wb, 0, "=REGEXTEST(\"INV-2026-009\", \"^INV-\\d{4}-\\d{3}$\")"),
        FValue::Bool(true)
    );
    assert_eq!(
        eval_formula(&wb, 0, "=REGEXEXTRACT(\"Order #4821\", \"\\d+\")"),
        FValue::Text("4821".into())
    );
    assert_eq!(
        eval_formula(&wb, 0, "=REGEXREPLACE(\"a1b2c3\", \"\\d\", \"X\")"),
        FValue::Text("aXbXcX".into())
    );
    assert_eq!(
        eval_formula(&wb, 0, "=REGEXTEST(\"abc\", \"[unclosed\")"),
        FValue::Error(ErrKind::Value)
    );

    // 4. arr::apply_xml linear single-pass multi-anchor rewrite
    let raw_xml = r#"<worksheet><sheetData><row r="1"><c r="A1" s="2"><f>SEQUENCE(2,1)</f><v>1</v></c><c r="B1"><f>SEQUENCE(1,2)</f><v>1</v></c><c r="C1"><v>99</v></c></row></sheetData></worksheet>"#;
    let arrays = vec![
        ArrayFml {
            anchor: (0, 0),
            range: (0, 0, 1, 0),
        },
        ArrayFml {
            anchor: (0, 1),
            range: (0, 1, 0, 2),
        },
    ];
    let patched = sheetcore::arr::apply_xml(raw_xml, &arrays);
    assert!(patched.contains(r#"<c r="A1" s="2"><f t="array" ref="A1:A2">SEQUENCE(2,1)</f>"#));
    assert!(patched.contains(r#"<c r="B1"><f t="array" ref="B1:C1">SEQUENCE(1,2)</f>"#));
    assert!(patched.contains(r#"<c r="C1"><v>99</v></c>"#));

    // 5. External workbook [1]Sheet1!A1:D500 O(1) VLOOKUP / XLOOKUP / INDEX
    let mut cache = BTreeMap::new();
    for r in 0..500u32 {
        cache.insert((0, r, 0), Value::Text(format!("EXT-{r:04}").into()));
        cache.insert((0, r, 3), Value::Number((r as f64) * 3.0));
    }
    wb.links.push(ExternalBook {
        index: 1,
        part: "xl/externalLinks/externalLink1.xml".into(),
        target: "ext_book.xlsx".into(),
        abs: None,
        sheets: vec!["Sheet1".into()],
        defined_names: vec![],
        cache: ExternalCache::from_map(cache),
        refreshed: true,
    });
    assert_eq!(
        eval_formula(&wb, 0, "=VLOOKUP(\"EXT-0321\", [1]Sheet1!A1:D500, 4, FALSE)"),
        FValue::number(963.0)
    );
    assert_eq!(
        eval_formula(&wb, 0, "=XLOOKUP(\"EXT-0499\", [1]Sheet1!A1:A500, [1]Sheet1!D1:D500)"),
        FValue::number(1497.0)
    );
    assert_eq!(
        eval_formula(&wb, 0, "=INDEX([1]Sheet1!A1:D500, 322, 4)"),
        FValue::number(963.0)
    );
}
