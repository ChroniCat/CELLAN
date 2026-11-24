use std::sync::{Arc, atomic::AtomicBool};
use sheetcore::{
    Cell, Sheet, StringPool, Value, Workbook,
    cells::{self, Shift as CellShift},
    cf::{self, CfBlock, CfKind, CfRule, CfSheet, CfStats, Cfvo, CfvoKind, Dxf},
    collate::SortKeyCache,
    data_tools::{self, SortKey, SortMode},
    dv::{self, DataValidation, DvIndex, DvKind},
    formula,
    pivot::{self, Agg, GroupBy, PivotSpec},
    pivot_multi::{self, AxisField, PivotLayout, PivotSpec2, ShowAs, ValueField},
    search::{self, SearchOpts, SearchOrder},
    sort_filter::{self, CmpOp, Criterion, Filter},
    structural::{Structural, apply_structural_wb},
    undo::{Action, CellDelta, UndoStack, snapshot_structure, sparse_rect_keys},
    xlsx,
};

fn num(n: f64) -> Cell {
    Cell {
        value: Value::Number(n),
        formula: None,
    }
}

fn txt(s: &str) -> Cell {
    Cell {
        value: Value::Text(Arc::from(s)),
        formula: None,
    }
}

fn frm(f: &str, cached: Value) -> Cell {
    Cell {
        value: cached,
        formula: Some(Arc::from(f)),
    }
}

#[test]
fn arc_text_sharing_string_pool_and_value_ref() {
    let mut pool = StringPool::with_capacity(8);
    let a = pool.intern("应收账款-华东大区");
    let b = pool.intern("应收账款-华东大区");
    assert!(Arc::ptr_eq(&a, &b));
    assert_eq!(pool.len(), 1);

    let mut sh = Sheet::new("Pool");
    sh.set(0, 0, Cell { value: Value::Text(a.clone()), formula: None });
    let cloned = sh.clone();
    match (sh.value_ref(0, 0).as_ref(), cloned.value_ref(0, 0).as_ref()) {
        (Value::Text(x), Value::Text(y)) => assert!(Arc::ptr_eq(x, y)),
        _ => panic!("expected Value::Text"),
    }
    assert_eq!(sh.value_ref(999, 999).as_ref(), &Value::Empty);
}

#[test]
fn sparse_xfd_store_uniform_styles_journal_and_navigation() {
    let mut sh = Sheet::new("SparseXFD");
    sh.set(0, 0, num(1.0));
    sh.set(500_000, 16_383, num(2.0));
    let (pages, slabs, _) = sh.store_stats();
    assert_eq!(pages, 2);
    assert_eq!(slabs, 2);
    assert!(sh.allocated_bytes() < 512 * 1024);

    // Sparse navigation across huge empty spans
    sh.set(0, 5000, num(3.0));
    assert_eq!(sh.next_occupied_in_row(0, 0, true), Some(5000));
    assert_eq!(sh.next_occupied_in_row(0, 6000, false), Some(5000));
    sh.set(400_000, 0, num(4.0));
    assert_eq!(sh.next_occupied_in_col(0, 0, true), Some(400_000));
    assert_eq!(sh.next_occupied_in_col(0, 500_000, false), Some(400_000));

    // Uniform style pages and bitmap journal
    sh.styles.insert_rect(0, 0, 1023, 3, 5);
    assert_eq!(sh.styles.uniform_page_count(), 16);
    sh.cleared.insert_rect(0, 0, 99_999, 9);
    assert_eq!(sh.cleared.len(), 1_000_000);
    assert!(sh.cleared.payload_bytes() < 512 * 1024);

    // Batch merges + O(log M) merge_at + monotonic revision
    let rev0 = sh.merges_revision();
    sh.merge_ranges((0..100u32).map(|i| (i * 3, 0, i * 3 + 1, 2)));
    assert!(sh.merges_revision() > rev0);
    assert_eq!(sh.merge_at(31, 1), Some((30, 0, 31, 2)));
    assert_eq!(sh.merge_at(32, 1), None);
}

