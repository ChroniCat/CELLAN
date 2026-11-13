//! Exercise the real tab and save handlers without creating a native window.

use super::*;
use std::sync::mpsc::{self, Sender};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn app() -> SheetApp {
    SheetApp::new_with_context(&egui::Context::default(), None)
}

fn put(wb: &mut Workbook, number: f64) {
    wb.sheets[0].set(0, 0, Cell { value: Value::Number(number), formula: None });
    wb.dirty = true;
}

fn book(number: f64) -> Workbook {
    let mut wb = Workbook::new_blank();
    put(&mut wb, number);
    wb.dirty = false;
    wb
}

fn delayed_save(app: &mut SheetApp, target: &str) -> Sender<Result<sheetcore::xlsx::SaveReceipt, String>> {
    let (tx, rx) = mpsc::channel();
    app.tasks.saving = Some(document_tasks::PendingSave { target: target.into(), rx });
    app.wb.dirty = false;
    tx
}

fn parked(app: &SheetApp, index: usize) -> &ParkedDoc {
    match &app.tabs[index] {
        TabSlot::Parked(doc) => doc,
        TabSlot::Active => panic!("expected parked document"),
    }
}

#[test]
fn save_completion_updates_its_parked_document_only() {
    let mut app = app();
    put(&mut app.wb, 11.0);
    let tx = delayed_save(&mut app, "a-saved.xlsx");
    app.open_in_new_tab(PathBuf::new());
    put(&mut app.wb, 22.0);
    tx.send(Ok(sheetcore::xlsx::SaveReceipt::default())).unwrap();
    app.poll_save();

    assert_eq!(app.wb.path, None, "A must not assign its save path to B");
    assert_eq!(app.wb.sheets[0].value(0, 0), Value::Number(22.0));
    assert!(app.wb.dirty);
    assert_eq!(parked(&app, 0).wb.path, Some(PathBuf::from("a-saved.xlsx")));
    assert_eq!(parked(&app, 0).title, "a-saved.xlsx");
    app.switch_tab(0);
    assert_eq!(app.wb.sheets[0].value(0, 0), Value::Number(11.0));
    assert!(!app.wb.dirty);
}

#[test]
fn saves_completing_out_of_order_keep_their_own_paths() {
    let mut app = app();
    let a = delayed_save(&mut app, "a.xlsx");
    app.open_in_new_tab(PathBuf::new());
    let b = delayed_save(&mut app, "b.xlsx");
    b.send(Ok(sheetcore::xlsx::SaveReceipt::default())).unwrap();
    app.poll_save();
    assert_eq!(app.wb.path, Some(PathBuf::from("b.xlsx")));
    assert!(parked(&app, 0).tasks.saving.is_some());
    app.switch_tab(0);
    a.send(Ok(sheetcore::xlsx::SaveReceipt::default())).unwrap();
    app.poll_save();
    assert_eq!(app.wb.path, Some(PathBuf::from("a.xlsx")));
    assert_eq!(parked(&app, 1).wb.path, Some(PathBuf::from("b.xlsx")));
}

#[test]
fn failed_or_disconnected_save_keeps_owner_dirty() {
    for disconnected in [false, true] {
        let mut app = app();
        let tx = delayed_save(&mut app, "a.xlsx");
        app.open_in_new_tab(PathBuf::new());
        if !disconnected {
            tx.send(Err("disk full".into())).unwrap();
        }
        drop(tx);
        app.poll_save();
        assert!(!app.wb.dirty, "failure in A must not dirty B");
        assert!(parked(&app, 0).wb.dirty);
        assert_eq!(parked(&app, 0).wb.path, None);
        assert!(parked(&app, 0).tasks.saving.is_none());
    }
}

