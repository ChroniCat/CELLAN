use super::{SheetApp, sort_apps, sort_apps_with_baseline, until};
use std::sync::mpsc;

fn late_join(
    env: &crate::test_env::TestEnv,
    guest: &SheetApp,
    baseline: impl FnOnce(&mut sheetcore::Workbook),
) -> SheetApp {
    let mut app = SheetApp::new_with_context(&eframe::egui::Context::default(), None);
    app.wb.path = Some(env.path("late.xlsx"));
    baseline(&mut app.wb);
    let addr = &guest.net.as_ref().unwrap().addr;
    app.net = Some(crate::net::join(3, "Late".into(), "doc".into(), addr, "sort".into()).unwrap());
    app.peer = Some(sheetcore::sync::Peer::new(3));
    app.undo.set_site(3);
    app
}

#[test]
fn partial_cell_shift_room_new_join_sees_names_edited_after_room_creation() {
    let env = crate::test_env::TestEnv::new("shift-recent-names");
    let (mut host, guest) = sort_apps(&env);
    host.wb.names.push(("Amount".into(), None, "Sheet1!$B$4".into()));
    let mut late = late_join(&env, &guest, |_| {});
    until(&mut host, &mut late, |_, app| {
        app.joined_doc && !app.in_snapshot && app.wb.names.iter().any(|(name, _, formula)| {
            name == "Amount" && formula == "Sheet1!$B$4"
        })
    });
}

fn start_shift_paste(
    app: &mut SheetApp,
) -> mpsc::Sender<Result<sheetcore::clipboard_xmlss::SparseBlock, String>> {
    app.sel = (3, 0);
    app.sel_end = app.sel;
    app.begin_background_paste(false)
}

fn complete_shift_parser(
    sender: mpsc::Sender<Result<sheetcore::clipboard_xmlss::SparseBlock, String>>,
) {
    let xml = r#"<Workbook xmlns:ss="urn:schemas-microsoft-com:office:spreadsheet"><Worksheet><Table><Row><Cell><Data ss:Type="Number">42</Data></Cell></Row></Table></Worksheet></Workbook>"#;
    let block = sheetcore::clipboard_xmlss::parse_sparse_ext(xml, false).unwrap();
    sender.send(Ok(block)).unwrap();
}

fn finish_shift_paste(
    app: &mut SheetApp,
    sender: mpsc::Sender<Result<sheetcore::clipboard_xmlss::SparseBlock, String>>,
) {
    let history = app.undo.depth();
    complete_shift_parser(sender);
    app.poll_async_ui_jobs();
    assert!(app.status.contains("已取消后台粘贴"), "{}", app.status);
    assert_eq!(app.wb.sheet(0).value(3, 0), sheetcore::Value::Empty);
    assert_eq!(app.undo.depth(), history);
}

