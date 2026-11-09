use super::*;
use sheetcore::{Cell, Sheet, Value};
use std::time::Duration;

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