#[test]
fn unsupported_xls_save_keeps_formula_and_shows_compatibility_warning() {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("sc_app_xls_compat_{}_{}", std::process::id(), stamp));
    std::fs::create_dir_all(&dir).unwrap();
    let target = dir.join("keep.xls");
    std::fs::write(&target, b"ORIGINAL").unwrap();
    let mut app = app();
    app.wb.sheets[0].set(
        0,
        0,
        Cell {
            value: Value::Number(7.0),
            formula: Some("=@A1".into()),
        },
    );
    app.wb.path = Some(target.clone());
    app.wb.dirty = true;
    app.save(false);
    let deadline = Instant::now() + Duration::from_secs(10);
    while app.has_pending_save() && Instant::now() < deadline {
        app.poll_save();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!app.has_pending_save(), "保存线程未结束");
    assert_eq!(app.wb.sheets[0].get(0, 0).unwrap().formula.as_deref(), Some("=@A1"));
    assert_eq!(app.wb.sheets[0].value(0, 0), Value::Number(7.0));
    assert!(app.wb.dirty, "保存失败必须保持脏位");
    assert_eq!(std::fs::read(&target).unwrap(), b"ORIGINAL", "失败不得改写目标");
    assert!(app.status.contains("兼容提示"), "应显示兼容提示：{}", app.status);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn loading_and_recalculation_follow_the_tab_that_started_them() {
    let mut app = app();
    let (tx, rx) = mpsc::channel();
    app.tasks.open_rx = Some(rx);
    app.tasks.busy = true;
    app.open_in_new_tab(PathBuf::new());
    put(&mut app.wb, 99.0);
    tx.send(OpenMsg::Loaded(Box::new(book(10.0)), false, Box::new(sheetcore::xlsx::LoadDiag::default()))).unwrap();
    tx.send(OpenMsg::Recalced(
        Box::new(book(20.0)), Box::new(sheetcore::formula::deps::Deps::empty()),
    )).unwrap();
    app.poll_open();
    assert_eq!(app.wb.sheets[0].value(0, 0), Value::Number(99.0));
    app.switch_tab(0);
    app.poll_open();
    assert_eq!(app.wb.sheets[0].value(0, 0), Value::Number(20.0));
    assert!(app.tasks.open_rx.is_none());
    assert!(!app.tasks.busy);
    assert_eq!(parked(&app, 1).wb.sheets[0].value(0, 0), Value::Number(99.0));
}

#[test]
fn stale_recalculation_still_respects_edits_after_switching_tabs() {
    let mut app = app();
    let (tx, rx) = mpsc::channel();
    app.tasks.open_rx = Some(rx);
    app.tasks.busy = true;
    app.doc_gen = 1;
    put(&mut app.wb, 42.0);
    app.open_in_new_tab(PathBuf::new());
    tx.send(OpenMsg::Recalced(
        Box::new(book(10.0)), Box::new(sheetcore::formula::deps::Deps::empty()),
    )).unwrap();
    app.switch_tab(0);
    app.poll_open();
    assert_eq!(app.wb.sheets[0].value(0, 0), Value::Number(42.0));
    assert!(app.tasks.needs_full);
    assert!(app.wb.dirty);
}

#[test]
fn recalculation_cannot_restore_the_path_from_before_save_as() {
    let mut app = app();
    app.wb = book(42.0);
    app.wb.path = Some(PathBuf::from("old.xlsx"));
    app.wb.dirty = true;
    let old_snapshot = app.wb.clone();
    let save = delayed_save(&mut app, "new.xlsx");
    save.send(Ok(sheetcore::xlsx::SaveReceipt::default())).unwrap();
    app.poll_save();
    let (tx, rx) = mpsc::channel();
    app.tasks.open_rx = Some(rx);
    tx.send(OpenMsg::Recalced(
        Box::new(old_snapshot), Box::new(sheetcore::formula::deps::Deps::empty()),
    )).unwrap();
    app.poll_open();
    assert_eq!(app.wb.path, Some(PathBuf::from("new.xlsx")));
    assert!(!app.wb.dirty, "old calculation must not restore old save state");
}

