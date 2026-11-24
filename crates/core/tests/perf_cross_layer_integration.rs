//! End-to-end verification of the 4 cross-layer integration contracts across
//! `storage-mem` × `io-file` × `formula-calc` × `ui-render` × `collab-ai`.

use sheetcore::formula::{self, FValue, deps::Deps};
use sheetcore::layout::{Axis, DEFAULT_ROW_H};
use sheetcore::merges::MergeIndex;
use sheetcore::model::{Cell, Sheet, Value, Workbook};
use sheetcore::pivot::{self, Agg, GroupBy, PivotSpec};
use sheetcore::sort_filter::{CmpOp, Criterion, Filter};
use sheetcore::structural::Structural;
use sheetcore::sync::ValueWire;
use std::sync::{Arc, atomic::AtomicBool};

/// [跨层-1] Verify true `Arc::ptr_eq` zero-copy string sharing across XLSX SST load ->
/// `Value::Text(Arc<str>)` -> `FValue::Text(Arc<str>)` -> `ValueWire::Text(Arc<str>)` -> `Value::Text`.
#[test]
fn cross_layer_1_arc_str_zero_copy_across_io_storage_formula_and_collab() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sst_zero_copy.xlsx");

    let mut wb = Workbook::new_blank();
    {
        let sh = wb.sheet_mut(0);
        for r in 0..20_000u32 {
            sh.set(
                r,
                0,
                Cell {
                    value: Value::Text(if r % 2 == 0 { "华东大区-上海".into() } else { "华北大区-北京".into() }),
                    formula: None,
                },
            );
        }
    }
    sheetcore::xlsx::save_atomic(&path, &wb).unwrap();

    let loaded = sheetcore::xlsx::load(&path).unwrap();
    let sh = loaded.sheet(0);
    let v0 = sh.value_ref(0, 0);
    let Value::Text(ref a0) = *v0 else {
        panic!("expected text at (0,0)");
    };
    let v2 = sh.value_ref(2, 0);
    let Value::Text(ref a2) = *v2 else {
        panic!("expected text at (2,0)");
    };
    assert!(
        Arc::ptr_eq(a0, a2),
        "SST cells sharing a string must share the exact same Arc<str> allocation"
    );

    let fv = formula::to_fvalue(&Value::Text(Arc::clone(a0)));
    let FValue::Text(ref f_arc) = fv else {
        panic!("expected FValue::Text");
    };
    assert!(Arc::ptr_eq(a0, f_arc), "to_fvalue must Arc::clone without heap allocation");

    let back_val = formula::from_fvalue(&fv);
    let Value::Text(ref back_arc) = back_val else {
        panic!("expected Value::Text");
    };
    assert!(Arc::ptr_eq(a0, back_arc), "from_fvalue must Arc::clone without heap allocation");

    let wire = ValueWire::from(&back_val);
    let ValueWire::Text(ref wire_arc) = wire else {
        panic!("expected ValueWire::Text");
    };
    assert!(Arc::ptr_eq(a0, wire_arc), "ValueWire::from(&Value) must Arc::clone without heap allocation");

    let from_wire = Value::from(&wire);
    let Value::Text(ref final_arc) = from_wire else {
        panic!("expected Value::Text");
    };
    assert!(Arc::ptr_eq(a0, final_arc), "Value::from(&ValueWire) must Arc::clone without heap allocation");
}

/// [跨层-2] Verify fast-opened workbooks with cached `<v>` values certify `computed_baseline`
/// via `Deps::build_with_cached_baseline(&wb, true)` so the very first row insertion hits
/// `recalc_structural` instead of forcing a full-workbook recalculation.
#[test]
fn cross_layer_2_fast_open_cached_baseline_enables_first_edit_structural_recalc() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fast_open_baseline.xlsx");

    let mut wb = Workbook::new_blank();
    {
        let sh = wb.sheet_mut(0);
        sh.set(0, 0, Cell { value: Value::Text("Key".into()), formula: None });
        sh.set(0, 1, Cell { value: Value::Text("Val".into()), formula: None });
        for r in 1..=50u32 {
            sh.set(r, 0, Cell { value: Value::Number(r as f64), formula: None });
            sh.set(r, 1, Cell { value: Value::Number((r * 10) as f64), formula: None });
            sh.set(
                r,
                2,
                Cell {
                    value: Value::Empty,
                    formula: Some(format!("=IF(A{}>0, VLOOKUP(A{}, $A$2:$B$51, 2, FALSE), 0)", r + 1, r + 1).into()),
                },
            );
        }
    }
    formula::recalc_all(&mut wb);
    sheetcore::xlsx::save_atomic(&path, &wb).unwrap();

    // Simulate big-file fast open (`load_diag` without `recalc_all`)
    let (mut fast_wb, diag) = sheetcore::xlsx::load_diag(&path).unwrap();
    assert!(diag.is_complete() && !fast_wb.load_incomplete);

    // Poison an early cached value with a sentinel to prove `recalc_structural` preserves
    // unaffected cached cells before `shift.at` rather than running full-workbook recalc!
    fast_wb.sheet_mut(0).set(
        1,
        2,
        Cell {
            value: Value::Number(999_999.0),
            formula: Some("=IF(A2>0, VLOOKUP(A2, $A$2:$B$51, 2, FALSE), 0)".into()),
        },
    );

    let deps = Deps::build_with_cached_baseline(&fast_wb, true);
    assert!(
        deps.supports_structural(&fast_wb),
        "fast-opened workbook with complete <v> caches must support local structural recalc"
    );

    let op = Structural::InsertRows { at: 60, n: 1 };
    sheetcore::structural::apply_structural_wb(&mut fast_wb, 0, &op);
    let next_deps = deps
        .recalc_structural(&mut fast_wb, 0, &op, &AtomicBool::new(false))
        .expect("recalc_structural should succeed on first structural edit");
    assert!(next_deps.supports_structural(&fast_wb));
    assert_eq!(
        fast_wb.sheet(0).value(1, 2),
        Value::Number(999_999.0),
        "unaffected IF+VLOOKUP formula strictly before insertion point must keep its cached value"
    );
}

