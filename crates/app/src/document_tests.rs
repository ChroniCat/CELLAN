//! Exercise the real tab and save handlers without creating a native window.

use super::*;
use std::sync::mpsc::{self, Sender};
use std::time::{Duration, Instant};

fn app() -> SheetApp {
    SheetApp::new_with_context(&egui::Context::default(), None)
}

fn put(wb: &mut Workbook, number: f64) {
    wb.sheets[0].set(0, 0, Cell { value: Value::Number(number), formula: None });
    wb.dirty = true;
}

#[test]
fn overlapping_background_edits_keep_both_closures_and_reuse_the_index() {
    let mut app = app();
    let ctx = egui::Context::default();
    for r in 1..=650 {
        for (col, formula) in ["=$A$1", "=$B$1"].into_iter().enumerate() {
            app.wb.sheets[0].set(r, col as u32, Cell {
                value: Value::Number(0.0), formula: Some(formula.into()),
            });
        }
    }
    app.deps = sheetcore::formula::deps::Deps::build(&app.wb).into();
    app.deps_built = true;
    let original = app.deps.clone();
    for (col, value) in [11.0, 22.0].into_iter().enumerate() {
        app.wb.sheets[0].set(0, col as u32, Cell { value: Value::Number(value), formula: None });
        app.doc_gen += 1;
        app.fast_recalc_cells(&[(0, 0, col as u32)]);
        assert!(!app.tasks.needs_full);
        assert_eq!(app.tasks.dirty_recalc_seeds.len(), col + 1);
        app.start_silent_recalc(&ctx);
        assert!(app.bg_recalc_rx.is_some());
    }
    app.add_cf(sheetcore::cf::CfKind::Expr { formula: "A1>0".into(), dxf: 0 });
    let deadline = Instant::now() + Duration::from_secs(10);
    while app.tasks.recalc_pending() && Instant::now() < deadline {
        app.poll_silent_recalc();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!app.tasks.recalc_pending());
    assert!(std::sync::Arc::ptr_eq(&original, &app.deps));
    assert!(!app.wb.sheets[0].cf.blocks.is_empty());
    assert_eq!(app.wb.sheets[0].value(650, 0), Value::Number(11.0));
    assert_eq!(app.wb.sheets[0].value(650, 1), Value::Number(22.0));
}

#[test]
fn autosave_probes_a_foreign_host_only_after_idle_and_backoff() {
    let env = crate::test_env::TestEnv::new("autosave-probe");
    let path = env.path("shared.xlsx");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut app = app();
    app.autosave = true;
    app.wb.path = Some(path.clone());
    app.wb.dirty = true;
    session_lock::write_session(&path, &listener.local_addr().unwrap().to_string(), "other", "other-session", 1).unwrap();
    app.tasks.last_edit = Some(Instant::now());
    app.tick_autosave(None);
    assert_eq!(listener.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
    app.tasks.last_edit = Some(Instant::now() - Duration::from_secs(6));
    app.tick_autosave(None);
    wait_for_background_saves(&mut app);
    assert!(listener.accept().is_ok());
    assert!(app.tasks.saving.is_none() && app.wb.dirty);
    assert!(app.status.contains("阻止直接覆盖"));
    app.tick_autosave(None);
    assert!(app.tasks.saving.is_none());
    assert_eq!(listener.accept().unwrap_err().kind(), std::io::ErrorKind::WouldBlock);
    // A retry delay belongs only to the workbook whose save failed.
    app.has_open_workbook = true;
    app.open_in_new_tab(PathBuf::new());
    let second = env.path("second.xlsx");
    app.wb.path = Some(second.clone());
    put(&mut app.wb, 22.0);
    app.tasks.last_edit = Some(Instant::now() - Duration::from_secs(1));
    app.tick_autosave(None);
    wait_for_background_saves(&mut app);
    assert_eq!(sheetcore::xlsx::load(&second).unwrap().sheets[0].value(0, 0), Value::Number(22.0));
    drop(listener);
    session_lock::remove(&path);
    app.switch_tab(0);
    app.tasks.autosave_retry_at = Some(Instant::now());
    app.tick_autosave(None);
    wait_for_background_saves(&mut app);
    assert!(path.is_file() && !app.wb.dirty);
}

fn wait_for_background_saves(app: &mut SheetApp) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while app.has_pending_save() && Instant::now() < deadline {
        app.poll_save();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!app.has_pending_save(), "save worker did not finish");
}

#[test]
fn imported_ods_stays_intact_through_autosave_and_manual_save_as() {
    let env = crate::test_env::TestEnv::new("autosave-ods");
    let source = env.path("budget.ods");
    let original = include_bytes!("../../core/tests/fixtures/import-budget.ods");
    std::fs::write(&source, original).unwrap();
    let mut app = app();
    app.autosave = true;
    app.open_path(source.clone());
    let deadline = Instant::now() + Duration::from_secs(10);
    while app.tasks.open_rx.is_some() && Instant::now() < deadline {
        app.poll_open();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!app.tasks.busy && !app.wb.load_incomplete);
    assert_eq!(app.wb.sheet(0).value(0, 0), Value::Number(25.0));
    app.begin_edit(Some("80".into()));
    assert!(app.commit_edit(None));
    app.tasks.last_edit = Some(Instant::now() - Duration::from_secs(1));
    app.tick_autosave(None);
    assert!(app.wb.dirty && !app.has_pending_save());
    assert!(sheetcore::xlsx::save_atomic(&source, &app.wb).is_err());
    assert!(sheetcore::xlsx::save(&source, &app.wb).is_err());
    let copy = env.path("budget.xlsx");
    env.sc("SC_SAVE_AS", copy.to_str().unwrap());
    app.save(false);
    wait_for_background_saves(&mut app);
    assert_eq!(app.wb.path.as_ref(), Some(&copy));
    assert_eq!(sheetcore::xlsx::load(&copy).unwrap().sheet(0).value(0, 0), Value::Number(80.0));
    assert_eq!(sheetcore::xlsx::load(&source).unwrap().sheet(0).value(0, 0), Value::Number(25.0));
    assert_eq!(std::fs::read(&source).unwrap().as_slice(), original);
}