#[test]
fn partial_cell_shift_room_moves_note_and_invalidates_background_paste_on_both_peers() {
    use sheetcore::{CellRef, Value};
    let env = crate::test_env::TestEnv::new("cell-shift-room-note");
    for host_writes in [false, true] {
        let (mut host, mut guest) = sort_apps(&env);
        host.set_comment_cell(0, 1, 0, "keep note", Some("Owner"), false)
            .unwrap();
        let matches = |app: &SheetApp, shifted: bool| {
            let sh = app.wb.sheet(0);
            let (pos, empty) = if shifted { (2, 1) } else { (1, 2) };
            sh.notes.get(&CellRef::new(pos, 0)).is_some_and(|note| {
                note.text == "keep note" && note.author == "Owner" && (note.r, note.c) == (pos, 0)
            }) && !sh.notes.contains_key(&CellRef::new(empty, 0))
                && sh.value(pos, 0) == Value::Number(1.0)
                && sh.value(empty, 0) == Value::Empty
        };
        until(&mut host, &mut guest, |h, g| {
            matches(h, false) && matches(g, false)
        });
        let (history, observer_history) = if host_writes {
            (host.undo.depth(), guest.undo.depth())
        } else {
            (guest.undo.depth(), host.undo.depth())
        };
        for phase in 0..3 {
            let old_host = host.wb.sheet(0).coordinate_revision();
            let old_guest = guest.wb.sheet(0).coordinate_revision();
            let host_paste = start_shift_paste(&mut host);
            let guest_paste = start_shift_paste(&mut guest);
            let writer = if host_writes { &mut host } else { &mut guest };
            if phase == 0 {
                writer.sel = (1, 0);
                writer.sel_end = writer.sel;
                writer.shift_cells(true, false);
            } else {
                assert!(writer.apply_undo(phase == 2), "{}", writer.status);
            }
            let shifted = phase != 1;
            until(&mut host, &mut guest, |h, g| {
                matches(h, shifted)
                    && matches(g, shifted)
                    && h.wb.sheet(0).coordinate_revision() != old_host
                    && g.wb.sheet(0).coordinate_revision() != old_guest
            });
            let (writer, observer) = if host_writes {
                (&host, &guest)
            } else {
                (&guest, &host)
            };
            assert_eq!(observer.undo.depth(), observer_history);
            assert_eq!(
                writer.undo.depth(),
                if shifted {
                    (history.0 + 1, 0)
                } else {
                    (history.0, 1)
                }
            );
            finish_shift_paste(&mut host, host_paste);
            finish_shift_paste(&mut guest, guest_paste);
        }
    }
}

#[test]
fn partial_cell_shift_room_cancels_paste_when_commit_arrives_between_ui_polls() {
    use sheetcore::Value;
    use std::time::{Duration, Instant};
    let env = crate::test_env::TestEnv::new("cell-shift-queued-before-paste");
    let (mut host, mut guest) = sort_apps(&env);
    guest.poll_net();
    let original = guest.wb.sheet(0).coordinate_revision();
    let history = guest.undo.depth();
    let pending = start_shift_paste(&mut guest);
    let prior_watermark = host
        .net
        .as_ref()
        .unwrap()
        .current_watermark_cursor()
        .unwrap()
        .0;
    host.sel = (1, 0);
    host.sel_end = host.sel;
    host.shift_cells(true, false);
    let deadline = Instant::now() + Duration::from_secs(10);
    while host.wb.sheet(0).value(2, 0) != Value::Number(1.0) {
        host.poll_net();
        assert!(Instant::now() < deadline, "{}", host.status);
        std::thread::yield_now();
    }
    // The reader receives the shift's watermark strictly after queueing its commit.
    // No guest UI event is consumed until the parser returns.
    let (seq, _) = host
        .net
        .as_ref()
        .unwrap()
        .current_watermark_cursor()
        .unwrap();
    assert!(seq > prior_watermark);
    let deadline = Instant::now() + Duration::from_secs(10);
    while guest.net.as_ref().unwrap().received_watermark_for_test() < seq {
        assert!(Instant::now() < deadline, "missing post-commit watermark");
        std::thread::yield_now();
    }
    assert_eq!(guest.wb.sheet(0).coordinate_revision(), original);
    assert_eq!(guest.wb.sheet(0).value(1, 0), Value::Number(1.0));

    finish_shift_paste(&mut guest, pending);
    assert_ne!(guest.wb.sheet(0).coordinate_revision(), original);
    assert_eq!(guest.wb.sheet(0).value(2, 0), Value::Number(1.0));
    assert_eq!(guest.undo.depth(), history);
    assert_eq!(host.wb.sheet(0).value(3, 0), Value::Empty);
}

