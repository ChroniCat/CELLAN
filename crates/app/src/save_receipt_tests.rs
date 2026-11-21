use super::*;
use sheetcore::{Cell, Sheet, Value};
use std::time::Duration;

fn wait_for_document_save(tasks: &mut DocumentTasks, wb: &mut Workbook) -> Result<(PathBuf, bool), String> {
    let start = Instant::now();
    loop {
        if let Some(result) = tasks.poll_save(wb) { return result; }
        assert!(start.elapsed() < Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn edit_in_another_sheet_during_save_retires_the_committed_detail() {
    let env = crate::test_env::TestEnv::new("save-while-summary-edited");
    let path = env.path("ledger.xlsx");
    let mut tasks = DocumentTasks::default();
    let mut wb = Workbook::new_blank();
    wb.sheets.push(Sheet::new("Summary"));
    for r in 0..1_000 {
        wb.sheet_mut(0).set(r, 0, Cell { value: Value::Number(r as f64), formula: None });
    }
    wb.sheet_mut(0).remove(0, 0);
    tasks.start_save(&mut wb, path.clone());
    wb.sheet_mut(1).set(0, 0, Cell { value: Value::Number(3.0), formula: None });
    wb.dirty = true;
    wait_for_document_save(&mut tasks, &mut wb).unwrap();
    assert!(wb.dirty && wb.sheet(1).edited);
    assert!(!wb.sheet(0).edited && wb.sheet(0).cleared.is_empty());
    let part = wb.sheet(0).source_part.clone().unwrap();
    let baseline = sheetcore::fixups::read_part_file(&path, &part).unwrap();
    tasks.start_save(&mut wb, path.clone());
    wait_for_document_save(&mut tasks, &mut wb).unwrap();
    assert_eq!(sheetcore::fixups::read_part_file(&path, &part).unwrap(), baseline);
    assert_eq!(sheetcore::xlsx::load(&path).unwrap().sheet(1).value(0, 0), Value::Number(3.0));
}

#[test]
fn f9_completing_after_save_keeps_the_receipt_and_persists_changed_formula_caches() {
    let env = crate::test_env::TestEnv::new("f9-during-save");
    let path = env.path("ledger.xlsx");
    let mut app = crate::SheetApp::new_with_context(&egui::Context::default(), None);
    let number = |n| Cell { value: Value::Number(n), formula: None };
    app.wb.sheets.push(Sheet::new("Summary"));
    app.wb.sheet_mut(0).set(0, 0, number(5.0));
    app.wb.sheet_mut(0).set(1, 0, number(8.0));
    app.wb.sheet_mut(0).remove(1, 0);
    app.wb.sheet_mut(1).set(0, 0, Cell { value: Value::Number(1.0), formula: Some("=Sheet1!A1+2".into()) });

    // Both workers capture the deletion. Consume the save first, while the F9
    // result remains in its channel, independent of worker scheduling.
    app.tasks.start_save(&mut app.wb, path.clone());
    app.recalc_all_async();
    assert!(app.tasks.busy);
    wait_for_document_save(&mut app.tasks, &mut app.wb).unwrap();
    assert_eq!(sheetcore::xlsx::load(&path).unwrap().sheet(1).value(0, 0), Value::Number(1.0));
    assert!(!app.wb.sheet(0).edited && app.wb.sheet(0).cleared.is_empty());

    let start = Instant::now();
    while app.tasks.busy {
        app.poll_open();
        assert!(start.elapsed() < Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(app.wb.path.as_deref(), Some(path.as_path()));
    assert!(!app.wb.sheet(0).edited && app.wb.sheet(0).cleared.is_empty());
    assert_eq!(app.wb.sheet(1).value(0, 0), Value::Number(7.0));

    // A third sheet is the only user edit in this save. The new cache on
    // Summary still needs writing, while the committed detail can be reused.
    app.wb.sheets.push(Sheet::new("Notes"));
    app.wb.sheet_mut(2).set(0, 0, number(12.0));
    app.wb.dirty = true;
    app.tasks.start_save(&mut app.wb, path.clone());
    wait_for_document_save(&mut app.tasks, &mut app.wb).unwrap();
    let back = sheetcore::xlsx::load(&path).unwrap();
    assert_eq!(back.sheet(0).value(1, 0), Value::Empty);
    assert_eq!(back.sheet(1).value(0, 0), Value::Number(7.0));
    assert_eq!(back.sheet(2).value(0, 0), Value::Number(12.0));
}

#[test]
fn repeated_save_preserves_new_style_and_deletion_across_a_failed_retry() {
    let env = crate::test_env::TestEnv::new("repeat-save-edits");
    let path = env.path("ledger.xlsx");
    let mut tasks = DocumentTasks::default();
    let mut wb = Workbook::new_blank();
    let number = |n| Cell { value: Value::Number(n), formula: None };
    wb.sheet_mut(0).set(0, 0, number(17.0));
    wb.sheet_mut(0).set(1, 0, number(20.0));
    let bold = wb.styles.intern(sheetcore::style::CellStyle { bold: true, ..Default::default() });
    tasks.start_save(&mut wb, path.clone());
    wb.sheet_mut(0).set_style(1, 0, bold);
    wb.sheet_mut(0).remove(0, 0);
    wb.dirty = true; // committing a worksheet edit in the desktop application
    wait_for_document_save(&mut tasks, &mut wb).unwrap();
    assert!(wb.dirty && wb.sheet(0).styles_dirty && !wb.sheet(0).cleared.is_empty());
    let first = sheetcore::xlsx::load(&path).unwrap();
    assert_eq!(first.sheet(0).value(0, 0), Value::Number(17.0));

    // An ordinary file as a parent forces a write failure before replacement.
    tasks.start_save(&mut wb, path.join("failed.xlsx"));
    assert!(wait_for_document_save(&mut tasks, &mut wb).is_err());
    assert!(wb.dirty && wb.sheet(0).styles_dirty && !wb.sheet(0).cleared.is_empty());
    assert_eq!(wb.path.as_deref(), Some(path.as_path()));

    tasks.start_save(&mut wb, path.clone());
    wait_for_document_save(&mut tasks, &mut wb).unwrap();
    assert!(!wb.dirty && !wb.sheet(0).edited && !wb.sheet(0).styles_dirty);
    assert!(wb.sheet(0).style_edits.is_empty() && wb.sheet(0).cleared.is_empty());
    let back = sheetcore::xlsx::load(&path).unwrap();
    assert_eq!(back.sheet(0).value(0, 0), Value::Empty);
    assert!(back.styles.get(back.sheet(0).style_of(1, 0)).bold);
}

#[test]
fn saving_with_pending_calculation_uses_the_same_random_sample_on_disk_and_screen() {
    let env = crate::test_env::TestEnv::new("pending-volatile-save");
    let path = env.path("cached.xlsx");
    let mut tasks = DocumentTasks::default();
    tasks.needs_full = true;
    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(0, 0, Cell { value: Value::Empty, formula: Some("=RAND()".into()) });
    tasks.start_save(&mut wb, path.clone());
    let started = Instant::now();
    loop {
        if let Some(result) = tasks.poll_save(&mut wb) {
            assert!(result.unwrap().1);
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!tasks.recalc_pending());
    let live = wb.sheets[0].value(0, 0);
    assert!(matches!(live, Value::Number(_)));
    assert_eq!(sheetcore::xlsx::load(&path).unwrap().sheets[0].value(0, 0), live);
}

#[test]
fn saving_during_pivot_replacement_preserves_only_the_live_pending_attachment() {
    let env = crate::test_env::TestEnv::new("async-pivot");
    let path = env.path("pivot.xlsx");
    let mut tasks = DocumentTasks::default();
    let mut wb = Workbook::new_blank();
    wb.sheets.push(Sheet::new("Keep"));
    for sheet in &mut wb.sheets {
        for (r, c, text) in [(0, 0, "Group"), (0, 1, "Amount"), (1, 0, "A")] {
            sheet.set(r, c, Cell { value: Value::Text(text.into()), formula: None });
        }
        sheet.set(1, 1, Cell { value: Value::Number(7.0), formula: None });
    }
    let spec = sheetcore::pivot::PivotSpec {
        source: (0, 0, 1, 1), row_field: 0, col_field: None,
        value_field: 1, agg: sheetcore::pivot::Agg::Sum,
        group: sheetcore::pivot::GroupBy::None,
        exclude: Vec::new(),
    };
    let queue = |wb: &mut Workbook| {
        let result = sheetcore::pivot::compute(&wb.sheets[0], &spec);
        let rect = sheetcore::pivot::write_to(&mut wb.sheets[0], (5, 0), &result);
        wb.pivots_pending.push((0, spec.clone(), result, rect, "Amount".into()));
    };
    let wait = |tasks: &mut DocumentTasks, wb: &mut Workbook| {
        let started = Instant::now();
        loop {
            if let Some(result) = tasks.poll_save(wb) { result.unwrap(); break; }
            assert!(started.elapsed() < Duration::from_secs(20));
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    queue(&mut wb);
    tasks.start_save(&mut wb, path.clone());
    assert!(sheetcore::sheet_ops::delete_sheet(&mut wb, 0));
    assert!(sheetcore::sheet_ops::rename_sheet(&mut wb, 0, "Live"));
    queue(&mut wb);
    wait(&mut tasks, &mut wb);
    assert!(wb.dirty);
    assert_eq!(wb.pivots_pending.len(), 1, "completion must not consume new attachments");
    for _ in 0..3 {
        tasks.start_save(&mut wb, path.clone());
        wait(&mut tasks, &mut wb);
        let definitions: Vec<_> = sheetcore::fixups::parts(&path).into_iter()
            .filter(|p| p.contains("pivotCacheDefinition") && p.ends_with(".xml")).collect();
        assert_eq!(definitions.len(), 1, "the deleted snapshot attachment must not survive");
        let xml = String::from_utf8(sheetcore::fixups::read_part_file(&path, &definitions[0]).unwrap()).unwrap();
        assert!(xml.contains("sheet=\"Live\""), "{xml}");
        let workbook = String::from_utf8(sheetcore::fixups::read_part_file(&path, "xl/workbook.xml").unwrap()).unwrap();
        assert!(workbook.contains("<pivotCache "), "registration must survive the complete pipeline");
    }
}

#[test]
fn background_save_acknowledges_only_snapshot_sheets_without_losing_new_edits() {
    let env = crate::test_env::TestEnv::new("async-receipt");
    let path = env.path("snapshot.xlsx");
    let mut tasks = DocumentTasks::default();
    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(0, 0, Cell { value: Value::Number(1.), formula: None });
    tasks.start_save(&mut wb, path.clone());
    sheetcore::sheet_ops::rename_sheet(&mut wb, 0, "Renamed");
    sheetcore::sheet_ops::insert_sheet(&mut wb, 0, Sheet::new("Sheet1"));
    wb.sheets[1].set(0, 0, Cell::default());
    let started = Instant::now();
    loop {
        if let Some(result) = tasks.poll_save(&mut wb) { assert_eq!(result.unwrap().0, path); break; }
        assert!(started.elapsed() < Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(wb.dirty, "edits made after snapshot remain dirty");
    assert!(wb.sheets[0].source_part.is_none());
    assert!(wb.sheets[1].source_part.is_some());
    assert!(wb.sheets[1].cleared.contains(&sheetcore::model::CellRef::new(0, 0)));
    assert_eq!(sheetcore::xlsx::load(&path).unwrap().sheets[0].value(0, 0), Value::Number(1.));
    tasks.start_save(&mut wb, path.clone());
    loop {
        if let Some(result) = tasks.poll_save(&mut wb) { result.unwrap(); break; }
        assert!(started.elapsed() < Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!wb.dirty);
    assert!(wb.sheets.iter().all(|s| s.source_part.is_some()));
    let back = sheetcore::xlsx::load(&path).unwrap();
    assert_eq!(back.sheets.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(), ["Sheet1", "Renamed"]);
    assert_eq!(back.sheets[1].value(0, 0), Value::Empty);
}