#[test]
fn opening_another_file_while_saving_preserves_the_saving_tab() {
    let mut app = app();
    let tx = delayed_save(&mut app, "a.xlsx");
    // The load may fail, but its result must belong to the new tab.
    app.open_path(std::env::temp_dir().join("sheetflow-no-such-doc-for-save-test.xlsx"));
    assert_eq!(app.tabs.len(), 2);
    assert_eq!(app.active_tab, 1);
    tx.send(Ok(sheetcore::xlsx::SaveReceipt::default())).unwrap();
    app.poll_save();
    assert_eq!(app.wb.path, None);
    assert_eq!(parked(&app, 0).wb.path, Some(PathBuf::from("a.xlsx")));
}

#[test]
fn closing_a_saving_tab_waits_for_its_result() {
    let mut app = app();
    let tx = delayed_save(&mut app, "a.xlsx");
    app.open_in_new_tab(PathBuf::new());
    app.close_tab(0);
    assert_eq!(app.tabs.len(), 2);
    app.switch_tab(0);
    app.close_tab(0);
    assert_eq!(app.tabs.len(), 2);
    tx.send(Ok(sheetcore::xlsx::SaveReceipt::default())).unwrap();
    app.poll_save();
    app.close_tab(0);
    assert_eq!(app.tabs.len(), 1);
    assert_eq!(app.wb.path, None);
}

#[test]
fn rejected_edit_survives_tab_switch_new_tab_and_save() {
    let mut app = app();
    app.open_in_new_tab(PathBuf::new());
    app.switch_tab(0);
    app.wb.sheets[0].dv.push(sheetcore::dv::DataValidation {
        ranges: vec![(0, 0, 0, 0)],
        kind: sheetcore::dv::DvKind::Whole,
        f1: "1".into(),
        f2: "10".into(),
        ..Default::default()
    });
    app.editing = Some(EditState::new("99".into()));
    app.switch_tab(1);
    assert_eq!(app.active_tab, 0);
    assert_eq!(app.editing.as_ref().unwrap().buf, "99");
    app.open_in_new_tab(PathBuf::new());
    assert_eq!(app.tabs.len(), 2);
    assert_eq!(app.editing.as_ref().unwrap().buf, "99");
    app.save(false);
    assert!(app.tasks.saving.is_none());
    assert_eq!(app.editing.as_ref().unwrap().buf, "99");
    assert_eq!(app.wb.sheets[0].value(0, 0), Value::Empty);
}

#[test]
fn failed_save_in_the_close_frame_keeps_the_window_open() {
    let mut app = app();
    let tx = delayed_save(&mut app, "a.xlsx");
    app.open_in_new_tab(PathBuf::new());
    let was_saving = app.has_pending_save();
    tx.send(Err("disk full".into())).unwrap();
    app.poll_save();
    assert!(!app.has_pending_save());
    let ctx = egui::Context::default();
    let mut input = egui::RawInput::default();
    input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap()
        .events.push(egui::ViewportEvent::Close);
    let output = ctx.run(input, |ctx| app.defer_close_while_saving(ctx, was_saving));
    assert!(output.viewport_output[&egui::ViewportId::ROOT].commands.iter()
        .any(|command| matches!(command, egui::ViewportCommand::CancelClose)));
    assert!(parked(&app, 0).wb.dirty);
    assert!(app.status.contains("disk full"));
}

#[test]
fn sheet_handlers_update_references_dependencies_and_indexed_state() {
    let mut app = app();
    app.editing = Some(EditState::new("1".into()));
    app.commit_edit(None);
    assert_eq!(app.undo.depth().0, 1);
    let mut other = sheetcore::Sheet::new("Other");
    other.set(0, 0, Cell { value: Value::Number(0.0), formula: Some("=Sheet1!A1".into()) });
    app.wb.sheets.push(other);
    app.wb.names.push(("LocalValue".into(), Some(1), "Other!$A$1".into()));
    assert!(app.set_sheet_name(0, "源表"));
    assert_eq!(app.undo.depth(), (0, 0));
    assert_eq!(app.wb.sheets[1].value(0, 0), Value::Number(1.0));
    put(&mut app.wb, 10.0);
    sheetcore::formula::recalc_dirty(&mut app.wb, &app.deps, &[(0, 0, 0)]);
    assert_eq!(app.wb.sheets[1].value(0, 0), Value::Number(10.0));
    app.move_sheet(0, 1);
    assert_eq!(app.wb.sheets[0].name, "Other");
    assert_eq!(app.wb.names[0].1, Some(0));
    app.delete_sheet(1);
    assert_eq!(app.wb.sheets.len(), 1);
    assert_eq!(app.wb.sheets[0].value(0, 0).display(), "#REF!");
}