#[test]
fn partial_cell_shift_room_late_join_restores_sources_and_cells() {
    use sheetcore::{
        Value,
        external::{Kind, Source},
    };
    let env = crate::test_env::TestEnv::new("cell-shift-late-source");
    // The same workbook binds each participant to their own copy of the CSV.
    let sources = |path: &std::path::Path| {
        vec![
            Source {
                name: "move".into(),
                at: (1, 0),
                target_sheet: Some("Sheet1".into()),
                rows: 1,
                cols: 1,
                kind: Kind::Internal {
                    sheet: "Sheet1".into(),
                    rect: (0, 3, 0, 3),
                },
                csv_opts: None,
            },
            Source {
                name: "split".into(),
                at: (4, 0),
                target_sheet: Some("Sheet1".into()),
                rows: 1,
                cols: 2,
                kind: Kind::Csv {
                    path: path.with_extension("csv").to_string_lossy().into_owned(),
                    delim: ',',
                    header: true,
                },
                csv_opts: Some(sheetcore::external::CsvOpts {
                    text_cols: vec![1],
                    ..Default::default()
                }),
            },
        ]
    };
    let baseline = |wb: &mut sheetcore::Workbook| {
        wb.sources = sources(wb.path.as_deref().unwrap());
        let style = sheetcore::style::CellStyle {
            italic: true,
            ..Default::default()
        };
        let id = wb
            .styles
            .intern_with_ext(style, Default::default(), None, true);
        wb.sheet_mut(0).set_style(1, 0, id);
    };
    let (mut host, mut guest) = sort_apps_with_baseline(&env, baseline);
    let matches = |app: &SheetApp, shifted: bool| {
        let mut expected = sources(app.wb.path.as_deref().unwrap());
        let (filled, empty) = if shifted {
            expected[0].at = (2, 0);
            expected[1].kind = Kind::Unbound(Box::new(expected[1].kind.clone()));
            (2, 1)
        } else {
            (1, 2)
        };
        let sh = app.wb.sheet(0);
        app.wb.sources == expected
            && sh.explicit_style_of(empty, 0).is_none()
            && sh
                .explicit_style_of(filled, 0)
                .is_some_and(|id| app.wb.styles.get(id).italic && app.wb.styles.quote_of(id))
            && sh.value(0, 0) == Value::Number(2.0)
            && sh.value(filled, 0) == Value::Number(1.0)
            && sh.value(empty, 0) == Value::Empty
            && sh.value(3, 0) == Value::Empty
    };
    host.sel = (1, 0);
    host.sel_end = host.sel;
    host.shift_cells(true, false);
    until(&mut host, &mut guest, |h, g| {
        matches(h, true) && matches(g, true)
    });

    let mut late = late_join(&env, &guest, baseline);
    until(&mut host, &mut late, |_, l| {
        l.joined_doc && matches(l, true)
    });
    assert!(
        !late.snapshot_is_delta,
        "exercise a full snapshot against the original file"
    );
    for redo in [false, true] {
        assert!(host.apply_undo(redo), "{}", host.status);
        until(&mut host, &mut guest, |h, g| {
            matches(h, redo) && matches(g, redo)
        });
        until(&mut host, &mut late, |h, l| {
            matches(h, redo) && matches(l, redo)
        });
    }
}

