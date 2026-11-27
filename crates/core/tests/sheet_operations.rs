use sheetcore::{
    formula, refs::rewrite_sheet, sheet_ops::{delete_sheet, insert_sheet, move_sheet, rename_sheet},
    structural::{apply_structural_wb, Structural}, xlsx, Cell, Sheet, Value, Workbook,
};

fn number(n: f64) -> Cell { Cell { value: Value::Number(n), formula: None } }
fn formula_cell(f: &str) -> Cell { Cell { value: Value::Empty, formula: Some(f.into()) } }

fn workbook() -> Workbook {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(0, 0, number(10.0));
    wb.sheets.push(Sheet::new("Result"));
    wb.sheets[1].set(0, 0, number(20.0));
    wb.sheets[1].set(0, 1, formula_cell("=Rate"));
    wb.sheets[1].set(0, 2, formula_cell("=Sheet1!A1"));
    wb.names.push(("Rate".into(), Some(1), "$A$1".into()));
    wb.names.push(("Linked".into(), None, "Result!$A$1".into()));
    wb.names_dirty = true;
    wb
}

struct Fixture(std::path::PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let next = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("sheet_operations_{}_{next}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn path(&self) -> std::path::PathBuf { self.0.join("book.xlsx") }
}
impl Drop for Fixture {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
}

#[test]
fn qualifier_rewrite_preserves_strings_external_links_and_table_columns() {
    let f = "=Old!$A$1+'old'!B2+Old!Local+Older!A1+\"Old!A1\"+[Book.xlsx]Old!A1+'[Book.xlsx]Old'!A1+T[Old!A1]+A1";
    assert_eq!(rewrite_sheet(f, "Old", Some("O'Brien 新")),
        "='O''Brien 新'!$A$1+'O''Brien 新'!B2+'O''Brien 新'!Local+Older!A1+\"Old!A1\"+[Book.xlsx]Old!A1+'[Book.xlsx]Old'!A1+T[Old!A1]+A1");
    assert_eq!(rewrite_sheet("='O''Brien 新'!$A$1+'O''Brien 新'!A:C+'O''Brien 新'!1:3", "O'Brien 新", Some("New")),
        "='New'!$A$1+'New'!A:C+'New'!1:3");
    // C11.4：三维端点现在属于支持的语法，改名必须同时更新起点和终点；
    // 其余位置（表名中间冒号）仍不能误当端点。
    assert_eq!(
        rewrite_sheet("=Old:Other!A1+Other:Old!A1", "Old", Some("New")),
        "=New:Other!A1+Other:New!A1"
    );
    assert_eq!(rewrite_sheet("=#REF!+REF!A1", "REF", Some("New")), "=#REF!+'New'!A1");
}

#[test]
fn deleting_sheet_consumes_entire_reference_and_prevents_name_reuse() {
    assert_eq!(rewrite_sheet("=Old!A1+SUM(Old!$A$1:$B2)+Old!A:C+Old!1:3+Old!Local+Old!A1#+Old!#REF!", "Old", None),
        "=#REF!+SUM(#REF!)+#REF!+#REF!+#REF!+#REF!+#REF!");
    let mut wb = workbook();
    wb.names.push(("Gone".into(), Some(0), "$A$1".into()));
    wb.names.push(("GlobalOld".into(), None, "Sheet1!$A$1".into()));
    assert!(delete_sheet(&mut wb, 0));
    assert_eq!(wb.names.iter().find(|(n, ..)| n == "Rate").unwrap().1, Some(0));
    assert!(!wb.names.iter().any(|(n, ..)| n == "Gone"));
    assert_eq!(wb.names.iter().find(|(n, ..)| n == "GlobalOld").unwrap().2, "#REF!");
    assert_eq!(wb.sheets[0].get(0, 2).unwrap().formula.as_deref(), Some("=#REF!"));
    assert!(insert_sheet(&mut wb, 1, Sheet::new("Sheet1")));
    wb.sheets[1].set(0, 0, number(99.0));
    assert_eq!(formula::eval_cell(&wb, 0, 0, 2).display(), "#REF!");
}

#[test]
fn renamed_formula_evaluates_and_survives_repeated_save() {
    let fixture = Fixture::new();
    let path = fixture.path();
    xlsx::save_atomic(&path, &workbook()).unwrap();
    let mut wb = xlsx::load(&path).unwrap();
    assert!(rename_sheet(&mut wb, 0, "O'Brien 新"));
    assert!(wb.sheets[1].edited);
    for _ in 0..2 {
        formula::recalc_all(&mut wb);
        assert_eq!(wb.sheets[1].value(0, 2), Value::Number(10.0));
        xlsx::save_atomic(&path, &wb).unwrap();
        let back = xlsx::load(&path).unwrap();
        assert_eq!(back.sheets.len(), 2);
        assert_eq!(back.sheets[1].get(0, 2).unwrap().formula.as_deref(), Some("='O''Brien 新'!A1"));
        assert_eq!(formula::eval_cell(&back, 1, 0, 2), formula::FValue::Number(10.0));
    }
}

#[test]
fn moved_local_names_follow_their_sheet_through_structure_edits_and_save() {
    let fixture = Fixture::new();
    let path = fixture.path();
    xlsx::save_atomic(&path, &workbook()).unwrap();
    let mut wb = xlsx::load(&path).unwrap();
    assert!(move_sheet(&mut wb, 0, 1));
    assert_eq!(formula::eval_cell(&wb, 0, 0, 1), formula::FValue::Number(20.0));
    apply_structural_wb(&mut wb, 0, &Structural::InsertRows { at: 0, n: 1 });
    for _ in 0..2 {
        xlsx::save_atomic(&path, &wb).unwrap();
        let back = xlsx::load(&path).unwrap();
        let rate = back.names.iter().find(|(n, ..)| n == "Rate").unwrap();
        assert_eq!(rate.1, Some(0));
        assert_eq!(rate.2, "$A$2");
        assert_eq!(formula::eval_cell(&back, 0, 1, 1), formula::FValue::Number(20.0));
    }
}

#[test]
fn deleted_sheet_remaps_surviving_names_before_structure_edits_and_save() {
    let fixture = Fixture::new();
    let path = fixture.path();
    let mut seed = workbook();
    seed.names.push(("Gone".into(), Some(0), "$A$1".into()));
    xlsx::save_atomic(&path, &seed).unwrap();
    let mut wb = xlsx::load(&path).unwrap();
    assert!(delete_sheet(&mut wb, 0));
    apply_structural_wb(&mut wb, 0, &Structural::InsertRows { at: 0, n: 1 });
    xlsx::save_atomic(&path, &wb).unwrap();
    let back = xlsx::load(&path).unwrap();
    assert_eq!(back.sheets.len(), 1);
    assert!(!back.names.iter().any(|(n, ..)| n == "Gone"));
    assert_eq!(formula::eval_cell(&back, 0, 1, 1), formula::FValue::Number(20.0));
    assert_eq!(formula::eval_cell(&back, 0, 1, 2).display(), "#REF!");
}

#[test]
fn metadata_references_and_pending_pivots_follow_sheet_identity() {
    use sheetcore::{cf::{CfBlock, CfKind, CfRule}, dv::DataValidation, external::{Source, Kind}, pivot::{PivotSpec, PivotResult, Agg}};
    let mut wb = workbook();
    wb.sheets[1].cf.blocks.push(CfBlock {
        ranges: vec![(0, 0, 0, 0)],
        rules: vec![CfRule { kind: CfKind::Expr { formula: "Sheet1!A1>0".into(), dxf: 0 }, stop: false, priority: 1 }],
    });
    wb.sheets[1].dv.push(DataValidation { f1: "Sheet1!$A$1".into(), ..Default::default() });
    let chart = sheetcore::chart_write::chart_from_selection(&wb.sheets[0], "Sheet1", (0, 0, 1, 1), "test", sheetcore::charts::ChartKind::Column);
    wb.sheets[1].charts.push(chart);
    wb.sources.push(Source { name: "Source".into(), kind: Kind::Internal { sheet: "Sheet1".into(), rect: (0, 0, 1, 1) }, at: (0, 0), target_sheet: None, rows: 2, cols: 2, csv_opts: None });
    wb.pivot_shifts.push(("Sheet1".into(), 0, 0, 1, false));
    let spec = PivotSpec { source: (0, 0, 1, 1), row_field: 0, col_field: None, value_field: 1, agg: Agg::Sum, group: sheetcore::pivot::GroupBy::None, exclude: Vec::new() };
    wb.pivots_pending.push((0, spec.clone(), PivotResult::default(), (2, 0, 3, 1), "First".into()));
    wb.pivots_pending.push((1, spec, PivotResult::default(), (2, 0, 3, 1), "Second".into()));
    assert!(rename_sheet(&mut wb, 0, "Renamed"));
    assert!(wb.sheets[1].cf_dirty && wb.sheets[1].dv_dirty && wb.sheets[1].charts_dirty);
    assert_eq!(wb.sheets[1].dv[0].f1, "'Renamed'!$A$1");
    assert_eq!(wb.sheets[1].charts[0].series[0].val_ref, "'Renamed'!$B$2:$B$2");
    assert_eq!(wb.pivot_shifts[0].0, "Renamed");
    assert!(matches!(&wb.sources[0].kind, Kind::Internal { sheet, .. } if sheet == "Renamed"));
    assert!(insert_sheet(&mut wb, 0, Sheet::new("Inserted")));
    assert_eq!(wb.pivots_pending.iter().map(|p| p.0).collect::<Vec<_>>(), vec![1, 2]);
    assert_eq!(wb.names[0].1, Some(2));
    assert!(move_sheet(&mut wb, 1, 2));
    assert_eq!(wb.pivots_pending.iter().map(|p| p.0).collect::<Vec<_>>(), vec![2, 1]);
    assert!(delete_sheet(&mut wb, 2));
    assert_eq!(wb.pivots_pending.len(), 1);
    assert_eq!(wb.pivots_pending[0].0, 1);
    assert!(wb.pivot_shifts.is_empty());
    assert!(matches!(&wb.sources[0].kind, Kind::Internal { sheet, .. } if sheet == "#REF!"));
    assert_eq!(wb.sheets[1].charts[0].series[0].val_ref, "#REF!");
}

#[test]
fn invalid_operations_leave_the_workbook_unchanged() {
    let mut wb = workbook();
    wb.dirty = false;
    wb.names_dirty = false;
    for name in ["result", "", "   ", "bad/name", "'quoted'", "12345678901234567890123456789012"] {
        assert!(!rename_sheet(&mut wb, 0, name));
    }
    assert!(!rename_sheet(&mut wb, 2, "Valid"));
    assert!(!move_sheet(&mut wb, 0, 0));
    assert!(!move_sheet(&mut wb, 0, 2));
    assert!(!delete_sheet(&mut wb, 2));
    assert!(!insert_sheet(&mut wb, 0, Sheet::new("RESULT")));
    assert_eq!(wb.names[0].1, Some(1));
    assert!(!wb.dirty && !wb.names_dirty);
    assert!(!delete_sheet(&mut Workbook::new_blank(), 0));
}

#[test]
fn partial_shift_updates_named_ranges_through_undo_redo_and_room_replay() {
    use sheetcore::{cells::Shift, structural::CellShift, undo::{UndoStack, snapshot_cell_shift_if_metadata},
        cell_shift_patch::{CellShiftPatch, CellShiftOverlay}};
    let mut seed = Workbook::new_blank();
    seed.sheets.push(Sheet::new("Report"));
    seed.sheets[0].set(2, 0, number(99.));
    seed.sheets[0].set(3, 0, number(11.));
    seed.names = vec![("Amount".into(), None, "Sheet1!$A$4".into()),
        ("ThisCell".into(), Some(0), "$A$4".into()), ("Other".into(), Some(1), "$A$4".into())];
    seed.sheets[1].set(0, 0, formula_cell("=Amount"));
    let shift = CellShift::new((0,0,0,0), Shift::Vertical, true);
    let mut wb = seed.clone();
    let before = snapshot_cell_shift_if_metadata(&wb, 0, &shift).unwrap();
    assert!(shift.apply(&mut wb, 0));
    assert!(before.cell_shift_changed_metadata(&wb));
    let after = before.capture_after(&wb);
    let mut history = UndoStack::new();
    history.push_structure("insert", before, after, (0,0,0));
    let check = |wb: &Workbook, row| {
        assert_eq!(wb.names[0].2, format!("Sheet1!$A${row}"));
        assert_eq!(wb.names[1].2, format!("$A${row}"));
        assert_eq!(wb.names[2].2, "$A$4");
        assert_eq!(formula::eval_cell(wb, 1, 0, 0), formula::FValue::Number(11.));
    };
    check(&wb, 5);
    assert!(history.undo_workbook(&mut wb, |_,_,_,_|true).is_some());
    check(&wb, 4);
    assert!(history.redo_workbook(&mut wb, |_,_,_,_|true).is_some());
    check(&wb, 5);

    let mut guest = seed.clone();
    guest.names.clear();
    let request = CellShiftPatch::prepare(&guest, 0, shift).unwrap();
    let accepted = request.canonical_forward(&seed).unwrap();
    assert!(accepted.apply(&mut seed, true));
    assert!(accepted.apply_committed(&mut guest, true));
    assert_eq!(guest.names[0].2, "Sheet1!$A$5");
    assert!(CellShiftOverlay::capture(&seed).apply_to(&mut guest));
    check(&guest, 5);
    assert!(accepted.apply(&mut guest, false));
    check(&guest, 4);
    seed.names.push(("Later".into(), None, "Sheet1!A5".into()));
    assert!(!accepted.apply(&mut seed, false));
}

#[test]
fn rename_replays_source_endpoints_independently_and_preserves_sparkline_identity() {
    use sheetcore::{external::{Kind,Source}, sparkline::Sparkline, undo::UndoStack};
    let mut wb = workbook();
    wb.sources.push(Source { name: "_sc_src_1".into(), kind: Kind::Internal {
        sheet:"Sheet1".into(), rect:(0,0,0,0)
    }, target_sheet:Some("Result".into()), at:(2,0), rows:1, cols:1, csv_opts:None });
    let mut same_sheet = wb.sources[0].clone();
    same_sheet.name = "_sc_src_2".into();
    same_sheet.target_sheet = Some("Sheet1".into());
    wb.sources.push(same_sheet);
    wb.sheets[1].sparklines.push(Sparkline { r: 2, c: 2, source_sheet:Some("Sheet1".into()), ..Default::default() });
    wb.sheets[1].sparklines_dirty = true;
    let mut history = UndoStack::new();
    let original = wb.sources.clone();
    assert!(history.rename_sheet(&mut wb, 0, "Raw Data", (0,0,0)));
    let renamed = wb.sources.clone();
    assert_eq!(renamed[0].target_sheet.as_deref(), Some("Result"));
    assert_eq!(renamed[1].target_sheet.as_deref(), Some("Raw Data"));
    assert!(matches!(&renamed[0].kind, Kind::Internal{sheet,..} if sheet == "Raw Data"));
    assert!(history.undo_workbook(&mut wb, |_,_,_,_|true).is_some());
    assert_eq!(wb.sources, original);
    assert_eq!(wb.sheets[1].sparklines[0].source_sheet.as_deref(), Some("Sheet1"));
    assert!(history.redo_workbook(&mut wb, |_,_,_,_|true).is_some());
    assert_eq!(wb.sources, renamed);
    assert!(insert_sheet(&mut wb, 2, Sheet::new("Sheet1")));
    let fixture = Fixture::new();
    xlsx::save_atomic(&fixture.path(), &wb).unwrap();
    let back = xlsx::load(&fixture.path()).unwrap();
    assert_eq!(back.sheets[1].sparklines[0].source_sheet.as_deref(), Some("Raw Data"));
    assert_eq!(back.sources[0].target_sheet.as_deref(), Some("Result"));
    assert!(history.delete_sheet(&mut wb, 0, (0,0,0)));
    assert_eq!(wb.sheets[0].sparklines[0].data_ref("Result"), "#REF!");
    assert!(history.undo_workbook(&mut wb, |_,_,_,_|true).is_some());
    assert_eq!(wb.sheets[1].sparklines[0].source_sheet.as_deref(), Some("Raw Data"));
    assert!(history.redo_workbook(&mut wb, |_,_,_,_|true).is_some());
    assert!(insert_sheet(&mut wb, 1, Sheet::new("#REF!")));
    xlsx::save_atomic(&fixture.path(), &wb).unwrap();
    let back = xlsx::load(&fixture.path()).unwrap();
    assert_eq!(back.sheets[0].sparklines[0].data_ref("Result"), "#REF!");
}