#[test]
fn autosave_guest_writes_its_local_copy_without_writing_host_original() {
    let env = crate::test_env::TestEnv::new("autosave-local-guest");
    env.sc("SC_HOST_ADDR", "127.0.0.1");
    let original = env.path("host.xlsx");
    let local = env.path("local.xlsx");
    sheetcore::xlsx::save_atomic(&original, &book(1.0)).unwrap();
    std::fs::copy(&original, &local).unwrap();
    let mut host = app();
    host.wb = sheetcore::xlsx::load(&original).unwrap();
    host.room_password = "room".into();
    host.start_host();
    let address = session_lock::read(&original).unwrap().host;
    let mut guest = app();
    guest.autosave = true;
    guest.wb = sheetcore::xlsx::load(&local).unwrap();
    guest.room_password = "room".into();
    guest.join_addr = address;
    guest.join_shared();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !guest.joined_doc && Instant::now() < deadline {
        host.poll_net();
        guest.poll_net();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(guest.joined_doc && !guest.is_delegated_collab_client());
    guest.begin_edit(Some("99".into()));
    assert!(guest.commit_edit(None));
    guest.tasks.last_edit = Some(Instant::now() - Duration::from_secs(1));
    guest.tick_autosave(None);
    wait_for_background_saves(&mut guest);
    assert_eq!(sheetcore::xlsx::load(&local).unwrap().sheet(0).value(0, 0), Value::Number(99.0));
    assert_eq!(sheetcore::xlsx::load(&original).unwrap().sheet(0).value(0, 0), Value::Number(1.0));
    guest.leave_session();
    host.leave_session();
}

#[test]
fn autosave_two_successive_cell_commits_reach_disk_promptly() {
    let env = crate::test_env::TestEnv::new("autosave-wallclock");
    let path = env.path("budget.xlsx");
    sheetcore::xlsx::save_atomic(&path, &book(0.0)).unwrap();
    let mut app = app();
    app.autosave = true;
    app.has_open_workbook = true;
    app.wb = sheetcore::xlsx::load(&path).unwrap();
    for (row, amount) in [(0, 125.0), (1, 248.0)] {
        app.sel = (row, 0);
        app.sel_end = app.sel;
        app.begin_edit(Some(amount.to_string()));
        let started = Instant::now();
        assert!(app.commit_edit(None));
        assert!(app.wb.dirty);
        while app.wb.dirty || app.has_pending_save() {
            assert!(started.elapsed() < Duration::from_secs(8), "a small workbook took too long to autosave");
            app.tick_autosave(None);
            app.poll_save();
            std::thread::sleep(Duration::from_millis(10));
        }
        let disk = sheetcore::xlsx::load(&path).unwrap();
        assert_eq!(disk.sheet(0).value(0, 0), Value::Number(125.0));
        assert_eq!(disk.sheet(0).value(row, 0), Value::Number(amount));
        eprintln!("autosave A{} commit to disk readback: {} ms", row + 1, started.elapsed().as_millis());
    }
}

#[test]
fn autosave_writes_committed_edits_while_typing_and_after_switching_tabs() {
    let env = crate::test_env::TestEnv::new("autosave-edit-tabs");
    let mut app = app();
    app.autosave = true;
    app.has_open_workbook = true;
    let a = env.path("a.xlsx");
    app.wb.path = Some(a.clone());
    put(&mut app.wb, 11.0);
    app.tasks.last_edit = Some(Instant::now());
    app.tick_autosave(None);
    assert!(app.tasks.saving.is_none());
    app.open_in_new_tab(PathBuf::new());
    let b = env.path("b.xlsx");
    app.wb.path = Some(b.clone());
    put(&mut app.wb, 22.0);
    app.sel = (1, 0);
    app.sel_end = app.sel;
    app.begin_edit(Some("uncommitted".into()));
    // The user keeps committing other edits, so the short debounce never expires.
    app.tasks.last_edit = Some(Instant::now());
    app.tasks.autosave_dirty_since = Some(Instant::now() - Duration::from_secs(3));
    if let TabSlot::Parked(first) = &mut app.tabs[0] {
        first.tasks.last_edit = Some(Instant::now() - Duration::from_secs(1));
    }
    app.tick_autosave(None);
    assert!(parked(&app, 0).tasks.saving.as_ref().is_some_and(|s| s.quiet));
    assert!(app.tasks.saving.as_ref().is_some_and(|s| s.quiet));
    wait_for_background_saves(&mut app);
    assert_eq!(app.editing.as_ref().unwrap().buf, "uncommitted");
    assert_eq!(sheetcore::xlsx::load(&a).unwrap().sheets[0].value(0, 0), Value::Number(11.0));
    let saved = sheetcore::xlsx::load(&b).unwrap();
    assert_eq!(saved.sheets[0].value(0, 0), Value::Number(22.0));
    assert!(saved.sheets[0].get(1, 0).is_none());
}

#[test]
fn private_workbook_revisions_stay_in_this_session() {
    let env = crate::test_env::TestEnv::new("private-revisions");
    let mut app = app();
    let path = env.path("private.xlsx");
    app.wb.path = Some(path.clone());
    let before = Cell { value: Value::Number(21.0), formula: None };
    let after = Cell { value: Value::Number(42.0), formula: Some("21*2".into()) };
    let delta = sheetcore::undo::CellDelta {
        sheet: 0, row: 0, col: 0, before: before.clone(), after: after.clone(),
        before_style: 0, after_style: 0,
    };
    app.wb.sheets[0].set(0, 0, after);
    let enc = sheetcore::crypto::EncryptionSettings::new("test-open-password");
    for (source, output) in [(false, true), (true, true), (true, false)] {
        app.wb.source_encryption = source.then(|| enc.clone());
        app.wb.encryption = output.then(|| enc.clone());
        app.rev.clear();
        app.record_revision("local", std::slice::from_ref(&delta));
        app.record_remote_ops(&[sheetcore::sync::Op {
            sheet: 0, row: 0, col: 0, value: sheetcore::sync::ValueWire::Number(42.0),
            formula: Some("21*2".into()), ts: 1, site: 7,
        }], vec![Some(before.clone())]);
        app.reload_revisions();
        assert_eq!(app.rev.len(), 2);
        assert!(app.rev[1].remote);
        assert!(!sheetcore::revision::dir_for(&path).exists());
        assert!(!env.path(".~sheetflow-history").exists());
    }
    app.wb.encryption = None;
    app.wb.source_encryption = None;
    app.rev.clear();
    app.record_revision("in_memory_only", &[delta]);
    app.reload_revisions();
    assert_eq!(app.rev.len(), 1);
    assert_eq!(app.rev[0].label, "in_memory_only");
    assert!(!sheetcore::revision::dir_for(&path).exists());
    assert!(!env.path(".~sheetflow-history").exists());
}

#[test]
fn shared_directory_auto_hosts_and_peers_auto_join_silently_without_passwords() {
    let env = crate::test_env::TestEnv::new("silent-auto-collaboration");
    unsafe {
        std::env::set_var("SC_HOST_ADDR", "127.0.0.1");
    }

    // 1. 普通私有未共享目录：无他人占用卡时不主动监听端口
    let local_dir = env.path("private-local");
    std::fs::create_dir_all(&local_dir).unwrap();
    let local_path = local_dir.join("draft.xlsx");
    sheetcore::xlsx::save(&local_path, &book(1.0)).unwrap();
    let mut local_app = app();
    local_app.wb.path = Some(local_path.clone());
    local_app.try_auto_join();
    local_app.auto_discovery_tick();
    assert!(local_app.net.is_none());
    assert!(!session_lock::lock_path(&local_path).exists());

    // 2. 共享目录：甲和乙零口令、零手工点击，打开同一文件即自动建房并连入实时同步
    let shared_dir = env.path("team-shared");
    std::fs::create_dir_all(&shared_dir).unwrap();
    let shared_path = shared_dir.join("book.xlsx");
    sheetcore::xlsx::save(&shared_path, &book(10.0)).unwrap();
    session_lock::set_dir_marked_shared(&shared_path, true).unwrap();

    let mut host_a = app();
    host_a.my_name = "甲".into();
    host_a.wb = book(25.0);
    host_a.wb.path = Some(shared_path.clone());
    assert!(host_a.room_password.is_empty() && host_a.auto_share);
    host_a.try_auto_join();
    assert!(
        host_a.net.as_ref().is_some_and(|n| n.hosting),
        "甲打开共享目录文件应静默自动开启宿主"
    );

    let mut peer_b = app();
    peer_b.my_name = "乙".into();
    peer_b.wb = book(10.0);
    peer_b.wb.path = Some(shared_path.clone());
    assert!(peer_b.room_password.is_empty() && peer_b.auto_share);
    peer_b.try_auto_join();
    assert!(
        peer_b.net.as_ref().is_some_and(|n| !n.hosting),
        "乙打开同一文件应静默自动加入甲的会话"
    );

    for _ in 0..40 {
        host_a.poll_net();
        peer_b.poll_net();
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(peer_b.joined_doc, "乙应无阻塞完成快照同步");
    assert_eq!(peer_b.wb.sheets[0].value(0, 0), Value::Number(25.0));
    assert_eq!(host_a.peers_list.len(), 2, "甲应立即看到在线 2 人");
    assert_eq!(peer_b.peers_list.len(), 2, "乙应立即看到在线 2 人");

    peer_b.leave_session();
    host_a.leave_session();
    unsafe {
        std::env::remove_var("SC_HOST_ADDR");
    }
}

#[test]
fn room_password_belongs_to_its_workbook_tab() {
    let _env = crate::test_env::TestEnv::new("room-password-tabs");
    let mut app = app();
    app.has_open_workbook = true;
    app.room_password = "first-room".into();
    app.open_in_new_tab(PathBuf::new());
    assert!(app.room_password.is_empty());
    app.room_password = "second-room".into();
    app.switch_tab(0);
    assert_eq!(app.room_password, "first-room");
    app.switch_tab(1);
    assert_eq!(app.room_password, "second-room");
}

fn book(number: f64) -> Workbook {
    let mut wb = Workbook::new_blank();
    put(&mut wb, number);
    wb.dirty = false;
    wb
}

fn delayed_save(app: &mut SheetApp, target: &str) -> Sender<Result<(sheetcore::xlsx::SaveReceipt, Option<CalculatedSnapshot>), String>> {
    let (tx, rx) = mpsc::channel();
    app.tasks.saving = Some(document_tasks::PendingSave {
        used_dirty_graph: false,
        target: target.into(),
        rx,
        quiet: false,
        host: None,
    });
    app.wb.dirty = false;
    tx
}

#[test]
fn update_restart_waits_for_local_edits_collaboration_and_background_writes() {
    let _env = crate::test_env::TestEnv::new("restart-pending-work");
    let mut app = app();
    app.has_open_workbook = true;
    assert!(app.safe_to_restart_after_update());

    app.editing = Some(EditState::new("pending value".into()));
    assert!(!app.safe_to_restart_after_update());
    app.editing = None;
    app.pending_edit = Some("waiting for cell lock".into());
    assert!(!app.safe_to_restart_after_update());
    app.pending_edit = None;
    app.delegated_save_pending = Some((app.doc_uid, Instant::now()));
    assert!(!app.safe_to_restart_after_update());
    app.delegated_save_pending = None;
    put(&mut app.wb, 23.0);
    assert!(!app.safe_to_restart_after_update());

    let _pending_write = delayed_save(&mut app, "pending.xlsx");
    assert!(!app.safe_to_restart_after_update());
    app.open_in_new_tab(PathBuf::new());
    assert!(!app.safe_to_restart_after_update());
    app.switch_tab(0);
    app.tasks.saving = None;
    assert!(app.safe_to_restart_after_update());
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
    tx.send(Ok((sheetcore::xlsx::SaveReceipt::default(), None))).unwrap();
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
    b.send(Ok((sheetcore::xlsx::SaveReceipt::default(), None))).unwrap();
    app.poll_save();
    assert_eq!(app.wb.path, Some(PathBuf::from("b.xlsx")));
    assert!(parked(&app, 0).tasks.saving.is_some());
    app.switch_tab(0);
    a.send(Ok((sheetcore::xlsx::SaveReceipt::default(), None))).unwrap();
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
    let dir = crate::test_env::scratch("sc_app_xls_compat");
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
    tx.send(OpenMsg::Loaded(Box::new(book(10.0)), false, Box::new(sheetcore::xlsx::LoadDiag::default()), String::new())).unwrap();
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
    save.send(Ok((sheetcore::xlsx::SaveReceipt::default(), None))).unwrap();
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
fn recalculation_preserves_current_input_and_output_passwords() {
    let _env = crate::test_env::TestEnv::new("recalc-passwords");
    for manual in [false, true] {
        for removing in [false, true] {
            let mut app = app();
            let previous = sheetcore::crypto::EncryptionSettings::new("earlier");
            app.wb.encryption = Some(previous.clone());
            app.wb.source_encryption = Some(previous);
            let snapshot = app.wb.clone();
            app.wb.encryption = (!removing).then(|| sheetcore::crypto::EncryptionSettings::new("chosen"));
            app.wb.source_encryption = Some(sheetcore::crypto::EncryptionSettings::new("saved"));
            let (tx, rx) = mpsc::channel();
            app.tasks.open_rx = Some(rx);
            let wb = Box::new(snapshot);
            let deps = Box::new(sheetcore::formula::deps::Deps::empty());
            let msg = if manual {
                OpenMsg::RecalcedWb(wb, deps, app.wb_epoch, app.doc_gen, true)
            } else {
                OpenMsg::Recalced(wb, deps)
            };
            tx.send(msg).unwrap();
            app.poll_open();
            assert!(app.wb.encryption.as_ref().map(|e| e.password.as_str()) == (!removing).then_some("chosen"));
            assert!(app.wb.source_encryption.as_ref().is_some_and(|e| e.password == "saved"));
        }
    }
}

#[test]
fn opening_another_file_while_saving_preserves_the_saving_tab() {
    let mut app = app();
    let tx = delayed_save(&mut app, "a.xlsx");
    // The load may fail, but its result must belong to the new tab.
    app.open_path(std::env::temp_dir().join("sheetflow-no-such-doc-for-save-test.xlsx"));
    assert_eq!(app.tabs.len(), 2);
    assert_eq!(app.active_tab, 1);
    tx.send(Ok((sheetcore::xlsx::SaveReceipt::default(), None))).unwrap();
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
    tx.send(Ok((sheetcore::xlsx::SaveReceipt::default(), None))).unwrap();
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
    let _env = crate::test_env::TestEnv::new("failed-save-close-frame");
    let mut app = app();
    let tx = delayed_save(&mut app, "a.xlsx");
    app.open_in_new_tab(PathBuf::new());
    tx.send(Err("disk full".into())).unwrap();
    let ctx = egui::Context::default();
    let mut input = egui::RawInput::default();
    input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap()
        .events.push(egui::ViewportEvent::Close);
    let output = ctx.run(input, |ctx| {
        app.commit_edit_before_close(ctx);
        app.poll_save();
        app.advance_save_and_exit(ctx);
    });
    assert!(!app.has_pending_save());
    assert_eq!(app.tab_close_confirm, Some(parked(&app, 0).doc_uid));
    assert!(output.viewport_output[&egui::ViewportId::ROOT].commands.iter()
        .any(|command| matches!(command, egui::ViewportCommand::CancelClose)));
    assert!(parked(&app, 0).wb.dirty);
    assert!(app.status.contains("disk full"));
}

#[test]
fn pending_validation_input_survives_close_save_tab_switch_and_ai() {
    for style in [sheetcore::dv::DvStyle::Warning, sheetcore::dv::DvStyle::Information] {
        let mut app = app();
        app.open_in_new_tab(PathBuf::new());
        app.wb.sheets[0].dv.push(sheetcore::dv::DataValidation {
            ranges: vec![(0, 0, 0, 0)], kind: sheetcore::dv::DvKind::Whole,
            f1: "1".into(), f2: "10".into(), error_style: style, ..Default::default()
        });
        app.editing = Some(EditState::new("99".into()));
        let ctx = egui::Context::default();
        let mut input = egui::RawInput::default();
        input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().events.push(egui::ViewportEvent::Close);
        let output = ctx.run(input, |ctx| app.commit_edit_before_close(ctx));
        assert!(output.viewport_output[&egui::ViewportId::ROOT].commands.iter()
            .any(|cmd| matches!(cmd, egui::ViewportCommand::CancelClose)));
        app.save(false);
        app.switch_tab(0);
        app.toggle_ai_assistant(Some(true));
        app.start_ai_prompt("查看金额".into());
        assert_eq!(app.active_tab, 1);
        assert_eq!(app.dv_alert.as_ref().unwrap().buf, "99");
        assert!(!app.ai.busy && !app.has_pending_save());
    }
}

#[test]
fn save_and_exit_refuses_to_overwrite_another_open_document() {
    let env = crate::test_env::TestEnv::new("close-path-conflict");
    let path = env.path("shared-name.xlsx");
    let mut app = app();
    app.wb.path = Some(path.clone());
    app.open_in_new_tab(PathBuf::new());
    put(&mut app.wb, 22.0);
    app.wb.path = Some(path.clone());
    app.save_all_before_exit = true;
    app.advance_save_and_exit(&egui::Context::default());
    assert!(!app.has_pending_save() && !app.save_all_before_exit);
    assert!(app.wb.dirty && !path.exists());
    assert!(app.status.contains("其他标签页"));
}

#[test]
fn close_named_tab_during_a_save_flushes_later_edits_without_another_click() {
    let env = crate::test_env::TestEnv::new("close-tab-later-edit");
    let path = env.path("changing.xlsx");
    let mut app = app();
    app.autosave = false;
    app.wb.path = Some(path.clone());
    let tx = delayed_save(&mut app, path.to_str().unwrap());
    app.request_close_tab(0);
    assert!(app.has_open_workbook && app.tab_close_confirm.is_none());
    put(&mut app.wb, 45.0);
    tx.send(Ok((Default::default(), None))).unwrap();
    let ctx = egui::Context::default();
    let deadline = Instant::now() + Duration::from_secs(10);
    while app.has_open_workbook && Instant::now() < deadline {
        app.poll_save();
        app.advance_save_and_exit(&ctx);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!app.has_open_workbook);
    assert_eq!(sheetcore::xlsx::load(&path).unwrap().sheets[0].value(0, 0), Value::Number(45.0));
}

#[test]
fn close_window_with_expired_host_request_requires_a_decision_and_keeps_original() {
    let env = crate::test_env::TestEnv::new("close-host-timeout");
    let path = env.path("shared.xlsx");
    sheetcore::xlsx::save_atomic(&path, &book(12.0)).unwrap();
    let mut app = app();
    app.wb = sheetcore::xlsx::load(&path).unwrap();
    put(&mut app.wb, 99.0);
    app.shared_client_source = Some((app.doc_uid, path.clone()));
    app.delegated_save_pending = Some((app.doc_uid, Instant::now() - Duration::from_secs(9)));
    app.save_all_before_exit = true;
    app.advance_save_and_exit(&egui::Context::default());
    assert!(app.wb.dirty && !app.has_pending_save() && !app.save_all_before_exit);
    assert_eq!(app.tab_close_confirm, Some(app.doc_uid));
    assert!(app.close_save_error.as_deref().unwrap().contains("宿主"));
    app.save(false);
    assert!(app.wb.dirty && !app.has_pending_save() && app.status.contains("宿主"));
    assert_eq!(sheetcore::xlsx::load(&path).unwrap().sheets[0].value(0, 0), Value::Number(12.0));
}

#[test]
fn native_window_close_commits_the_edit_and_flushes_the_named_file_without_a_prompt() {
    let env = crate::test_env::TestEnv::new("window-event-close");
    for autosave in [false, true] {
        let path = env.path(&format!("auto-{autosave}.xlsx"));
        let mut app = app();
        app.autosave = autosave;
        app.wb.path = Some(path.clone());
        app.editing = Some(EditState::new("52".into()));
        let ctx = egui::Context::default();
        let mut raw = egui::RawInput::default();
        raw.viewports.get_mut(&egui::ViewportId::ROOT).unwrap().events.push(egui::ViewportEvent::Close);
        let closing = ctx.run(raw, |ctx| {
            app.commit_edit_before_close(ctx);
            app.advance_save_and_exit(ctx);
        });
        assert!(closing.viewport_output[&egui::ViewportId::ROOT].commands.contains(&egui::ViewportCommand::CancelClose));
        assert!(app.has_pending_save() && app.tab_close_confirm.is_none());
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut closed = false;
        while !closed && Instant::now() < deadline {
            app.poll_save();
            let frame = ctx.run(Default::default(), |ctx| app.advance_save_and_exit(ctx));
            closed = frame.viewport_output[&egui::ViewportId::ROOT].commands.contains(&egui::ViewportCommand::Close);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(closed && app.tab_close_confirm.is_none());
        assert_eq!(sheetcore::xlsx::load(&path).unwrap().sheets[0].value(0, 0), Value::Number(52.0));
    }
}

#[test]
fn close_unnamed_tab_waits_for_its_first_path() {
    let env = crate::test_env::TestEnv::new("close-new-tab");
    let path = env.path("first.xlsx");
    let mut app = app();
    put(&mut app.wb, 78.0);
    app.request_close_tab(0);
    assert_eq!(app.tab_close_confirm, Some(app.doc_uid));
    assert!(app.wb.dirty && !app.has_pending_save());
    env.sc("SC_SAVE_AS", path.to_str().unwrap());
    let ctx = egui::Context::default();
    let input = egui::RawInput {
        events: vec![egui::Event::Key {
            key: egui::Key::Enter, physical_key: None, pressed: true, repeat: false,
            modifiers: egui::Modifiers::NONE,
        }],
        ..Default::default()
    };
    let _ = ctx.run(input, |ctx| app.tab_close_confirm_dialog(ctx));
    let deadline = Instant::now() + Duration::from_secs(10);
    while app.has_open_workbook && Instant::now() < deadline {
        app.poll_save();
        app.advance_save_and_exit(&ctx);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(!app.has_open_workbook);
    assert_eq!(sheetcore::xlsx::load(&path).unwrap().sheets[0].value(0, 0), Value::Number(78.0));
}

#[test]
fn save_and_exit_waits_for_dirty_background_documents_and_newer_edits() {
    let env = crate::test_env::TestEnv::new("save-all-close");
    let mut app = app();
    let a = env.path("a.xlsx");
    let b = env.path("b.xlsx");
    app.wb.path = Some(a.clone());
    put(&mut app.wb, 11.0);
    app.open_in_new_tab(PathBuf::new());
    app.wb.path = Some(b.clone());
    put(&mut app.wb, 22.0);
    let ctx = egui::Context::default();
    app.save_all_before_exit = true;
    app.advance_save_and_exit(&ctx);
    assert!(app.has_pending_save());
    put(&mut app.wb, 23.0);
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut closed = false;
    while app.save_all_before_exit && Instant::now() < deadline {
        app.poll_save();
        let output = ctx.run(Default::default(), |ctx| app.advance_save_and_exit(ctx));
        closed |= output.viewport_output[&egui::ViewportId::ROOT].commands.iter()
            .any(|cmd| matches!(cmd, egui::ViewportCommand::Close));
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(closed && !app.has_pending_save());
    assert_eq!(sheetcore::xlsx::load(&a).unwrap().sheets[0].value(0, 0), Value::Number(11.0));
    assert_eq!(sheetcore::xlsx::load(&b).unwrap().sheets[0].value(0, 0), Value::Number(23.0));
}

#[test]
fn save_and_exit_stops_on_background_failure_and_preserves_dirty_workbook() {
    let mut app = app();
    let tx = delayed_save(&mut app, "background.xlsx");
    app.open_in_new_tab(PathBuf::new());
    app.save_all_before_exit = true;
    tx.send(Err("disk full".into())).unwrap();
    app.poll_save();
    let ctx = egui::Context::default();
    let output = ctx.run(Default::default(), |ctx| app.advance_save_and_exit(ctx));
    assert!(!app.save_all_before_exit);
    assert!(parked(&app, 0).wb.dirty);
    assert!(app.status.contains("disk full"));
    assert!(!output.viewport_output[&egui::ViewportId::ROOT].commands.iter()
        .any(|cmd| matches!(cmd, egui::ViewportCommand::Close)));
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
    let dir = crate::test_env::scratch("sheetflow-app-save");
    let path = dir.join("book.xlsx");
    sheetcore::xlsx::save_atomic(&path, &book(11.0)).unwrap();
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
    assert!(sheetcore::backup::list(&path).is_empty(), "Ctrl+S must not create automatic backup copies");
    assert!(!dir.join(".~sheetflow-history").exists());
    assert!(sheetcore::backup::list(&path.with_extension("xlsx.tmp")).is_empty());
}

#[test]
fn xls_save_as_uses_native_format_history_and_its_owning_tab() {
    let dir = crate::test_env::scratch("sheetflow-app-xls-save");
    let source = dir.join("source.xlsx");
    let target = dir.join("book.XLS");
    sheetcore::xlsx::save_atomic(&source, &book(11.0)).unwrap();
    sheetcore::xls_write::write(&target, &book(10.0)).unwrap();
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
    assert!(sheetcore::backup::list(&target).is_empty());
    assert!(!dir.join(".~sheetflow-history").exists());
}

#[test]
fn xls_save_with_formula_succeeds() {
    let dir = crate::test_env::scratch("sheetflow-app-xls-formula");
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


#[test]
fn compatibility_warning_is_visible_on_open() {
    let mut app = app();
    let (tx, rx) = mpsc::channel();
    app.tasks.open_rx = Some(rx);
    app.tasks.busy = true;
    app.open_in_new_tab(PathBuf::new());
    let mut diag = sheetcore::xlsx::LoadDiag::default();
    diag.push(
        Some("Other"),
        "xl/worksheets/sheet1.xml",
        "兼容提示",
        "外部工作簿三维引用暂不参与重算；保留文件缓存".into(),
        false,
    );
    tx.send(OpenMsg::Loaded(Box::new(book(1.0)), false, Box::new(diag), String::new())).unwrap();
    app.switch_tab(0);
    app.poll_open();
    assert!(app.show_load_diag, "兼容诊断必须可见，不能只在状态栏一闪而过");
    assert!(app.load_diag.as_ref().is_some_and(|d| d.problems.iter().any(|p| p.what == "兼容提示")));
    assert!(!app.has_pending_save());
}


#[test]
fn load_diag_close_button_and_window_open_dismiss_modal() {
    let mut app = app();
    let mut diag = sheetcore::xlsx::LoadDiag::default();
    diag.push(
        Some("Sheet1"),
        "xl/worksheets/sheet1.xml",
        "读取警告",
        "示例非致命诊断".into(),
        false,
    );
    app.load_diag = Some(diag);
    app.show_load_diag = true;

    let ctx = egui::Context::default();
    fn run_diag_frame(app: &mut SheetApp, ctx: &egui::Context, events: Vec<egui::Event>) {
        let mut raw = egui::RawInput::default();
        raw.screen_rect = Some(egui::Rect::from_min_size(
            egui::pos2(0.0, 0.0),
            egui::vec2(1000.0, 720.0),
        ));
        raw.events = events;
        ctx.run(raw, |ctx| {
            app.load_diag_ui(ctx);
        });
    }

    // 第 1 帧：弹窗可见且打上对话框标记
    run_diag_frame(&mut app, &ctx, vec![]);
    assert!(ui_kit::take_dialog_shown(&ctx), "第 1 帧应渲染工作簿加载诊断");

    // Escape 关闭后，下一帧不应再渲染窗口（即使 self.load_diag 仍保留供状态栏重开）
    run_diag_frame(&mut app, &ctx, vec![egui::Event::Key {
        key: egui::Key::Escape,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    }]);
    assert!(!app.show_load_diag, "Escape 应关闭诊断弹窗");
    let _ = ui_kit::take_dialog_shown(&ctx);
    run_diag_frame(&mut app, &ctx, vec![]);
    assert!(!ui_kit::take_dialog_shown(&ctx), "关闭后的下一帧不得继续渲染弹窗");
    assert!(app.load_diag.is_some(), "保留 diag 以便状态栏再次查看");

    // 通过 ui_kit::window(.open(&mut false)) 验证即使直接传 false 也不渲染
    let mut forced_closed = false;
    let mut raw = egui::RawInput::default();
    raw.screen_rect = Some(egui::Rect::from_min_size(
        egui::pos2(0.0, 0.0),
        egui::vec2(1000.0, 720.0),
    ));
    let mut body_ran = false;
    ctx.run(raw, |ctx| {
        ui_kit::window(ctx, "工作簿加载诊断", 560.0)
            .open(&mut forced_closed)
            .show(ctx, |_| {
                body_ran = true;
            });
    });
    assert!(!body_ran, "open=false 时 Dlg::show 必须直接返回且不渲染窗口");
    assert!(!ui_kit::take_dialog_shown(&ctx));

    // 点击底部「关闭」按钮（Tab / Enter 命中主按钮「关闭」）同样必须关闭弹窗并清模态
    app.show_load_diag = true;
    run_diag_frame(&mut app, &ctx, vec![]);
    assert!(ui_kit::take_dialog_shown(&ctx));
    run_diag_frame(&mut app, &ctx, vec![egui::Event::Key {
        key: egui::Key::Enter,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    }]);
    assert!(!app.show_load_diag, "点击/触发「关闭」主按钮必须关闭工作簿加载诊断");
    let _ = ui_kit::take_dialog_shown(&ctx);
    run_diag_frame(&mut app, &ctx, vec![]);
    assert!(!ui_kit::take_dialog_shown(&ctx), "点击关闭后不得残留对话框");
}


#[test]
fn actual_external_3d_fixture_opens_cleanly_without_compatibility_warning() {
    let mut app = app();
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../core/tests/fixtures/c11_external_3d.xlsx");
    app.open_path(fixture);
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && (app.tasks.busy || app.tasks.open_rx.is_some()) {
        app.poll_open();
        std::thread::sleep(Duration::from_millis(20));
    }
    app.poll_open();
    assert!(!app.show_load_diag, "原生支持外部三维引用后不应再弹加载诊断警告");
    assert!(app.load_diag.is_none(), "c11_external_3d.xlsx 应完整加载无警告：{:?}", app.load_diag);
    let cell = app.wb.sheets[0].get(0, 0).expect("fixture A1");
    assert_eq!(cell.formula.as_deref(), Some("=SUM([1]First:Third!A1)"));
    assert_eq!(cell.value, Value::Number(60.0));
}


#[test]
fn poll_open_requests_opens_distinct_file_in_new_tab_and_switches_to_parked_duplicate() {
    let env = crate::test_env::TestEnv::new("single_inst_tabs");
    let dir = env.path("docs");
    std::fs::create_dir_all(&dir).unwrap();
    let a = dir.join("a.xlsx");
    let b = dir.join("b.xlsx");
    let c = dir.join("c.xlsx");
    sheetcore::xlsx::save(&a, &book(11.0)).unwrap();
    sheetcore::xlsx::save(&b, &book(22.0)).unwrap();
    sheetcore::xlsx::save(&c, &book(33.0)).unwrap();

    let ctx = egui::Context::default();
    let mut app = app();
    let drain_open = |app: &mut SheetApp| {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && (app.tasks.busy || app.tasks.open_rx.is_some()) {
            app.poll_open();
            std::thread::sleep(Duration::from_millis(15));
        }
        app.poll_open();
    };

    app.open_path(a.clone());
    drain_open(&mut app);
    app.open_in_new_tab(b.clone());
    drain_open(&mut app);
    assert_eq!(app.tabs.len(), 2);
    assert_eq!(app.active_tab, 1);

    let (tx, rx) = mpsc::channel();
    app.open_requests = Some(rx);

    // 1. 重复双击已在后台标签页 0 打开的 a.xlsx：切回标签 0，不得再开新标签
    tx.send(a.clone()).unwrap();
    app.poll_open_requests(&ctx);
    assert_eq!(app.tabs.len(), 2);
    assert_eq!(app.active_tab, 0);
    assert_eq!(app.wb.path.as_ref(), Some(&a));

    // 2. 双击全新文件 c.xlsx：自动并入第 3 个标签页并激活
    tx.send(c.clone()).unwrap();
    app.poll_open_requests(&ctx);
    drain_open(&mut app);
    assert_eq!(app.tabs.len(), 3);
    assert_eq!(app.active_tab, 2);
    assert_eq!(app.wb.path.as_ref(), Some(&c));
}


#[test]
fn append_crash_log_writes_version_and_diagnostic_to_app_dir() {
    let _env = crate::test_env::TestEnv::new("crash_log");
    let path = crate::append_crash_log("STARTUP_ERROR", "mock gl error")
        .expect("crash.log path");
    assert!(path.is_file());
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains(crate::update::APP_NAME));
    assert!(text.contains(crate::update::VERSION));
    assert!(text.contains("STARTUP_ERROR"));
    assert!(text.contains("mock gl error"));
}

#[test]
fn dual_host_guards_block_duplicate_manual_host_resolve_split_brain_and_converge_on_promote() {
    let env = crate::test_env::TestEnv::new("dual-host-guards");
    // 强制使用本机回环地址写入占用卡，确保测试在任何网卡环境下可确定性连入
    unsafe {
        std::env::set_var("SC_HOST_ADDR", "127.0.0.1");
    }
    let path = env.path("team-budget.xlsx");
    sheetcore::xlsx::save(&path, &book(10.0)).unwrap();

    // 1. 甲开启共享后，乙在同一文件上手动点「开启共享」不得再起第二个宿主，且不会覆盖甲的占用卡
    let mut host_a = app();
    host_a.my_name = "甲".into();
    host_a.wb.path = Some(path.clone());
    host_a.room_password = "room-secret".into();
    host_a.start_host();
    assert!(host_a.net.as_ref().is_some_and(|n| n.hosting), "甲应当成功开房");
    let lock_a = session_lock::read(&path).expect("甲应写出占用卡");
    assert!(!lock_a.session.is_empty());

    let mut peer_b = app();
    peer_b.my_name = "乙".into();
    peer_b.wb.path = Some(path.clone());
    peer_b.room_password = "room-secret".into();
    peer_b.start_host();
    assert!(peer_b.net.is_none(), "已有活跃宿主时乙手动开启共享必须被阻止");
    assert_eq!(peer_b.join_addr, lock_a.host, "应自动为乙填入现有宿主地址");
    assert_eq!(
        session_lock::read(&path).unwrap().session,
        lock_a.session,
        "甲的占用卡不得被乙改写或删除"
    );

    // 乙与丙正常加入甲的房间
    peer_b.join_shared();
    let mut peer_c = app();
    peer_c.my_name = "丙".into();
    peer_c.wb.path = Some(path.clone());
    peer_c.room_password = "room-secret".into();
    peer_c.join_addr = lock_a.host.clone();
    peer_c.join_shared();
    for _ in 0..40 {
        host_a.poll_net();
        peer_b.poll_net();
        peer_c.poll_net();
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(peer_b.joined_doc && peer_c.joined_doc, "乙和丙应完成入房同步");

    // 2. 模拟甲突然崩溃（监听端口立即关闭，但磁盘上的占用卡仍在 20s 新鲜期内）：
    //    乙和丙先后触发 promote_to_host，乙接任新宿主，丙探测到乙已接任后自动转连乙而非双开宿主！
    let dead_net = host_a.net.take().unwrap();
    dead_net.shutdown();
    drop(dead_net);
    std::thread::sleep(Duration::from_millis(80));

    peer_b.promote_to_host();
    assert!(peer_b.net.as_ref().is_some_and(|n| n.hosting), "乙应成功接任新宿主");
    let lock_b = session_lock::read(&path).expect("乙应写出新占用卡");
    assert_ne!(lock_b.session, lock_a.session);

    peer_c.promote_to_host();
    assert!(
        peer_c.net.as_ref().is_some_and(|n| !n.hosting),
        "丙在乙已接任后调用 promote_to_host 应自动加入乙，不能形成双宿主"
    );
    assert_eq!(peer_c.join_addr, lock_b.host);

    // 3. 模拟网络分区恢复后的双宿主脑裂：强制构造一个晚启动的冲突宿主丁，验证心跳裁决与覆盖保存拦截
    let listener_d = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let rogue_session = session_lock::new_session_id();
    let rogue_started = lock_b.started_ms.saturating_add(5_000);
    let rogue_net = crate::net::host_on_with_session(
        listener_d,
        77,
        "丁".into(),
        "team-budget.xlsx".into(),
        "room-secret".into(),
        rogue_session,
        rogue_started,
    )
    .unwrap();
    let mut rogue_d = app();
    rogue_d.my_name = "丁".into();
    rogue_d.wb = book(999.0);
    rogue_d.wb.path = Some(path.clone());
    rogue_d.wb.dirty = true;
    rogue_d.net = Some(rogue_net);
    rogue_d.lock_beat = None;

    rogue_d.beat_lock();
    assert!(rogue_d.net.is_none(), "晚启动的冲突宿主丁在心跳巡检时必须自动降级退出");
    assert_eq!(
        rogue_d.wb.sheets[0].value(0, 0),
        Value::Number(999.0),
        "降级后丁的本地未保存修改必须完好保留在内存中"
    );
    assert_eq!(
        session_lock::read(&path).unwrap().session,
        lock_b.session,
        "丁降级或销毁时绝不能覆盖或删除主宿主乙的占用卡"
    );

    // 丁在未加入乙会话的情况下直接点保存，必须被拦截以免覆盖乙正在主持的原文件
    rogue_d.save(false);
    let deadline = Instant::now() + Duration::from_secs(5);
    while rogue_d.has_pending_save() && Instant::now() < deadline {
        rogue_d.poll_save();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(rogue_d.wb.dirty, "拒绝覆盖后应保留所有未保存的编辑");
    assert!(rogue_d.tasks.saving.is_none(), "离线/已降级实例不得直接覆盖活跃宿主的共享原文件");
    assert!(rogue_d.status.contains("另存为"), "状态栏应提示另存为或先加入会话：{}", rogue_d.status);

    peer_c.leave_session();
    peer_b.leave_session();
    unsafe {
        std::env::remove_var("SC_HOST_ADDR");
    }
}

#[test]
fn large_workbook_syncs_editable_immediately_and_rapid_back_to_back_edits_propagate_live() {
    let env = crate::test_env::TestEnv::new("large-wb-realtime-edits");
    unsafe {
        std::env::set_var("SC_HOST_ADDR", "127.0.0.1");
    }
    let dir = env.path("shared-large");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("large.xlsx");

    let mut wb = Workbook::new_blank();
    for r in 0..1_000u32 {
        for c in 0..10u32 {
            wb.sheets[0].set(
                r,
                c,
                Cell {
                    value: Value::Number((r * 10 + c) as f64),
                    formula: None,
                },
            );
        }
    }
    wb.dirty = false;
    sheetcore::xlsx::save(&path, &wb).unwrap();
    session_lock::set_dir_marked_shared(&path, true).unwrap();

    let mut host_a = app();
    host_a.my_name = "甲".into();
    host_a.wb = wb.clone();
    host_a.wb.path = Some(path.clone());
    host_a.try_auto_join();
    assert!(host_a.net.as_ref().is_some_and(|n| n.hosting));

    let mut peer_b = app();
    peer_b.my_name = "乙".into();
    peer_b.wb = Workbook::new_blank();
    peer_b.wb.path = Some(path.clone());
    peer_b.try_auto_join();

    for _ in 0..60 {
        host_a.poll_net();
        peer_b.poll_net();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(peer_b.joined_doc, "10,000 格大表应完成入房快照同步");
    assert!(
        !peer_b.deps_pending && !host_a.deps_pending,
        "两台机器均不得被 deps_pending 锁死编辑"
    );

    // 乙在同一帧内连续编辑 A1、回车跳到 A2 立即编辑、再回车跳到 A3 写公式 =A1+A2（0 网络等待延迟）
    peer_b.sel = (0, 0);
    peer_b.sel_end = (0, 0);
    peer_b.begin_edit(Some("120".into()));
    assert!(peer_b.editing.is_some(), "第 1 格应零延迟进入编辑态");
    assert!(peer_b.commit_edit(Some((1, 0))));
    assert_eq!(peer_b.sel, (1, 0));

    peer_b.begin_edit(Some("80".into()));
    assert!(peer_b.editing.is_some(), "编辑完上一格后下一格应无需等待直接进入编辑态");
    assert!(peer_b.commit_edit(Some((1, 0))));
    assert_eq!(peer_b.sel, (2, 0));

    peer_b.begin_edit(Some("=A1+A2".into()));
    assert!(peer_b.editing.is_some(), "第 3 格公式也应零延迟进入编辑态");
    assert!(peer_b.commit_edit(Some((1, 0))));
    assert_eq!(peer_b.wb.sheets[0].value(2, 0), Value::Number(200.0));

    for _ in 0..40 {
        host_a.poll_net();
        peer_b.poll_net();
        std::thread::sleep(Duration::from_millis(15));
    }
    assert_eq!(host_a.wb.sheets[0].value(0, 0), Value::Number(120.0));
    assert_eq!(host_a.wb.sheets[0].value(1, 0), Value::Number(80.0));
    assert_eq!(
        host_a.wb.sheets[0].value(2, 0),
        Value::Number(200.0),
        "甲应实时看到乙连续编辑的数值与公式计算结果"
    );

    // 甲反过来连续编辑 B1、B2，乙也应实时收到
    host_a.sel = (0, 1);
    host_a.sel_end = (0, 1);
    host_a.begin_edit(Some("555".into()));
    assert!(host_a.editing.is_some());
    assert!(host_a.commit_edit(Some((1, 0))));
    host_a.begin_edit(Some("666".into()));
    assert!(host_a.editing.is_some());
    assert!(host_a.commit_edit(Some((1, 0))));

    for _ in 0..40 {
        host_a.poll_net();
        peer_b.poll_net();
        std::thread::sleep(Duration::from_millis(15));
    }
    assert_eq!(peer_b.wb.sheets[0].value(0, 1), Value::Number(555.0));
    assert_eq!(peer_b.wb.sheets[0].value(1, 1), Value::Number(666.0));

    peer_b.leave_session();
    host_a.leave_session();
    unsafe {
        std::env::remove_var("SC_HOST_ADDR");
    }
}

#[test]
fn real_world_heavy_cf_and_formulas_delta_sync_and_sub_15ms_live_edits() {
    let env = crate::test_env::TestEnv::new("heavy-cf-formula-delta");
    unsafe {
        std::env::set_var("SC_HOST_ADDR", "127.0.0.1");
    }
    let dir = env.path("lan-heavy");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("heavy_ledger.xlsx");

    let mut wb = Workbook::new_blank();
    wb.names.push(("TAX_RATE".into(), None, "0.13".into()));
    {
        let sh = wb.sheet_mut(0);
        for r in 0..3_000u32 {
            sh.set(
                r,
                0,
                Cell {
                    value: Value::Text(format!("DEPT-{}", r % 40).into()),
                    formula: None,
                },
            );
            sh.set(
                r,
                1,
                Cell {
                    value: Value::Number((r + 1) as f64 * 10.0),
                    formula: None,
                },
            );
            sh.set(
                r,
                2,
                Cell {
                    value: Value::Number((r + 1) as f64 * 1.3),
                    formula: Some(format!("=B{}*TAX_RATE", r + 1).into()),
                },
            );
            sh.set(
                r,
                3,
                Cell {
                    value: Value::Number((r + 1) as f64 * 11.3),
                    formula: Some(format!("=B{}+C{}", r + 1, r + 1).into()),
                },
            );
        }
        // 整列百万行条件格式（重复值高亮 + 三色阶）：复现真实财务表最重卡顿源
        sh.cf.dxfs.push(sheetcore::cf::Dxf {
            fill: Some([255, 199, 206]),
            color: Some([156, 0, 6]),
            ..Default::default()
        });
        sh.cf.blocks.push(sheetcore::cf::CfBlock {
            ranges: vec![(0, 0, 1_048_575, 0)],
            rules: vec![sheetcore::cf::CfRule {
                kind: sheetcore::cf::CfKind::Dup {
                    unique: false,
                    dxf: 0,
                },
                stop: false,
                priority: 1,
            }],
        });
        sh.cf.blocks.push(sheetcore::cf::CfBlock {
            ranges: vec![(0, 1, 1_048_575, 3)],
            rules: vec![sheetcore::cf::CfRule {
                kind: sheetcore::cf::CfKind::Scale {
                    points: vec![
                        (sheetcore::cf::Cfvo::min(), [248, 105, 107]),
                        (sheetcore::cf::Cfvo::max(), [99, 190, 123]),
                    ],
                },
                stop: false,
                priority: 2,
            }],
        });
        sh.cf_dirty = true;
    }
    wb.dirty = false;
    sheetcore::xlsx::save(&path, &wb).unwrap();
    session_lock::set_dir_marked_shared(&path, true).unwrap();

    // 1. 甲通过真实 open_path 打开大表：Loaded 一到达即解除 busy 遮罩，无需等待后台全表重算
    let mut host_a = app();
    host_a.my_name = "甲".into();
    host_a.open_path(path.clone());
    let deadline = std::time::Instant::now() + Duration::from_secs(6);
    while host_a.wb.sheet(0).len() == 0 && std::time::Instant::now() < deadline {
        host_a.poll_open();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(host_a.wb.sheet(0).len(), 12_000);
    assert!(
        !host_a.tasks.busy && host_a.tasks.open_label.is_none(),
        "Loaded 到达后必须立即解除正在打开遮罩"
    );
    assert!(!host_a.wb_base_hash.is_empty(), "应记录磁盘基准文件指纹");
    assert!(host_a.net.as_ref().is_some_and(|n| n.hosting));

    // 2. 乙同样通过真实 open_path 打开同一共享文件：应命中同文件 0 冗余增量握手，不清空已读 12,000 格
    let mut peer_b = app();
    peer_b.my_name = "乙".into();
    peer_b.open_path(path.clone());
    let deadline = std::time::Instant::now() + Duration::from_secs(6);
    while peer_b.wb.sheet(0).len() == 0 && std::time::Instant::now() < deadline {
        peer_b.poll_open();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(peer_b.wb.sheet(0).len(), 12_000);
    assert!(!peer_b.tasks.busy && peer_b.tasks.open_label.is_none());

    let t_sync = std::time::Instant::now();
    while !peer_b.joined_doc && t_sync.elapsed() < Duration::from_secs(2) {
        host_a.poll_net();
        peer_b.poll_net();
        assert_eq!(
            peer_b.wb.sheet(0).len(),
            12_000,
            "同文件增量握手期间不得把乙的工作表清空闪白"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(peer_b.joined_doc && peer_b.snapshot_is_delta, "应通过增量握手极速入房");

    // 3. 验证百万行整列条件格式 compute_stats + 视口 600 格 overlay_at 耗时远低于单帧阈值
    let t_cf = std::time::Instant::now();
    let mut st = sheetcore::cf::CfStats::default();
    {
        let sh = host_a.wb.sheet(0);
        sheetcore::cf::compute_stats(&sh.cf, sh, &mut st);
        for rr in 0..50u32 {
            for cc in 0..12u32 {
                let _ = sheetcore::cf::overlay_at(&sh.cf, &st, sh, rr, cc, |_, _, _| false);
            }
        }
    }
    let cf_ms = t_cf.elapsed().as_millis();
    assert!(
        cf_ms < 80,
        "整列百万行条件格式统计+视口叠加必须在毫秒级完成，实际耗时 {cf_ms}ms"
    );

    // 4. 连续快速编辑多格（含定义名称工作簿）：本地单次 commit 必须极速完成且不置 tasks.busy，对端实时可见且无孤儿锁残留
    peer_b.sel = (0, 1);
    peer_b.sel_end = (0, 1);
    for step in 0..5u32 {
        let t_edit = std::time::Instant::now();
        peer_b.begin_edit(Some(format!("{}", (step + 1) * 1000)));
        assert_eq!(
            peer_b.editing.as_ref().and_then(|e| e.caret_override),
            Some(4),
            "新开启编辑框必须将光标定位于末尾"
        );
        assert!(peer_b.commit_edit(Some((1, 0))));
        let edit_ms = t_edit.elapsed().as_millis();
        assert!(
            edit_ms < 50,
            "含全局名称与 6000 公式工作簿上的单格提交必须毫秒级返回，第 {step} 步耗时 {edit_ms}ms"
        );
        assert!(!peer_b.tasks.busy, "连续编辑绝不能触发模态重算遮罩");
    }

    let t_prop = std::time::Instant::now();
    while host_a.wb.sheet(0).value(4, 1) != Value::Number(5000.0)
        && t_prop.elapsed() < Duration::from_millis(500)
    {
        host_a.poll_net();
        peer_b.poll_net();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(host_a.wb.sheet(0).value(0, 1), Value::Number(1000.0));
    assert_eq!(host_a.wb.sheet(0).value(4, 1), Value::Number(5000.0));
    // 确保迟到 LockOk 被立即自动退还，甲的锁表里不得残留乙已经离开的旧单元格锁
    for _ in 0..10 {
        host_a.poll_net();
        peer_b.poll_net();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        host_a.locks.is_empty() && peer_b.my_lock.is_none(),
        "快速连续回车切格后不得残留任何孤儿单元格编辑锁"
    );

    peer_b.leave_session();
    host_a.leave_session();
    unsafe {
        std::env::remove_var("SC_HOST_ADDR");
    }
}

#[test]
fn collaborative_edits_save_promptly_to_original_file_with_zero_sidecar_pollution() {
    let env = crate::test_env::TestEnv::new("collab-prompt-save-no-sidecar");
    unsafe {
        std::env::set_var("SC_HOST_ADDR", "127.0.0.1");
    }
    let shared_dir = env.path("shared-folder");
    std::fs::create_dir_all(&shared_dir).unwrap();
    let shared_path = shared_dir.join("愉悦定金确认明细表-20261120.xlsx");
    sheetcore::xlsx::save_atomic(&shared_path, &book(100.0)).unwrap();
    session_lock::set_dir_marked_shared(&shared_path, true).unwrap();

    let mut host_a = app();
    host_a.my_name = "甲".into();
    host_a.wb = sheetcore::xlsx::load(&shared_path).unwrap();
    host_a.try_auto_join();
    assert!(host_a.net.as_ref().is_some_and(|n| n.hosting));

    let mut peer_b = app();
    peer_b.my_name = "乙".into();
    peer_b.wb = sheetcore::xlsx::load(&shared_path).unwrap();
    peer_b.try_auto_join();

    for _ in 0..50 {
        host_a.poll_net();
        peer_b.poll_net();
        if peer_b.joined_doc {
            break;
        }
        std::thread::sleep(Duration::from_millis(15));
    }
    assert!(peer_b.joined_doc);

    // 1. 协作者乙编辑 A1 并按 Ctrl+S（save(false)），委托宿主甲立即落盘原文件并同步清除双方 dirty
    peer_b.sel = (0, 0);
    peer_b.sel_end = (0, 0);
    peer_b.begin_edit(Some("520".into()));
    assert!(peer_b.commit_edit(None));
    assert!(peer_b.wb.dirty);
    peer_b.save(false);
    assert!(
        peer_b.tasks.saving.is_none(),
        "客户端不应直接并发抢写共享原文件，而应委托宿主统一保存"
    );

    let deadline = Instant::now() + Duration::from_secs(10);
    while (host_a.wb.dirty || peer_b.wb.dirty || host_a.tasks.saving.is_some())
        && Instant::now() < deadline
    {
        host_a.poll_net();
        host_a.poll_save();
        peer_b.poll_net();
        std::thread::sleep(Duration::from_millis(15));
    }
    assert!(!host_a.wb.dirty && !peer_b.wb.dirty, "宿主落盘广播后双方 dirty 均应归零");
    assert_eq!(
        sheetcore::xlsx::load(&shared_path).unwrap().sheets[0].value(0, 0),
        Value::Number(520.0)
    );

    // 2. 协作者乙继续编辑 A2 且双方均不按 Ctrl+S：宿主自动将新值写回原文件
    peer_b.sel = (1, 0);
    peer_b.sel_end = (1, 0);
    peer_b.begin_edit(Some("880".into()));
    assert!(peer_b.commit_edit(None));
    for _ in 0..30 {
        host_a.poll_net();
        peer_b.poll_net();
        if host_a.wb.sheets[0].value(1, 0) == Value::Number(880.0) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(host_a.wb.dirty && peer_b.wb.dirty);
    host_a.tasks.last_edit = Some(Instant::now() - Duration::from_secs(4));
    host_a.tasks.shared_autosave_paused = true;
    host_a.tick_autosave(None);
    assert!(host_a.tasks.saving.is_none() && host_a.wb.dirty);
    assert_eq!(sheetcore::xlsx::load(&shared_path).unwrap().sheet(0).value(1, 0), Value::Empty);
    host_a.tasks.shared_autosave_paused = false;
    host_a.tick_autosave(None);
    assert!(host_a.tasks.saving.is_some(), "协作宿主停手后应自动启动后台保存");

    let deadline = Instant::now() + Duration::from_secs(10);
    while (host_a.wb.dirty || peer_b.wb.dirty || host_a.tasks.saving.is_some())
        && Instant::now() < deadline
    {
        host_a.poll_net();
        host_a.poll_save();
        peer_b.poll_net();
        std::thread::sleep(Duration::from_millis(15));
    }
    assert!(!host_a.wb.dirty && !peer_b.wb.dirty);
    let reloaded = sheetcore::xlsx::load(&shared_path).unwrap();
    assert_eq!(reloaded.sheets[0].value(0, 0), Value::Number(520.0));
    assert_eq!(reloaded.sheets[0].value(1, 0), Value::Number(880.0));

    // A host receipt can acknowledge committed cells while the next input is still a draft.
    peer_b.sel = (2, 0);
    peer_b.sel_end = (2, 0);
    peer_b.begin_edit(Some("42".into()));
    assert!(peer_b.commit_edit(None));
    peer_b.sel = (3, 0);
    peer_b.sel_end = (3, 0);
    peer_b.begin_edit(Some("73".into()));
    peer_b.receive_host_save(peer_b.peer.as_ref().unwrap().clock);
    assert!(!peer_b.wb.dirty && peer_b.editing.as_ref().unwrap().buf == "73");
    assert_eq!(peer_b.wb.sheet(0).value(3, 0), Value::Empty);
    peer_b.cancel_edit();

    // The room never transmits page headers. A cell clock cannot acknowledge a header,
    // even when the same workbook also has a later cell which the room did transmit.
    peer_b.wb.sheets[0].page.header = "本地页眉".into();
    peer_b.wb.sheets[0].page_dirty = true;
    peer_b.wb.dirty = true;
    peer_b.receive_host_save(u64::MAX);
    assert!(peer_b.wb.dirty);
    peer_b.save(false);
    assert!(peer_b.delegated_save_pending.is_none() && !peer_b.has_pending_save());
    assert!(peer_b.status.contains("另存为"));
    peer_b.begin_edit(Some("73".into()));
    assert!(peer_b.commit_edit(None));
    peer_b.receive_host_save(u64::MAX);
    peer_b.request_close_tab(0);
    assert!(peer_b.wb.dirty && peer_b.has_open_workbook);
    assert_eq!(peer_b.tab_close_confirm, Some(peer_b.doc_uid));
    assert!(peer_b.close_save_error.as_ref().unwrap().contains("另存"));

    // The rescue copy now owns its own path. Future receipts for the room must leave it alone.
    let copy = env.path("client-copy.xlsx");
    env.sc("SC_SAVE_AS", copy.to_str().unwrap());
    peer_b.save_closing_document(0, true);
    let deadline = Instant::now() + Duration::from_secs(10);
    while peer_b.has_pending_save() && Instant::now() < deadline {
        peer_b.poll_save();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(peer_b.wb.path.as_ref(), Some(&copy));
    assert_eq!(sheetcore::xlsx::load(&copy).unwrap().sheet(0).page.header, "本地页眉");
    assert!(!peer_b.is_delegated_collab_client());
    peer_b.wb.dirty = true;
    peer_b.receive_host_save(u64::MAX);
    assert!(peer_b.wb.dirty);

    // A guest's parked copy never writes the original after its host leaves,
    // even if the owner card is gone and local autosave is enabled.
    let mut parked_guest = app();
    parked_guest.autosave = true;
    parked_guest.has_open_workbook = true;
    parked_guest.wb = sheetcore::xlsx::load(&shared_path).unwrap();
    parked_guest.try_auto_join();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !parked_guest.joined_doc && Instant::now() < deadline {
        host_a.poll_net();
        parked_guest.poll_net();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(parked_guest.is_delegated_collab_client());
    put(&mut parked_guest.wb, 777.0);
    parked_guest.tasks.last_edit = Some(Instant::now() - Duration::from_secs(1));
    parked_guest.open_in_new_tab(PathBuf::new());

    let host_copy = env.path("host-copy.xlsx");
    env.sc("SC_SAVE_AS", host_copy.to_str().unwrap());
    put(&mut host_a.wb, 444.0);
    host_a.tasks.shared_autosave_paused = true;
    host_a.save(true);
    wait_for_background_saves(&mut host_a);
    assert_ne!(host_a.tasks.autosave_after_detach, Some(false));
    assert!(host_a.net.is_none(), "a different file cannot confirm saves for the original room");
    assert_eq!(sheetcore::xlsx::load(&host_copy).unwrap().sheet(0).value(0, 0), Value::Number(444.0));
    assert!(!session_lock::lock_path(&shared_path).exists());
    parked_guest.tick_autosave(None);
    assert!(parked(&parked_guest, 0).wb.dirty && !parked_guest.has_pending_save());
    assert_eq!(sheetcore::xlsx::load(&shared_path).unwrap().sheet(0).value(0, 0), Value::Number(520.0));

    // 3. 原文件目录下绝不能残留 .~sheetflow-history、.saving-*、.lock 等副文件
    let names: Vec<String> = std::fs::read_dir(&shared_dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert!(!names.iter().any(|n| n == ".~sheetflow-history"), "unexpected history dir: {names:?}");
    assert!(!names.iter().any(|n| n.contains(".saving-")), "unexpected saving file: {names:?}");
    assert!(!names.iter().any(|n| n.starts_with(".~")), "unexpected sidecar file: {names:?}");

    unsafe {
        std::env::remove_var("SC_HOST_ADDR");
    }
}

#[test]
fn edits_made_while_disconnected_survive_the_rejoin_snapshot_and_reach_the_host() {
    let _env = crate::test_env::TestEnv::new("collab-rejoin-outbox");
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let host = crate::net::host_on(listener, 1, "甲".into(), "doc".into(), "room-pw".into()).unwrap();

    let mut b = app();
    b.my_name = "乙".into();
    b.room_password = "room-pw".into();
    b.join_addr = format!("127.0.0.1:{port}");
    b.join_shared();
    let t = Instant::now();
    while !b.joined_doc && t.elapsed() < Duration::from_secs(5) {
        b.poll_net();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(b.joined_doc, "乙应完成入房");
    while host.events.try_recv().is_ok() {}

    // 断线期间的编辑：本地副本与表格里有，但报文没送到宿主（等同于断线时被丢弃的帧）
    let cell = Cell { value: Value::Number(77.0), formula: None };
    let offline = b.peer.as_mut().unwrap().local_set(0, 5, 5, &cell);
    b.wb.sheet_mut(0).set(5, 5, cell);

    // 重连成功：宿主发来新的 Welcome 与完整快照（快照里没有这格）
    let net = b.net.as_ref().unwrap();
    net.inject_event_for_test(crate::net::NetEvent::Welcome {
        peers: vec![(1, "甲".into(), 1)],
        delta_sync: false,
        clock: 0,
    });
    net.inject_event_for_test(crate::net::NetEvent::SnapshotDone);
    b.poll_net();

    assert_eq!(
        b.wb.sheet(0).value(5, 5),
        Value::Number(77.0),
        "重连快照不得静默抹掉断线期间的编辑"
    );
    assert!(b.rejoin_outbox.is_none(), "补发后暂存应清空");

    // 宿主必须收到补发的这条编辑
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut got = false;
    while Instant::now() < deadline && !got {
        if let Ok(crate::net::NetEvent::Ops(ops)) = host.events.recv_timeout(Duration::from_millis(50)) {
            got = ops.iter().any(|o| {
                o.row == 5 && o.col == 5 && o.ts == offline.ts && o.value == sheetcore::sync::ValueWire::Number(77.0)
            });
        }
    }
    assert!(got, "断线期间的编辑应在重连后补发给宿主");

    // 宿主上更新的版本（别人断线期间改过同一格）必须胜出，不能被补发覆盖
    let stale = sheetcore::sync::Op {
        sheet: 0,
        row: 6,
        col: 6,
        value: sheetcore::sync::ValueWire::Number(1.0),
        formula: None,
        ts: 2,
        site: b.peer.as_ref().unwrap().site,
    };
    b.rejoin_outbox = Some((b.doc_uid, vec![stale]));
    b.peer.as_mut().unwrap().apply(sheetcore::sync::Op {
        sheet: 0,
        row: 6,
        col: 6,
        value: sheetcore::sync::ValueWire::Number(2.0),
        formula: None,
        ts: 1_000,
        site: 1,
    });
    b.wb.sheet_mut(0).set(6, 6, Cell { value: Value::Number(2.0), formula: None });
    b.replay_rejoin_outbox();
    assert_eq!(b.wb.sheet(0).value(6, 6), Value::Number(2.0), "宿主上更新的版本应胜出");

    b.leave_session();
    host.shutdown();
}