/// [跨层-3] Verify `xlsx_fresh` streaming save (`iter_cells_with_explicit_style` + `FreshSst`)
/// combined with in-memory session pivot attachment (`attach_for_session_wb`) round-trips cleanly.
#[test]
fn cross_layer_3_fresh_stream_styles_and_in_memory_pivot_save_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("fresh_with_pivot.xlsx");

    let mut wb = Workbook::new_blank();
    wb.sheets.push(Sheet::new("PivotOut"));
    let mut bold = wb.styles.get(0).clone();
    bold.bold = true;
    let bold_id = wb.styles.intern(bold);

    {
        let sh = wb.sheet_mut(0);
        sh.set(0, 0, Cell { value: Value::Text("Region".into()), formula: None });
        sh.set(0, 1, Cell { value: Value::Text("OrderCode".into()), formula: None });
        sh.set(0, 2, Cell { value: Value::Text("Amount".into()), formula: None });
        // >= 16,384 cells triggers `xlsx_fresh::write` (`iter_cells_with_explicit_style`)
        for r in 1..=6_000u32 {
            sh.set(
                r,
                0,
                Cell {
                    value: Value::Text(if r % 2 == 0 { "East".into() } else { "West".into() }),
                    formula: None,
                },
            );
            sh.set(
                r,
                1,
                Cell {
                    value: Value::Text(format!("ORD-{r:06}").into()),
                    formula: None,
                },
            );
            sh.set(r, 2, Cell { value: Value::Number(10.0), formula: None });
        }
        sh.set_style_rect(1, 0, 512, 0, bold_id);
    }

    let spec = PivotSpec {
        source: (0, 0, 6_000, 2),
        row_field: 0,
        col_field: None,
        value_field: 2,
        agg: Agg::Sum,
        group: GroupBy::None,
        exclude: Vec::new(),
    };
    let res = pivot::compute(wb.sheet(0), &spec);
    let out_rect = pivot::write_to(wb.sheet_mut(1), (0, 0), &res);
    wb.pivots_pending
        .push((1, spec, res, out_rect, "Amount".to_string()));

    sheetcore::xlsx::save_atomic(&path, &wb).unwrap();

    let reloaded = sheetcore::xlsx::load(&path).unwrap();
    assert_eq!(reloaded.sheet(0).value(1, 0), Value::Text("West".into()));
    assert_eq!(reloaded.sheet(0).value(6_000, 1), Value::Text("ORD-006000".into()));
    let reloaded_style = reloaded.styles.get(reloaded.sheet(0).style_of(256, 0));
    assert!(reloaded_style.bold, "Uniform style page must survive xlsx_fresh iter_cells_with_explicit_style");

    let zip_file = std::fs::File::open(&path).unwrap();
    let mut zip = zip::ZipArchive::new(zip_file).unwrap();
    assert!(zip.by_name("xl/sharedStrings.xml").is_ok(), "FreshSst must emit sharedStrings.xml");
    assert!(zip.by_name("xl/pivotTables/pivotTable1.xml").is_ok(), "in-memory pivot attachment must emit pivotTable1.xml");
}

/// [跨层-4] Verify interval `HiddenIndices` / `Axis` (zero-size revision, 950k hidden rows)
/// and `Sheet::merges_revision()` + `MergeIndex::refresh_merges` + `display::merge_span`.
#[test]
fn cross_layer_4_interval_hidden_axis_and_merge_revisions() {
    let mut sh = Sheet::new("Layout");
    sh.set(0, 0, Cell { value: Value::Text("H".into()), formula: None });
    sh.set(1, 0, Cell { value: Value::Number(1.0), formula: None });
    sh.set(950_000, 0, Cell { value: Value::Number(100.0), formula: None });

    let mut f = Filter::new(0, 0, 950_000, 0);
    f.criteria.insert(0, Criterion::new(CmpOp::Gt, "50"));
    let hidden = f.hidden_row_intervals(&sh);
    assert_eq!(hidden.len(), 949_999);
    assert!(hidden.intervals().len() <= 2, "950k contiguous hidden rows must coalesce into <=2 intervals");

    let mut axis = Axis::new(DEFAULT_ROW_H);
    axis.hide_hidden(&hidden);
    assert!(axis.spans().len() <= 2);
    assert_eq!(axis.next_visible(1), Some(950_000));
    assert_eq!(axis.prev_visible(949_999), Some(0));

    let zero_rev_before = axis.zero_size_revision();
    axis.set(0, 36.0);
    assert_eq!(
        axis.zero_size_revision(),
        zero_rev_before,
        "resizing a visible row must not bump zero_size_revision"
    );

    let m_rev0 = sh.merges_revision();
    sh.merge_range(2, 1, 5, 3);
    assert_ne!(sh.merges_revision(), m_rev0);
    assert_eq!(sheetcore::display::merge_span(&sh, 4, 2), Some((2, 1, 5, 3)));
    let mut idx = MergeIndex::default();
    idx.refresh_merges(&sh.merges);
    assert_eq!(idx.at(5, 3), Some((2, 1, 5, 3)));
}