#[test]
fn partial_cell_shift_room_rejects_undo_after_a_reply_at_the_moved_thread() {
    use sheetcore::{CellRef, Value};
    let env = crate::test_env::TestEnv::new("cell-shift-thread-conflict");
    let (mut host, mut guest) = sort_apps(&env);
    host.set_comment_cell(0, 1, 0, "original", Some("root author"), true)
        .unwrap();
    let old = CellRef::new(1, 0);
    let dest = CellRef::new(2, 0);
    let original = host.wb.sheet(0).threads[&old].clone();
    until(&mut host, &mut guest, |_, g| {
        g.wb.sheet(0).threads.get(&old) == Some(&original)
    });
    guest.sel = (1, 0);
    guest.sel_end = guest.sel;
    guest.shift_cells(true, false);
    let moved = |app: &SheetApp, replied: bool| {
        let sh = app.wb.sheet(0);
        sh.value(2, 0) == Value::Number(1.0)
            && sh.value(1, 0) == Value::Empty
            && !sh.threads.contains_key(&old)
            && !sh.notes.contains_key(&old)
            && sh.threads.get(&dest).is_some_and(|tc| {
                (tc.r, tc.c) == (2, 0)
                    && tc.entries.len() == if replied { 2 } else { 1 }
                    && tc.entries[0] == original.entries[0]
                    && (!replied
                        || tc.entries[1].author == "late author"
                            && tc.entries[1].text == "later reply")
                    && sh.notes.get(&dest) == Some(&tc.summary_note())
            })
    };
    until(&mut host, &mut guest, |h, g| {
        moved(h, false) && moved(g, false)
    });
    let history = guest.undo.depth();
    let revisions = [
        host.wb.sheet(0).coordinate_revision(),
        guest.wb.sheet(0).coordinate_revision(),
    ];
    host.reply_comment_cell(0, 2, 0, "later reply", Some("late author"))
        .unwrap();
    assert!(moved(&host, true) && moved(&guest, false));
    // Request undo before the guest polls the reply. Only the host can detect this conflict.
    assert!(guest.apply_undo(false), "{}", guest.status);
    until(&mut host, &mut guest, |h, g| {
        moved(h, true) && moved(g, true) && !g.net.as_ref().unwrap().pending_structure()
    });
    assert_eq!(guest.undo.depth(), history);
    guest.wb.sheet_mut(0).set(
        4,
        2,
        sheetcore::Cell {
            value: Value::Number(9.0),
            formula: None,
        },
    );
    guest.broadcast_cells(&[(0, 4, 2)]);
    until(&mut host, &mut guest, |h, _| {
        h.wb.sheet(0).value(4, 2) == Value::Number(9.0)
    });
    assert!(moved(&host, true) && moved(&guest, true));
    assert_eq!(
        [
            host.wb.sheet(0).coordinate_revision(),
            guest.wb.sheet(0).coordinate_revision()
        ],
        revisions
    );
    let mut late = late_join(&env, &guest, |_| {});
    until(&mut host, &mut late, |_, l| l.joined_doc && moved(l, true));
    assert!(!late.snapshot_is_delta);
}

#[test]
fn partial_cell_shift_room_history_after_leaving_preserves_peer_edits() {
    use sheetcore::{Cell, CellRef, Value};
    let env = crate::test_env::TestEnv::new("cell-shift-leave-history");
    for host_rewrote_cell in [false, true] {
        let (mut host, mut guest) = sort_apps(&env);
        host.set_comment_cell(0, 1, 0, "move with value", Some("Owner"), false)
            .unwrap();
        let matches = |app: &SheetApp, shifted: bool| {
            let (row, blank) = if shifted { (2, 1) } else { (1, 2) };
            let sheet = app.wb.sheet(0);
            sheet.value(row, 0) == Value::Number(1.0)
                && sheet.value(blank, 0) == Value::Empty
                && sheet.notes.get(&CellRef::new(row, 0)).is_some_and(|note| {
                    (note.r, note.c) == (row, 0) && note.text == "move with value"
                })
                && !sheet.notes.contains_key(&CellRef::new(blank, 0))
        };
        until(&mut host, &mut guest, |h, g| {
            matches(h, false) && matches(g, false)
        });
        guest.sel = (1, 0);
        guest.sel_end = guest.sel;
        guest.shift_cells(true, false);
        until(&mut host, &mut guest, |h, g| {
            matches(h, true) && matches(g, true)
        });
        if host_rewrote_cell {
            // A concurrent user made this value their own, even though the text still matches.
            host.wb.sheet_mut(0).set(
                2,
                0,
                Cell {
                    value: Value::Number(1.0),
                    formula: None,
                },
            );
            host.broadcast_cells(&[(0, 2, 0)]);
            until(&mut host, &mut guest, |_, g| {
                g.peer
                    .as_ref()
                    .unwrap()
                    .cells
                    .get(&(0, 2, 0))
                    .is_some_and(|op| op.site == 1)
            });
        }
        let expected_depth = guest.undo.depth();
        guest.leave_session_at(None);
        assert!(guest.net.is_none());
        let before = guest.wb.sheet(0).coordinate_revision();
        if host_rewrote_cell {
            assert!(
                !guest.apply_undo(false),
                "a same-value edit by the host must survive"
            );
            assert_eq!(guest.undo.depth(), expected_depth);
            assert_eq!(guest.wb.sheet(0).coordinate_revision(), before);
            assert!(matches(&guest, true));
        } else {
            assert!(guest.apply_undo(false), "{}", guest.status);
            assert!(matches(&guest, false));
            assert_eq!(guest.undo.depth(), (expected_depth.0 - 1, 1));
            let undone = guest.wb.sheet(0).coordinate_revision();
            assert_ne!(undone, before);
            assert!(guest.apply_undo(true), "{}", guest.status);
            assert!(matches(&guest, true));
            assert_eq!(guest.undo.depth(), expected_depth);
            assert_ne!(guest.wb.sheet(0).coordinate_revision(), undone);
        }
        host.poll_net();
        assert!(matches(&host, true));
    }
}