#[test]
fn hidden_intervals_and_span_axis_scale_to_million_rows() {
    let mut sh = Sheet::new("Layout");
    sh.hidden_rows.insert_range(10, 950_009);
    assert_eq!(sh.hidden_rows.intervals(), &[(10, 950_009)]);
    sh.rows.hide_hidden(&sh.hidden_rows);
    assert_eq!(sh.rows.next_visible(10), Some(950_010));
    assert_eq!(sh.rows.prev_visible(950_009), Some(9));

    let y_after = sh.rows.pos(950_010);
    let y_before = sh.rows.pos(10);
    assert!((y_after - y_before).abs() < 1e-4);
    assert_eq!(sh.rows.index_at(y_before + 1.0), 950_010);

    // O(intervals) shift_axis on 950k hidden rows
    sh.insert_rows(5, 3);
    assert_eq!(sh.hidden_rows.intervals(), &[(13, 950_012)]);
}

#[test]
fn pivot_wb_builders_handle_high_cardinality_in_memory_and_roundtrip() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(0, 0, txt("Key"));
    wb.sheets[0].set(0, 1, txt("Bucket"));
    wb.sheets[0].set(0, 2, txt("Amount"));
    for r in 1..=4_000u32 {
        wb.sheets[0].set(r, 0, txt(&format!("SKU-{r:05}")));
        wb.sheets[0].set(r, 1, txt(if r % 2 == 0 { "线上" } else { "线下" }));
        wb.sheets[0].set(r, 2, num(r as f64));
    }

    let spec1 = PivotSpec {
        source: (0, 0, 4_000, 2),
        row_field: 0,
        col_field: Some(1),
        value_field: 2,
        agg: Agg::Sum,
        group: GroupBy::None,
        exclude: Vec::new(),
    };
    let res1 = pivot::compute(&wb.sheets[0], &spec1);
    assert_eq!(res1.headers.len(), 4);
    let built1 = sheetcore::pivot_parts::build_wb(
        &wb,
        "Sheet1",
        &spec1,
        &res1,
        (0, 5, 4001, 8),
        "求和",
        1,
        "PivotTable1",
        "sheetcore",
    )
    .unwrap();
    assert!(built1.0.contains("recordCount=\"4000\""));

    let spec2 = PivotSpec2 {
        source: (0, 0, 4_000, 2),
        rows: vec![AxisField::plain(0)],
        cols: vec![AxisField::plain(1)],
        values: vec![ValueField {
            col: 2,
            agg: Agg::Sum,
            label: Some("金额".into()),
            show_as: ShowAs::Normal,
            base: None,
        }],
        filters: Vec::new(),
        layout: PivotLayout {
            tabular: false,
            subtotals: false,
            grand_totals: true,
            ..Default::default()
        },
    };
    let res2 = pivot_multi::compute(&wb.sheets[0], &spec2);
    let built2 = sheetcore::pivot_parts2::build_wb(
        &wb,
        "Sheet1",
        &spec2,
        &res2,
        (0, 10, 4002, 13),
        2,
        "PivotTable2",
        "sheetcore",
    )
    .unwrap();
    assert!(built2.0.contains("recordCount=\"4000\""));

    // Verify full save pipeline uses in-memory workbook cleanly
    wb.pivots_pending.push((0, spec1, res1, (0, 5, 4001, 8), "求和".into()));
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("pivot-fast.xlsx");
    xlsx::save_atomic(&path, &wb).unwrap();
    let reloaded = xlsx::load(&path).unwrap();
    assert_eq!(reloaded.sheets[0].value(1, 2), Value::Number(1.0));
}