#[test]
fn drag_reordering_preserves_active_sheet_and_local_scope() {
    let mut app = app();
    app.wb.sheets.push(sheetcore::Sheet::new("Other"));
    app.wb.names.push(("LocalValue".into(), Some(0), "Sheet1!$A$1".into()));
    app.wb.dirty = false;
    assert!(app.swap_sheet_positions(1, 0));
    assert_eq!(app.sheet_idx, 1);
    assert_eq!(app.wb.sheets[app.sheet_idx].name, "Sheet1");
    assert_eq!(app.wb.names[0].1, Some(1));
    assert!(app.wb.dirty);
    let (_tx, rx) = mpsc::channel();
    app.tasks.open_rx = Some(rx);
    assert!(!app.swap_sheet_positions(0, 1));
    assert_eq!(app.wb.sheets[1].name, "Sheet1");
}

#[test]
fn failed_open_restores_dependencies_of_the_retained_document() {
    let mut app = app();
    put(&mut app.wb, 1.0);
    app.wb.sheets[0].set(0, 1, Cell { value: Value::Number(2.0), formula: Some("=A1+1".into()) });
    app.reset_editor_state();
    let (tx, rx) = mpsc::channel();
    app.tasks.open_rx = Some(rx);
    tx.send(OpenMsg::Failed("cannot read workbook".into())).unwrap();
    app.poll_open();
    app.editing = Some(EditState::new("10".into()));
    app.commit_edit(None);
    assert_eq!(app.wb.sheets[0].value(0, 1), Value::Number(11.0));
}

#[test]
fn duplicate_sheet_recalculates_its_array_without_false_obstruction() {
    let mut app = app();
    app.wb.sheets[0].set(0, 0, Cell { value: Value::Empty, formula: Some("=SEQUENCE(2)".into()) });
    sheetcore::formula::recalc_all(&mut app.wb);
    app.duplicate_sheet(0);
    assert_eq!(app.wb.sheets[1].value(0, 0), Value::Number(1.0));
    assert_eq!(app.wb.sheets[1].value(1, 0), Value::Number(2.0));
}

#[test]
fn duplicate_insertion_keeps_existing_local_name_scopes() {
    let mut app = app();
    app.wb.sheets.push(sheetcore::Sheet::new("Other"));
    app.wb.names.push(("LocalValue".into(), Some(1), "Other!$A$1".into()));
    app.duplicate_sheet(0);
    assert_eq!(app.wb.sheets.len(), 3);
    assert_eq!(app.wb.sheets[1].source_part, None);
    assert_eq!(app.wb.sheets[2].name, "Other");
    assert_eq!(app.wb.names[0].1, Some(2));
}