#[test]
fn partial_cell_shift_room_reconnect_replays_ordinary_edit_and_only_coordinate_valid_paste() {
    use sheetcore::{Cell, Value};
    use std::time::{Duration, Instant};
    let env = crate::test_env::TestEnv::new("cell-shift-paste-outbox");
    for host_shifted in [false, true] {
        let (mut host, mut guest) = sort_apps(&env);
        let number = |v| Cell {
            value: Value::Number(v),
            formula: None,
        };
        // Model exactly the gap between local commit and transport acknowledgement.
        // Keep the real replica and workbook optimistic without delivering either write to the host.
        let start = guest.peer.as_ref().unwrap().clock;
        let paste_ts = guest
            .peer
            .as_mut()
            .unwrap()
            .local_set(0, 3, 0, &number(42.0))
            .ts;
        guest.wb.sheet_mut(0).set(3, 0, number(42.0));
        guest.track_deferred_paste(start, paste_ts);
        let ordinary_ts = guest
            .peer
            .as_mut()
            .unwrap()
            .local_set(0, 6, 1, &number(77.0))
            .ts;
        guest.wb.sheet_mut(0).set(6, 1, number(77.0));
        assert!(ordinary_ts > paste_ts && paste_ts > guest.net.as_ref().unwrap().acked_op_ts());
        if host_shifted {
            host.sel = (1, 0);
            host.sel_end = host.sel;
            host.shift_cells(true, false);
            let deadline = Instant::now() + Duration::from_secs(10);
            while host.wb.sheet(0).value(2, 0) != Value::Number(1.0) {
                host.poll_net();
                assert!(Instant::now() < deadline, "{}", host.status);
                std::thread::yield_now();
            }
            let seq = host
                .net
                .as_ref()
                .unwrap()
                .current_watermark_cursor()
                .unwrap()
                .0;
            while guest.net.as_ref().unwrap().received_watermark_for_test() < seq {
                assert!(
                    Instant::now() < deadline,
                    "missing shift watermark before disconnect"
                );
                std::thread::yield_now();
            }
            assert_eq!(guest.wb.sheet(0).value(1, 0), Value::Number(1.0));
        }
        assert_eq!(host.wb.sheet(0).value(3, 0), Value::Empty);
        assert_eq!(host.wb.sheet(0).value(6, 1), Value::Empty);
        guest.net.as_ref().unwrap().disconnect_socket_for_test();
        // This cell was never sent normally, so its arrival proves SnapshotDone ran the real outbox.
        until(&mut host, &mut guest, |h, g| {
            g.joined_doc && !g.in_snapshot && h.wb.sheet(0).value(6, 1) == Value::Number(77.0)
        });
        let expected = if host_shifted {
            Value::Empty
        } else {
            Value::Number(42.0)
        };
        for app in [&host, &guest] {
            assert_eq!(
                app.wb.sheet(0).value(3, 0),
                expected,
                "stale optimistic paste, shifted={host_shifted} host={}",
                app.net.as_ref().unwrap().hosting
            );
            assert_eq!(app.wb.sheet(0).value(6, 1), Value::Number(77.0));
            assert_eq!(
                app.wb.sheet(0).value(if host_shifted { 2 } else { 1 }, 0),
                Value::Number(1.0)
            );
        }
    }
}