#[test]
fn move_rows_sparse_cell_shift_and_cow_batch_undo() {
    // 1) move_rows near top of a 100k-row sheet leaves tail rows intact and fast
    let mut sh = Sheet::new("Move");
    sh.set(1, 0, txt("Row1"));
    sh.set(2, 0, txt("Row2"));
    sh.set(3, 0, txt("Row3"));
    sh.set(90_000, 0, txt("Tail"));
    sh.move_rows(1, 1, 4); // move Row1 below Row3 -> Row2, Row3, Row1
    assert_eq!(sh.value_ref(1, 0).as_str(), Some("Row2"));
    assert_eq!(sh.value_ref(2, 0).as_str(), Some("Row3"));
    assert_eq!(sh.value_ref(3, 0).as_str(), Some("Row1"));
    assert_eq!(sh.value_ref(90_000, 0).as_str(), Some("Tail"));

    // 2) Sparse cells::insert / delete with distant occupied corner
    cells::insert(&mut sh, (1, 0, 1, 0), CellShift::Vertical, 2);
    assert_eq!(sh.value_ref(3, 0).as_str(), Some("Row2"));
    assert_eq!(sh.value_ref(90_002, 0).as_str(), Some("Tail"));
    cells::delete(&mut sh, (1, 0, 1, 0), CellShift::Vertical, 2);
    assert_eq!(sh.value_ref(1, 0).as_str(), Some("Row2"));
    assert_eq!(sh.value_ref(90_000, 0).as_str(), Some("Tail"));

    // 3) sparse_rect_keys uses iter_rect without scanning outside bounds
    let keys = sparse_rect_keys(&sh, 0, 0, 1000, 1000);
    assert_eq!(keys, vec![(1, 0), (2, 0), (3, 0)]);

    // 4) Large batch (> 16,384 cells) auto-compacts into Entry::CowBatch and honours guard
    let mut wb = Workbook::new_blank();
    let mut deltas = Vec::with_capacity(20_000);
    for r in 0..20_000u32 {
        wb.sheets[0].set(r, 0, num(r as f64));
        wb.sheets[0].set_style(r, 0, 3);
        deltas.push(CellDelta {
            sheet: 0,
            row: r,
            col: 0,
            before: Cell::default(),
            after: num(r as f64),
            before_style: 0,
            after_style: 3,
        });
    }
    let mut stack = UndoStack::new();
    stack.push(Action {
        label: "大批量写入".into(),
        deltas,
        focus: (0, 0, 0),
        snap_before: None,
        snap_after: None,
        by_site: 0,
        shapes: None,
        pictures: None,
        merges: None,
        refresh_sizes: None,
    });
    // Simulate peer modification on row 5
    wb.sheets[0].set(5, 0, num(999_999.0));
    let undone = stack.undo(&mut wb.sheets, |_, _, _, _| true).unwrap();
    assert_eq!(undone.skipped, 1);
    assert_eq!(wb.sheets[0].value(5, 0), Value::Number(999_999.0));
    assert_eq!(wb.sheets[0].value(0, 0), Value::Empty);
    assert_eq!(wb.sheets[0].style_of(0, 0), 0);

    let redone = stack.redo(&mut wb.sheets, |_, _, _, _| true).unwrap();
    assert_eq!(redone.touched.len(), 19_999);
    assert_eq!(wb.sheets[0].value(19_999, 0), Value::Number(19_999.0));
    assert_eq!(wb.sheets[0].style_of(19_999, 0), 3);
    assert_eq!(wb.sheets[0].value(5, 0), Value::Number(999_999.0));

    // Direct push_cow_batch from before/after Sheet CoW snapshots (including row_styles / col_styles!)
    let before_snap = wb.sheets[0].clone();
    wb.sheets[0].set(100, 2, txt("CoW直接快照"));
    wb.sheets[0].set_row_style(10, 7);
    wb.sheets[0].set_col_style(4, 9);
    let after_snap = wb.sheets[0].clone();
    stack.push_cow_batch("CoW快照编辑", 0, &before_snap, &after_snap, (0, 100, 2));
    stack.undo(&mut wb.sheets, |_, _, _, _| true).unwrap();
    assert_eq!(wb.sheets[0].value(100, 2), Value::Empty);
    assert_eq!(wb.sheets[0].row_styles.get(&10), None);
    assert_eq!(wb.sheets[0].col_styles.get(&4), None);
    stack.redo(&mut wb.sheets, |_, _, _, _| true).unwrap();
    assert_eq!(wb.sheets[0].value_ref(100, 2).as_str(), Some("CoW直接快照"));
    assert_eq!(wb.sheets[0].row_styles.get(&10), Some(&7));
    assert_eq!(wb.sheets[0].col_styles.get(&4), Some(&9));

    // Structural undo with foreign formula
    wb.sheets.push(Sheet::new("Summary"));
    wb.sheets[1].set(0, 0, frm("=Sheet1!A2", Value::Number(1.0)));
    let op = Structural::InsertRows { at: 1, n: 2 };
    let s_before = snapshot_structure(&wb, 0, &op);
    apply_structural_wb(&mut wb, 0, &op);
    let s_after = s_before.capture_after(&wb);
    stack.push_structure(op.label(), s_before, s_after, (0, 1, 0));
    assert_eq!(wb.sheets[1].get(0, 0).unwrap().formula.as_deref(), Some("=Sheet1!A4"));
    stack.undo_workbook(&mut wb, |_, _, _, _| true).unwrap();
    assert_eq!(wb.sheets[1].get(0, 0).unwrap().formula.as_deref(), Some("=Sheet1!A2"));
}

