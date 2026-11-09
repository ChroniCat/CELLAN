use super::*;
use sheetcore::{Cell, Sheet, Value};
use std::time::Duration;

#[test]
fn saving_during_pivot_replacement_preserves_only_the_live_pending_attachment() {
    let dir = std::env::temp_dir().join(format!("native_async_pivot_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("pivot.xlsx");
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
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn background_save_acknowledges_only_snapshot_sheets_without_losing_new_edits() {
    let dir = std::env::temp_dir().join(format!("native_async_receipt_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("snapshot.xlsx");
    let mut tasks = DocumentTasks::default();
    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(0, 0, Cell { value: Value::Number(1.), formula: None });
    tasks.start_save(&mut wb, path.clone());
    sheetcore::sheet_ops::rename_sheet(&mut wb, 0, "Renamed");
    sheetcore::sheet_ops::insert_sheet(&mut wb, 0, Sheet::new("Sheet1"));
    wb.sheets[1].set(0, 0, Cell::default());
    let started = Instant::now();
    loop {
        if let Some(result) = tasks.poll_save(&mut wb) { assert_eq!(result.unwrap(), path); break; }
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
    std::fs::remove_dir_all(dir).unwrap();
}