#[test]
fn ordinary_app_save_keeps_real_file_history_and_later_edits() {
    let dir = std::env::temp_dir().join(format!(
        "sheetflow-app-save-{}-{}", std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos(),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("book.xlsx");
    sheetcore::xlsx::save_atomic(&path, &book(11.0)).unwrap();
    let original = std::fs::read(&path).unwrap();
    let mut app = app();
    app.wb = sheetcore::xlsx::load(&path).unwrap();
    put(&mut app.wb, 22.0);
    app.save(false);
    assert!(app.tasks.saving.is_some());
    put(&mut app.wb, 33.0);
    let deadline = Instant::now() + Duration::from_secs(10);
    while app.tasks.saving.is_some() && Instant::now() < deadline {
        app.poll_save();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(app.tasks.saving.is_none(), "save did not complete");
    assert!(app.wb.dirty, "edits after the snapshot must remain unsaved");
    assert_eq!(app.wb.sheets[0].value(0, 0), Value::Number(33.0));
    assert_eq!(sheetcore::xlsx::load(&path).unwrap().sheets[0].value(0, 0), Value::Number(22.0));
    let history = sheetcore::backup::list(&path);
    assert_eq!(history.len(), 1, "Ctrl+S must back up the actual target");
    assert_eq!(std::fs::read(&history[0].path).unwrap(), original);
    assert!(sheetcore::backup::list(&path.with_extension("xlsx.tmp")).is_empty());
}

#[test]
fn xls_save_as_uses_native_format_history_and_its_owning_tab() {
    let dir = std::env::temp_dir().join(format!(
        "sheetflow-app-xls-save-{}-{}", std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos(),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("source.xlsx");
    let target = dir.join("book.XLS");
    sheetcore::xlsx::save_atomic(&source, &book(11.0)).unwrap();
    sheetcore::xls_write::write(&target, &book(10.0)).unwrap();
    let original = std::fs::read(&target).unwrap();
    let mut app = app();
    app.wb = sheetcore::xlsx::load(&source).unwrap();
    put(&mut app.wb, 22.0);
    app.tasks.start_save(&mut app.wb, target.clone());
    put(&mut app.wb, 33.0);
    app.open_in_new_tab(PathBuf::new());
    put(&mut app.wb, 99.0);
    let deadline = Instant::now() + Duration::from_secs(10);
    while app.has_pending_save() && Instant::now() < deadline {
        app.poll_save();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!app.has_pending_save(), "save did not complete");
    let owner = parked(&app, 0);
    assert_eq!(owner.wb.path.as_ref(), Some(&target));
    assert!(owner.wb.dirty);
    assert_eq!(owner.wb.sheets[0].value(0, 0), Value::Number(33.0));
    assert_eq!(app.wb.path, None);
    assert_eq!(app.wb.sheets[0].value(0, 0), Value::Number(99.0));
    assert!(std::fs::read(&target).unwrap().starts_with(&[0xD0, 0xCF, 0x11, 0xE0]));
    assert_eq!(sheetcore::xlsx::load(&target).unwrap().sheets[0].value(0, 0), Value::Number(22.0));
    assert_eq!(sheetcore::xlsx::load(&source).unwrap().sheets[0].value(0, 0), Value::Number(11.0));
    let history = sheetcore::backup::list(&target);
    assert_eq!(history.len(), 1);
    assert_eq!(std::fs::read(&history[0].path).unwrap(), original);
}

#[test]
fn xls_save_with_formula_succeeds() {
    let dir = std::env::temp_dir().join(format!(
        "sheetflow-app-xls-formula-{}-{}",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos(),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let target = dir.join("book.xls");
    sheetcore::xls_write::write(&target, &book(11.0)).unwrap();
    let mut app = app();
    app.wb = sheetcore::xlsx::load(&target).unwrap();
    app.wb.sheets[0].set(
        0,
        0,
        Cell {
            value: Value::Number(3.0),
            formula: Some("=1+2".into()),
        },
    );
    app.wb.dirty = true;
    app.save(false);
    let deadline = Instant::now() + Duration::from_secs(10);
    while app.has_pending_save() && Instant::now() < deadline {
        app.poll_save();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!app.has_pending_save(), "save did not complete");
    assert!(!app.wb.dirty, "存成功后应清掉脏位：{}", app.status);
    assert_eq!(app.wb.path.as_ref(), Some(&target));
    // 写出来的 .xls 必须还能被我们自己读回来，且值是公式算出来的那个
    let back = sheetcore::xlsx::load(&target).unwrap();
    match &back.sheets[0].get(0, 0).unwrap().value {
        Value::Number(n) => assert!((n - 3.0).abs() < 1e-9),
        v => panic!("A1 应为数字 3，得到 {v:?}"),
    }
}