#[test]
fn current_region_filter_sort_cf_dv_search_and_dedup_end_to_end() {
    let mut wb = Workbook::new_blank();
    {
        let sh = &mut wb.sheets[0];
        // Data island A1:C5 + distant corner XFD500000
        sh.set(0, 0, txt("部门"));
        sh.set(0, 1, txt("姓名"));
        sh.set(0, 2, txt("业绩"));
        let rows = [
            ("研发", "张三", 200.0),
            ("销售", "李四", 500.0),
            ("研发", "李四", 300.0),
            ("研发", "张三", 200.0),
        ];
        for (i, (dept, name, score)) in rows.iter().enumerate() {
            let r = (i + 1) as u32;
            sh.set(r, 0, txt(dept));
            sh.set(r, 1, txt(name));
            sh.set(r, 2, num(*score));
        }
        sh.set(500_000, 16_383, num(1.0));

        // 1) Bitmap current_region ignores distant XFD500000 and expands A1:C5 in O(occupied_pages)
        assert_eq!(sort_filter::current_region(sh, 2, 1), (0, 0, 4, 2));

        // 2) Native Value Filter + hidden_row_intervals
        let mut filter = Filter::new(0, 0, 4, 2);
        filter.criteria.insert(2, Criterion::new(CmpOp::Ge, "300"));
        let hid_intervals = filter.hidden_row_intervals(sh);
        assert!(hid_intervals.contains(&1));
        assert!(!hid_intervals.contains(&2));
        assert!(!hid_intervals.contains(&3));
        assert!(hid_intervals.contains(&4));

        // 3) Streaming hash remove_duplicates preserves first occurrence
        assert_eq!(data_tools::count_duplicates(sh, (0, 0, 4, 2), &[], true), 1);
        assert_eq!(data_tools::remove_duplicates(sh, (0, 0, 4, 2), &[], true), 1);
        assert_eq!(sh.value(4, 0), Value::Empty);

        // 4) SortKeyCache + MSD multi_sort (dept Pinyin asc, then score desc)
        let mut cache = SortKeyCache::new();
        assert_eq!(cache.get_or_compute("张三").to_vec(), sheetcore::collate::sort_key("张三"));
        data_tools::sort_multi(
            sh,
            (0, 0, 3, 2),
            &[
                SortKey { col: 0, asc: true, ignore_case: true, mode: SortMode::Pinyin },
                SortKey { col: 2, asc: false, ignore_case: true, mode: SortMode::Default },
            ],
            true,
        );
        // 销售 (xiao) < 研发 (yan) in Pinyin! So 销售 comes first, then 研发 300, then 研发 200!
        assert_eq!(sh.value_ref(1, 0).as_str(), Some("销售"));
        assert_eq!(sh.value(2, 2), Value::Number(300.0));
        assert_eq!(sh.value(3, 2), Value::Number(200.0));

        // 5) Incremental CfStats + CompiledShiftFormula
        sh.cf = CfSheet {
            blocks: vec![CfBlock {
                ranges: vec![(1, 2, 3, 2)],
                rules: vec![CfRule {
                    kind: CfKind::Scale {
                        points: vec![
                            (Cfvo { kind: CfvoKind::Min, val: 0.0, formula: String::new() }, [255, 0, 0]),
                            (Cfvo { kind: CfvoKind::Max, val: 0.0, formula: String::new() }, [0, 255, 0]),
                        ],
                    },
                    stop: false,
                    priority: 1,
                }],
            }],
            dxfs: vec![Dxf::default()],
            unknown_ext: false,
            dxf_unsupported: false,
        };
        let mut stats = CfStats::default();
        assert!(cf::refresh_stats(sh, &mut stats));
        assert_eq!(stats.ranges.get(&0), Some(&(200.0, 500.0)));
        assert!(!cf::refresh_stats(sh, &mut stats));
        sh.set(3, 2, num(100.0));
        assert!(cf::refresh_stats(sh, &mut stats));
        assert_eq!(stats.ranges.get(&0), Some(&(100.0, 500.0)));

        assert_eq!(cf::shift_formula("=$A1+B$2+1:2+[1:3]+\"A1\"", 3, 2), "=$A4+D$2+4:5+[1:3]+\"A1\"");

        // 6) DvIndex + revision-safe cached validate_wb
        let rule = DataValidation {
            ranges: vec![(1, 0, 100, 0)],
            kind: DvKind::List,
            f1: "=$A$2:$A$4".into(),
            ..Default::default()
        };
        sh.dv.push(rule);
    }

    let dv_idx = DvIndex::new(&wb.sheets[0].dv);
    let found = dv_idx.find(&wb.sheets[0].dv, 50, 0).unwrap().clone();
    assert!(dv::validate_wb(&wb, 0, &found, &Value::Text(Arc::from("研发")), 50, 0, |_, _, _| formula::FValue::Empty));
    assert!(!dv::validate_wb(&wb, 0, &found, &Value::Text(Arc::from("财务")), 50, 0, |_, _, _| formula::FValue::Empty));
    // Mutating the referenced range invalidates the cached set via CellVersion (including across Undo/fork with equal revision!)
    let baseline_sheet = wb.sheets[0].clone();
    let baseline_rev = baseline_sheet.cell_revision();
    wb.sheets[0].set(3, 0, txt("财务"));
    assert_eq!(wb.sheets[0].cell_revision(), baseline_rev + 1);
    assert!(dv::validate_wb(&wb, 0, &found, &Value::Text(Arc::from("财务")), 50, 0, |_, _, _| formula::FValue::Empty));
    // Simulate Undo restoring `baseline_sheet` (forking lineage at `baseline_rev`), then editing to a new value at the SAME `baseline_rev + 1`!
    wb.sheets[0] = baseline_sheet;
    wb.sheets[0].set(3, 0, txt("法务"));
    assert_eq!(wb.sheets[0].cell_revision(), baseline_rev + 1);
    assert!(!dv::validate_wb(&wb, 0, &found, &Value::Text(Arc::from("财务")), 50, 0, |_, _, _| formula::FValue::Empty));
    assert!(dv::validate_wb(&wb, 0, &found, &Value::Text(Arc::from("法务")), 50, 0, |_, _, _| formula::FValue::Empty));

    // Case-insensitive cross-sheet list validation (`=sheet1!$A$2:$A$4`)
    wb.sheets.push(Sheet::new("Entry"));
    let cross_rule = DataValidation {
        ranges: vec![(0, 0, 10, 0)],
        kind: DvKind::List,
        f1: "=sheet1!$A$2:$A$4".into(),
        ..Default::default()
    };
    assert!(dv::validate_wb(&wb, 1, &cross_rule, &Value::Text(Arc::from("法务")), 0, 0, |_, _, _| formula::FValue::Empty));

    // 7) Zero-alloc & parallel search (including >32,768 cells path)
    let mut big_wb = Workbook::new_blank();
    for r in 0..40_000u32 {
        big_wb.sheets[0].set(r, 0, txt(if r == 39_999 { "目标命中-Target" } else { "普通数据行" }));
    }
    let cancel = AtomicBool::new(false);
    let needle = search::normalize_needle("target", &SearchOpts::default());
    let hits = search::scoped_hits(&big_wb, Some(0), &[], &needle, &SearchOpts::default(), SearchOrder::RowMajor, &cancel);
    assert_eq!(hits.len(), 1);
    assert_eq!((hits[0].row, hits[0].col), (39_999, 0));

    let cjk_hits = search::hits_in_rect(&big_wb, 0, (39_990, 0, 39_999, 0), "目标命中", &SearchOpts::default(), SearchOrder::RowMajor);
    assert_eq!(cjk_hits.len(), 1);
}
