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
fn partial_cell_shift_room_reconnect_cancels_all_stale_unacked_edits_and_replays_when_unshifted() {
    use sheetcore::{Cell, Value};
    use std::time::{Duration, Instant};
    let env = crate::test_env::TestEnv::new("cell-shift-paste-outbox");
    for host_shifted in [false, true] {
        let (mut host, mut guest) = sort_apps(&env);
        let number = |v| Cell {
            value: Value::Number(v),
            formula: None,
        };
        // Model the gap between local optimistic commit and transport acknowledgement.
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
        if host_shifted {
            until(&mut host, &mut guest, |_, g| {
                g.joined_doc
                    && !g.in_snapshot
                    && !g.room_lost
                    && g.shift_intro.applied_seq > 0
                    && g.wb.sheet(0).value(3, 0) == Value::Empty
                    && g.wb.sheet(0).value(6, 1) == Value::Empty
                    && g.status.contains("已取消断线前未确认")
            });
            for _ in 0..15 {
                host.poll_net();
                guest.poll_net();
                std::thread::sleep(Duration::from_millis(10));
            }
        } else {
            // Neither cell was sent normally; arrival proves SnapshotDone ran the real outbox.
            until(&mut host, &mut guest, |h, g| {
                g.joined_doc
                    && !g.in_snapshot
                    && h.wb.sheet(0).value(6, 1) == Value::Number(77.0)
                    && h.wb.sheet(0).value(3, 0) == Value::Number(42.0)
            });
        }
        let expected_paste = if host_shifted {
            Value::Empty
        } else {
            Value::Number(42.0)
        };
        let expected_ordinary = if host_shifted {
            Value::Empty
        } else {
            Value::Number(77.0)
        };
        for app in [&host, &guest] {
            assert_eq!(
                app.wb.sheet(0).value(3, 0),
                expected_paste,
                "stale optimistic paste, shifted={host_shifted} host={}",
                app.net.as_ref().unwrap().hosting
            );
            assert_eq!(
                app.wb.sheet(0).value(6, 1),
                expected_ordinary,
                "stale ordinary edit must not replay across shift, shifted={host_shifted} host={}",
                app.net.as_ref().unwrap().hosting
            );
            assert_eq!(
                app.wb.sheet(0).value(if host_shifted { 2 } else { 1 }, 0),
                Value::Number(1.0)
            );
        }
        assert!(guest.rejoin_outbox.is_none());
    }
}

#[test]
fn partial_cell_shift_room_reconnect_cancels_ordinary_cell_style_merge_on_full_and_delta_snapshots()
{
    use sheetcore::{Cell, Value};
    use std::time::Duration;
    let env = crate::test_env::TestEnv::new("cell-shift-coord-epoch-reconnect");
    for force_full in [false, true] {
        let (mut host, mut guest) = sort_apps(&env);
        let red = [255, 0, 0];
        // Disconnect guest before host commits partial CellShift.
        if force_full {
            guest.net.as_ref().unwrap().request_snapshot_resync();
        } else {
            guest.net.as_ref().unwrap().disconnect_socket_for_test();
        }
        until(&mut host, &mut guest, |_, g| g.room_lost);

        // Make ordinary unacknowledged cell, style, and merge edits while offline.
        let offline_cell = Cell {
            value: Value::Number(77.0),
            formula: None,
        };
        let _ = guest
            .peer
            .as_mut()
            .unwrap()
            .local_set(0, 6, 1, &offline_cell);
        guest.wb.sheet_mut(0).set(6, 1, offline_cell);
        guest.sel = (6, 1);
        guest.sel_end = (6, 1);
        assert!(guest.apply_style_xy(move |st, _, _| st.fill = Some(red)));
        guest.sel = (7, 1);
        guest.sel_end = (7, 2);
        guest.toggle_merge(true);
        assert!(
            !guest.rejoin_style_outbox.is_empty() && !guest.rejoin_merge_outbox.is_empty(),
            "offline style & merge edits should queue into epoch-bound outboxes"
        );
        assert!(guest.wb.sheet(0).merges.contains(&(7, 1, 7, 2)));

        // Host commits partial CellShift while guest is disconnected.
        host.sel = (1, 0);
        host.sel_end = (1, 0);
        host.shift_cells(true, false);
        until(&mut host, &mut guest, |h, _| {
            h.wb.sheet(0).value(2, 0) == Value::Number(1.0) && h.shift_intro.applied_seq > 0
        });

        let mut saw_delta = false;
        until(&mut host, &mut guest, |_, g| {
            if g.snapshot_is_delta {
                saw_delta = true;
            }
            g.joined_doc
                && !g.in_snapshot
                && !g.room_lost
                && g.shift_intro.applied_seq > 0
                && g.wb.sheet(0).value(6, 1) == Value::Empty
                && g.status.contains("已取消断线前未确认")
        });
        for _ in 0..15 {
            host.poll_net();
            guest.poll_net();
            std::thread::sleep(Duration::from_millis(10));
        }

        if force_full {
            assert!(!saw_delta, "full-snapshot case should not use delta sync");
        } else {
            assert!(
                saw_delta,
                "delta-snapshot case should hit delta sync first then force full resync"
            );
            assert!(
                !guest.snapshot_is_delta,
                "forced full resync should complete on non-delta snapshot"
            );
        }
        assert!(guest.rejoin_outbox.is_none());
        assert!(guest.rejoin_style_outbox.is_empty());
        assert!(guest.rejoin_merge_outbox.is_empty());
        assert!(
            guest.status.contains("工作表坐标已改变") && guest.status.contains("已取消"),
            "unexpected status: {}",
            guest.status
        );
        for app in [&host, &guest] {
            let sh = app.wb.sheet(0);
            assert_eq!(sh.value(2, 0), Value::Number(1.0));
            assert_eq!(sh.value(1, 0), Value::Empty);
            assert_eq!(
                sh.value(6, 1),
                Value::Empty,
                "stale cell edit must not replay (force_full={force_full})"
            );
            let sid = sh.style_of(6, 1);
            assert_ne!(
                app.wb.styles.get(sid).fill,
                Some(red),
                "stale style edit must not replay (force_full={force_full})"
            );
            assert!(
                !sh.merges.contains(&(7, 1, 7, 2)),
                "stale merge edit must not replay (force_full={force_full})"
            );
        }
    }
}

#[test]
fn whole_row_structural_shifts_and_replays_offline_cell_style_merge_even_after_prior_cell_shift() {
    use sheetcore::{Cell, Value, structural::Structural};
    let env = crate::test_env::TestEnv::new("structural-only-after-prior-shift");
    for force_full in [false, true] {
        let (mut host, mut guest) = sort_apps(&env);
        let green = [0, 200, 0];

        // Commit an earlier CellShift FIRST so both peers are at shift_seq > 0 before disconnect.
        // This verifies Welcome captures shift_seq BEFORE resetting shift_intro.applied_seq = 0!
        host.sel = (1, 0);
        host.sel_end = (1, 0);
        host.shift_cells(true, false);
        until(&mut host, &mut guest, |h, g| {
            h.wb.sheet(0).value(2, 0) == Value::Number(1.0)
                && g.wb.sheet(0).value(2, 0) == Value::Number(1.0)
                && g.shift_intro.applied_seq > 0
        });
        let prior_shift_seq = guest.shift_intro.applied_seq;
        assert!(prior_shift_seq > 0);

        if force_full {
            guest.net.as_ref().unwrap().request_snapshot_resync();
        } else {
            guest.net.as_ref().unwrap().disconnect_socket_for_test();
        }
        until(&mut host, &mut guest, |_, g| g.room_lost);

        // Offline ordinary cell + style + merge edits made at shift_seq == prior_shift_seq.
        let offline_cell = Cell {
            value: Value::Number(55.0),
            formula: None,
        };
        let _ = guest
            .peer
            .as_mut()
            .unwrap()
            .local_set(0, 4, 1, &offline_cell);
        guest.wb.sheet_mut(0).set(4, 1, offline_cell);
        guest.sel = (4, 1);
        guest.sel_end = (4, 1);
        assert!(guest.apply_style_xy(move |st, _, _| st.fill = Some(green)));
        guest.sel = (6, 1);
        guest.sel_end = (6, 2);
        guest.toggle_merge(true);

        // Host commits ONLY whole-row Structural (InsertRows at=1, n=2) — NO new CellShift!
        let ins = Structural::InsertRows { at: 1, n: 2 };
        sheetcore::structural::apply_structural_wb(&mut host.wb, 0, &ins);
        host.shift_collab_coordinates(0, &ins);
        host.net.as_ref().unwrap().broadcast_structural(0, &ins).unwrap();

        // Guest reconnects -> offline edits shift by +2 rows and replay accurately!
        until(&mut host, &mut guest, |h, g| {
            g.joined_doc
                && !g.in_snapshot
                && !g.room_lost
                && h.wb.sheet(0).value(6, 1) == Value::Number(55.0)
                && h.wb.sheet(0).merges.contains(&(8, 1, 8, 2))
        });

        assert_eq!(guest.shift_intro.applied_seq, prior_shift_seq);
        assert_eq!(guest.net.as_ref().unwrap().coord_epoch().struct_rev, 1);
        assert_eq!(
            guest.net.as_ref().unwrap().coord_epoch().shift_seq,
            prior_shift_seq
        );
        for app in [&host, &guest] {
            let sh = app.wb.sheet(0);
            assert_eq!(sh.value(6, 1), Value::Number(55.0));
            assert_eq!(sh.value(4, 1), Value::Empty);
            let shifted_sid = sh.style_of(6, 1);
            assert_eq!(
                app.wb.styles.get(shifted_sid).fill,
                Some(green),
                "style edit should shift to (6,1) (force_full={force_full})"
            );
            let old_sid = sh.style_of(4, 1);
            assert_ne!(app.wb.styles.get(old_sid).fill, Some(green));
            assert!(sh.merges.contains(&(8, 1, 8, 2)));
            assert!(!sh.merges.contains(&(6, 1, 6, 2)));
        }
    }
}

#[test]
fn live_cell_shift_committed_updates_remote_cursors_and_clears_stale_edit_and_locks() {
    use sheetcore::Value;
    let env = crate::test_env::TestEnv::new("live-cell-shift-collab-housekeeping");
    let (mut host, mut guest) = sort_apps(&env);

    // 1. Live vertical INSERT at (1, 0): cells in col 0 at row >= 1 shift down by +1.
    guest.remote.insert(10, (0, "InBand".into(), 2, 2, 0, 3, 0));
    guest.remote.insert(11, (0, "OutBand".into(), 3, 2, 3, 2, 3));
    let now = guest.lock_now();
    guest.locks.request(0, 2, 0, 2, "G", now);
    guest.locks.request(0, 0, 5, 9, "Other", now);
    guest.my_lock = Some((0, 2, 0));
    guest.requested_lock = Some((0, 2, 0));
    guest.edit_sheet = Some(0);
    guest.edit_sel = Some((2, 0));
    guest.editing = Some(Default::default());
    guest.pending_edit = Some("draft".into());

    host.sel = (1, 0);
    host.sel_end = (1, 0);
    host.shift_cells(true, false);
    until(&mut host, &mut guest, |h, g| {
        h.wb.sheet(0).value(2, 0) == Value::Number(1.0)
            && g.wb.sheet(0).value(2, 0) == Value::Number(1.0)
            && g.shift_intro.applied_seq > 0
    });

    assert!(guest.editing.is_none(), "active edit in shifted band must cancel");
    assert!(guest.edit_sel.is_none());
    assert!(guest.edit_sheet.is_none());
    assert!(guest.pending_edit.is_none());
    assert!(guest.my_lock.is_none());
    assert!(guest.requested_lock.is_none());
    assert!(guest.locks.holder(0, 2, 0).is_none());
    assert!(
        guest.locks.holder(0, 0, 5).is_some(),
        "lock outside shifted band must remain"
    );
    let in_band = guest.remote.get(&10).expect("in-band cursor should shift");
    assert_eq!((in_band.0, in_band.3, in_band.4, in_band.5, in_band.6), (0, 3, 0, 4, 0));
    let out_band = guest.remote.get(&11).expect("out-of-band cursor should stay");
    assert_eq!((out_band.0, out_band.3, out_band.4, out_band.5, out_band.6), (0, 2, 3, 2, 3));

    // 2. Live vertical DELETE at (2, 0): cursor on deleted cell is dropped, while an active edit
    //    outside the shifted band stays intact.
    let seq_before = guest.shift_intro.applied_seq;
    guest.remote.insert(12, (0, "DeletedCell".into(), 4, 2, 0, 2, 0));
    guest.edit_sheet = Some(0);
    guest.edit_sel = Some((0, 5));
    guest.editing = Some(Default::default());
    host.sel = (2, 0);
    host.sel_end = (2, 0);
    host.shift_cells(false, false);
    until(&mut host, &mut guest, |h, g| {
        h.wb.sheet(0).value(2, 0) == Value::Empty
            && g.wb.sheet(0).value(2, 0) == Value::Empty
            && g.shift_intro.applied_seq > seq_before
    });

    assert!(
        !guest.remote.contains_key(&12),
        "remote cursor on deleted cell should be removed"
    );
    assert!(
        guest.editing.is_some() && guest.edit_sel == Some((0, 5)),
        "active edit outside shifted band must stay intact"
    );
}

#[test]
fn cell_shift_preflight_rejects_conflicts_cleanly_in_solo_and_room_modes() {
    use sheetcore::{Cell, CellRef, Value, structural::CellShiftConflict};
    let env = crate::test_env::TestEnv::new("cell-shift-preflight-app");

    let make_solo = || {
        let mut app = SheetApp::new_with_context(&eframe::egui::Context::default(), None);
        app.wb.sheet_mut(0).set(
            1,
            0,
            Cell {
                value: Value::Number(10.0),
                formula: None,
            },
        );
        app.wb.dirty = false;
        app.sel = (1, 0);
        app.sel_end = (1, 0);
        app
    };

    // Solo: PartialMerge
    {
        let mut app = make_solo();
        app.wb.sheet_mut(0).merge_range(1, 0, 1, 1);
        app.shift_cells(true, false);
        assert_eq!(app.status, CellShiftConflict::PartialMerge.message());
        assert_eq!(app.wb.sheet(0).value(1, 0), Value::Number(10.0));
        assert_eq!(app.wb.sheet(0).value(2, 0), Value::Empty);
        assert_eq!(app.undo.depth(), (0, 0));
        assert!(!app.wb.dirty);
    }

    // Solo: Table
    {
        let mut app = make_solo();
        app.wb.sheet_mut(0).tables.push(sheetcore::table::Table {
            name: "T1".into(),
            display: "T1".into(),
            range: (0, 0, 3, 2),
            ..Default::default()
        });
        app.shift_cells(true, false);
        assert_eq!(app.status, CellShiftConflict::Table.message());
        assert_eq!(app.wb.sheet(0).value(1, 0), Value::Number(10.0));
        assert_eq!(app.undo.depth(), (0, 0));
        assert!(!app.wb.dirty);
    }

    // Solo: AutoFilter
    {
        let mut app = make_solo();
        app.wb.sheet_mut(0).filter = Some(sheetcore::sort_filter::Filter::new(0, 0, 4, 2));
        app.shift_cells(true, false);
        assert_eq!(app.status, CellShiftConflict::AutoFilter.message());
        assert_eq!(app.wb.sheet(0).value(1, 0), Value::Number(10.0));
        assert_eq!(app.undo.depth(), (0, 0));
        assert!(!app.wb.dirty);
    }

    // Solo: ArrayOrSpill
    {
        let mut app = make_solo();
        app.wb
            .sheet_mut(0)
            .arrays
            .push((CellRef::new(2, 0), (2, 0, 3, 1)));
        app.shift_cells(true, false);
        assert_eq!(app.status, CellShiftConflict::ArrayOrSpill.message());
        assert_eq!(app.wb.sheet(0).value(1, 0), Value::Number(10.0));
        assert_eq!(app.undo.depth(), (0, 0));
        assert!(!app.wb.dirty);
    }

    // Solo: Pivot
    {
        let mut app = make_solo();
        app.wb.pivots_pending.push((
            0,
            sheetcore::pivot::PivotSpec {
                source: (0, 0, 3, 1),
                row_field: 0,
                col_field: None,
                value_field: 1,
                agg: sheetcore::pivot::Agg::Sum,
                group: sheetcore::pivot::GroupBy::None,
                exclude: Vec::new(),
            },
            sheetcore::pivot::PivotResult::default(),
            (5, 0, 7, 1),
            "P1".into(),
        ));
        app.shift_cells(true, false);
        assert_eq!(app.status, CellShiftConflict::Pivot.message());
        assert_eq!(app.wb.sheet(0).value(1, 0), Value::Number(10.0));
        assert_eq!(app.undo.depth(), (0, 0));
        assert!(!app.wb.dirty);
    }

    // Room mode: local preflight (PartialMerge) AND host-authoritative preflight (Table on host)
    {
        let (mut host, mut guest) = sort_apps(&env);
        let host_history = host.undo.depth();
        let guest_history = guest.undo.depth();

        // Local guest preflight blocks PartialMerge without sending a room request.
        guest.wb.sheet_mut(0).merge_range(1, 0, 1, 1);
        guest.sel = (1, 0);
        guest.sel_end = (1, 0);
        guest.shift_cells(true, false);
        assert_eq!(guest.status, CellShiftConflict::PartialMerge.message());
        assert!(!guest.net.as_ref().unwrap().pending_structure());
        assert_eq!(guest.undo.depth(), guest_history);
        assert_eq!(guest.wb.sheet(0).value(1, 0), Value::Number(1.0));
        assert_eq!(host.wb.sheet(0).value(1, 0), Value::Number(1.0));
        guest.wb.sheet_mut(0).unmerge_range(1, 0, 1, 1);

        // Host-authoritative preflight blocks when host has an overlapping Table.
        host.wb.sheet_mut(0).tables.push(sheetcore::table::Table {
            name: "HostTable".into(),
            display: "HostTable".into(),
            range: (0, 0, 3, 2),
            ..Default::default()
        });
        guest.sel = (1, 0);
        guest.sel_end = (1, 0);
        guest.shift_cells(true, false);
        assert!(guest.net.as_ref().unwrap().pending_structure());
        until(&mut host, &mut guest, |_, g| {
            !g.net.as_ref().unwrap().pending_structure()
        });
        assert_eq!(guest.status, CellShiftConflict::Table.message());
        assert_eq!(guest.undo.depth(), guest_history);
        assert_eq!(host.undo.depth(), host_history);
        assert_eq!(host.wb.sheet(0).value(1, 0), Value::Number(1.0));
        assert_eq!(host.wb.sheet(0).value(2, 0), Value::Empty);
        assert_eq!(guest.wb.sheet(0).value(1, 0), Value::Number(1.0));
        assert_eq!(guest.wb.sheet(0).value(2, 0), Value::Empty);
    }
}

#[test]
fn room_change_discards_stale_rejoin_outboxes_instead_of_replaying_onto_new_room() {
    use sheetcore::{Cell, Value};
    use std::time::Duration;
    let env = crate::test_env::TestEnv::new("cell-shift-room-change-outbox");
    let (mut host1, mut guest) = sort_apps(&env);
    let room1 = guest.net.as_ref().unwrap().connected_room_id();
    assert!(!room1.is_empty());

    // Disconnect from Room 1 and make offline cell + style + merge edits.
    guest.net.as_ref().unwrap().disconnect_socket_for_test();
    until(&mut host1, &mut guest, |_, g| g.room_lost);
    let red = [240, 20, 20];
    let offline = Cell {
        value: Value::Number(88.0),
        formula: None,
    };
    let _ = guest.peer.as_mut().unwrap().local_set(0, 5, 2, &offline);
    guest.wb.sheet_mut(0).set(5, 2, offline);
    guest.sel = (5, 2);
    guest.sel_end = (5, 2);
    assert!(guest.apply_style_xy(move |st, _, _| st.fill = Some(red)));
    guest.sel = (8, 1);
    guest.sel_end = (8, 2);
    guest.toggle_merge(true);

    // Start a brand-new host (Room 2) with a different room_id and switch guest to Room 2.
    let (mut host2, _unused) = sort_apps(&env);
    let host2_addr = format!("127.0.0.1:{}", host2.net.as_ref().unwrap().port);
    if let Some(old_host) = host1.net.take() {
        old_host.shutdown();
    }
    guest.room_password = "sort".into();
    guest.join_addr = host2_addr;
    guest.join_shared();
    until(&mut host2, &mut guest, |_, g| {
        g.joined_doc && !g.in_snapshot && !g.room_lost
    });
    for _ in 0..15 {
        host2.poll_net();
        guest.poll_net();
        std::thread::sleep(Duration::from_millis(10));
    }

    let room2 = guest.net.as_ref().unwrap().connected_room_id();
    assert_ne!(room1, room2);
    assert!(guest.rejoin_outbox.is_none());
    assert!(guest.rejoin_style_outbox.is_empty());
    assert!(guest.rejoin_merge_outbox.is_empty());
    assert!(
        guest.status.contains("工作表坐标已改变") && guest.status.contains("已取消"),
        "expected cancellation status on room change, got: {}",
        guest.status
    );
    for app in [&host2, &guest] {
        let sh = app.wb.sheet(0);
        assert_eq!(sh.value(5, 2), Value::Empty);
        let sid = sh.style_of(5, 2);
        assert_ne!(app.wb.styles.get(sid).fill, Some(red));
        assert!(!sh.merges.contains(&(8, 1, 8, 2)));
    }
}

#[test]
fn guest_authored_cell_shift_repeated_disconnect_without_offline_edits_creates_no_phantom_edits() {
    use sheetcore::{Value, structural::Structural};
    use std::time::Duration;
    let env = crate::test_env::TestEnv::new("guest-shift-repeated-disconnect");
    for host_does_shift in [true, false] {
        let (mut host, mut guest) = sort_apps(&env);
        assert_eq!(guest.net.as_ref().unwrap().acked_op_ts(), 0);

        // 1. Guest (site 2) authors a partial CellShift moving (1,0)=1.0 -> (2,0)=1.0.
        guest.sel = (1, 0);
        guest.sel_end = (1, 0);
        guest.shift_cells(true, false);
        until(&mut host, &mut guest, |h, g| {
            h.wb.sheet(0).value(2, 0) == Value::Number(1.0)
                && g.wb.sheet(0).value(2, 0) == Value::Number(1.0)
                && g.shift_intro.applied_seq > 0
                && !g.net.as_ref().unwrap().pending_structure()
                && g.net.as_ref().unwrap().acked_op_ts() > 0
        });
        let shift1_seq = guest.shift_intro.applied_seq;
        let shift1_acked = guest.net.as_ref().unwrap().acked_op_ts();
        assert!(shift1_acked > 1, "host must ack guest-authored shift op timestamps");

        // 2. Disconnect #1 (full snapshot resync) with zero offline edits -> repopulates guest peer.cells
        //    with site=2 shift ops and wipes in-memory committed_shift_ts_ranges.
        guest.net.as_ref().unwrap().request_snapshot_resync();
        until(&mut host, &mut guest, |_, g| g.room_lost);
        until(&mut host, &mut guest, |_, g| {
            g.joined_doc && !g.in_snapshot && !g.room_lost && g.shift_intro.applied_seq == shift1_seq
        });
        assert!(
            guest.net.as_ref().unwrap().acked_op_ts() >= shift1_acked,
            "Welcome must carry host_acked_ts across reconnect"
        );
        assert!(guest.rejoin_outbox.is_none());

        // 3. Disconnect #2 with ZERO offline edits while host commits either another CellShift
        //    or a whole-row Structural.
        guest.net.as_ref().unwrap().disconnect_socket_for_test();
        until(&mut host, &mut guest, |_, g| g.room_lost);

        if host_does_shift {
            host.sel = (1, 0);
            host.sel_end = (1, 0);
            host.shift_cells(true, false);
            until(&mut host, &mut guest, |h, _| {
                h.wb.sheet(0).value(3, 0) == Value::Number(1.0)
                    && h.shift_intro.applied_seq > shift1_seq
            });
            until(&mut host, &mut guest, |_, g| {
                g.joined_doc
                    && !g.in_snapshot
                    && !g.room_lost
                    && g.shift_intro.applied_seq > shift1_seq
            });
            for _ in 0..15 {
                host.poll_net();
                guest.poll_net();
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(
                guest.snapshot_is_delta,
                "zero offline edits must not trigger a spurious delta->full resync"
            );
            assert!(
                !guest.status.contains("已取消"),
                "must not show false cancellation banner: {}",
                guest.status
            );
            assert!(guest.rejoin_outbox.is_none());
            for app in [&host, &guest] {
                let sh = app.wb.sheet(0);
                assert_eq!(sh.value(0, 0), Value::Number(2.0));
                assert_eq!(sh.value(1, 0), Value::Empty);
                assert_eq!(sh.value(2, 0), Value::Empty);
                assert_eq!(sh.value(3, 0), Value::Number(1.0));
            }
        } else {
            let ins = Structural::InsertRows { at: 0, n: 2 };
            sheetcore::structural::apply_structural_wb(&mut host.wb, 0, &ins);
            host.shift_collab_coordinates(0, &ins);
            host.net.as_ref().unwrap().broadcast_structural(0, &ins).unwrap();
            until(&mut host, &mut guest, |_, g| {
                g.joined_doc
                    && !g.in_snapshot
                    && !g.room_lost
                    && g.net.as_ref().unwrap().structure_seen() == 1
            });
            for _ in 0..15 {
                host.poll_net();
                guest.poll_net();
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(
                !guest.status.contains("补发"),
                "committed guest shift ops must not replay as phantom edits: {}",
                guest.status
            );
            assert!(guest.rejoin_outbox.is_none());
            for app in [&host, &guest] {
                let sh = app.wb.sheet(0);
                assert_eq!(sh.value(0, 0), Value::Empty);
                assert_eq!(sh.value(1, 0), Value::Empty);
                assert_eq!(sh.value(2, 0), Value::Number(2.0));
                assert_eq!(sh.value(3, 0), Value::Empty);
                assert_eq!(sh.value(4, 0), Value::Number(1.0));
            }
        }
    }
}

#[test]
fn phase3_tx_room_cell_shift_tx_patch_wire_compat_undo_redo_and_conflict_guard() {
    use sheetcore::{
        cell_shift_patch::CellShiftPatch,
        cells::Shift,
        structural::CellShift,
        tx::{TxIntent, TxPatch},
        Cell, CellRef, Value,
    };
    let env = crate::test_env::TestEnv::new("p3-tx-room-shift");
    let (mut host, mut guest) = sort_apps(&env);

    // Put a formula referencing the shifted cell and verify TxPatch JSON wire parity.
    host.wb.sheet_mut(0).set(
        0,
        2,
        Cell {
            value: Value::Empty,
            formula: Some("=A2*10".into()),
        },
    );
    host.recalc_and_broadcast(&[(0, 0, 2)]);
    host.set_comment_cell(0, 1, 0, "tx note", Some("Host"), false)
        .unwrap();
    until(&mut host, &mut guest, |h, g| {
        h.wb.sheet(0).value(0, 2) == Value::Number(10.0)
            && g.wb.sheet(0).value(0, 2) == Value::Number(10.0)
            && g.wb.sheet(0).notes.contains_key(&CellRef::new(1, 0))
    });

    // Wire parity check: TxPatch::prepare serializes identically to CellShiftPatch.
    let op = CellShift::new((1, 0, 1, 0), Shift::Vertical, true);
    let tx_patch = TxPatch::prepare(&guest.wb, &TxIntent::from_cell_shift(0, &op))
        .expect("TxPatch::prepare must succeed");
    let tx_json = serde_json::to_string(&tx_patch).unwrap();
    let legacy_patch: CellShiftPatch =
        serde_json::from_str(&tx_json).expect("TxPatch wire JSON must deserialize as CellShiftPatch");
    assert_eq!(tx_patch.as_cell_shift(), Some(&legacy_patch));
    let roundtrip: TxPatch = serde_json::from_str(&tx_json).unwrap();
    assert_eq!(roundtrip, tx_patch);

    // Guest executes room CellShift (goes through WorkbookTx::preflight + TxPatch::prepare + apply_tx_effects).
    guest.cf_stats_gen = 64;
    guest.invalid_gen = 64;
    host.cf_stats_gen = 64;
    host.invalid_gen = 64;
    guest.sel = (1, 0);
    guest.sel_end = (1, 0);
    guest.shift_cells(true, false);
    until(&mut host, &mut guest, |h, g| {
        h.wb.sheet(0).value(2, 0) == Value::Number(1.0)
            && g.wb.sheet(0).value(2, 0) == Value::Number(1.0)
            && g.undo.depth() == (1, 0)
    });
    assert_eq!(guest.cf_stats_gen, u64::MAX);
    assert_eq!(guest.invalid_gen, u64::MAX);
    assert_eq!(host.cf_stats_gen, u64::MAX);
    assert_eq!(host.invalid_gen, u64::MAX);
    for peer in [&host, &guest] {
        let sh = peer.wb.sheet(0);
        assert_eq!(sh.value(1, 0), Value::Empty);
        assert_eq!(sh.value(2, 0), Value::Number(1.0));
        assert_eq!(sh.get(0, 2).unwrap().formula.as_deref(), Some("=A3*10"));
        assert_eq!(sh.value(0, 2), Value::Number(10.0));
        assert!(sh.notes.contains_key(&CellRef::new(2, 0)));
    }

    // Undo and Redo via unified Entry::Tx(TxDelta::RoomPatch).
    assert!(guest.apply_undo(false), "{}", guest.status);
    until(&mut host, &mut guest, |h, g| {
        h.wb.sheet(0).value(1, 0) == Value::Number(1.0)
            && g.wb.sheet(0).value(1, 0) == Value::Number(1.0)
            && g.undo.depth() == (0, 1)
    });
    assert_eq!(
        host.wb.sheet(0).get(0, 2).unwrap().formula.as_deref(),
        Some("=A2*10")
    );
    assert!(host.wb.sheet(0).notes.contains_key(&CellRef::new(1, 0)));

    assert!(guest.apply_undo(true), "{}", guest.status);
    until(&mut host, &mut guest, |h, g| {
        h.wb.sheet(0).value(2, 0) == Value::Number(1.0)
            && g.wb.sheet(0).value(2, 0) == Value::Number(1.0)
            && g.undo.depth() == (1, 0)
    });

    // Host modifies a shifted cell -> guest Undo must be blocked by conflict guard & preserve history.
    host.wb.sheet_mut(0).set(
        2,
        0,
        Cell {
            value: Value::Number(99.0),
            formula: None,
        },
    );
    host.recalc_and_broadcast(&[(0, 2, 0)]);
    until(&mut host, &mut guest, |_, g| {
        g.wb.sheet(0).value(2, 0) == Value::Number(99.0)
    });
    assert!(!guest.apply_undo(false));
    assert_eq!(guest.undo.depth(), (1, 0), "conflicting undo must retain room shift history");
    assert_eq!(guest.wb.sheet(0).value(2, 0), Value::Number(99.0));
}

#[test]
fn phase5_tx_room_defined_names_add_edit_delete_undo_and_late_join() {
    use sheetcore::{tx::DefinedNameIntent, Cell, Value};
    let env = crate::test_env::TestEnv::new("p5-tx-room-names");
    let (mut host, mut guest) = sort_apps(&env);

    host.wb.sheet_mut(0).set(
        0,
        3,
        Cell {
            value: Value::Empty,
            formula: Some("=TaxRate*100".into()),
        },
    );
    host.recalc_and_broadcast(&[(0, 0, 3)]);
    until(&mut host, &mut guest, |_, g| {
        g.wb.sheet(0)
            .get(0, 3)
            .is_some_and(|c| c.formula.as_deref() == Some("=TaxRate*100"))
    });

    // 1) Guest adds DefinedName via RoomTx -> both converge & recalc formula to 20.
    assert!(
        guest
            .submit_defined_name_intent(
                DefinedNameIntent::Define {
                    name: "TaxRate".into(),
                    scope: None,
                    formula: "0.2".into(),
                },
                "定义名称 TaxRate",
                (0, 0, 0),
            )
            .unwrap()
    );
    until(&mut host, &mut guest, |h, g| {
        h.wb.names.iter().any(|(n, _, f)| n == "TaxRate" && f == "0.2")
            && g.wb.names.iter().any(|(n, _, f)| n == "TaxRate" && f == "0.2")
            && h.wb.sheet(0).value(0, 3) == Value::Number(20.0)
            && g.wb.sheet(0).value(0, 3) == Value::Number(20.0)
            && !g.net.as_ref().unwrap().pending_structure()
    });
    assert_eq!(guest.undo.depth(), (1, 0));

    // 2) Host updates DefinedName to 0.5 -> both converge & recalc to 50.
    let idx = host
        .wb
        .names
        .iter()
        .position(|(n, _, _)| n == "TaxRate")
        .unwrap();
    assert!(
        host.submit_defined_name_intent(
            DefinedNameIntent::Update {
                index: idx,
                name: "TaxRate".into(),
                scope: None,
                formula: "0.5".into(),
            },
            "编辑名称",
            (0, 0, 0),
        )
        .unwrap()
    );
    until(&mut host, &mut guest, |h, g| {
        h.wb.names.iter().any(|(n, _, f)| n == "TaxRate" && f == "0.5")
            && g.wb.names.iter().any(|(n, _, f)| n == "TaxRate" && f == "0.5")
            && h.wb.sheet(0).value(0, 3) == Value::Number(50.0)
            && g.wb.sheet(0).value(0, 3) == Value::Number(50.0)
            && !h.net.as_ref().unwrap().pending_structure()
    });

    // 3) Late joiner receives updated DefinedNames via CellShiftOverlay and evaluates formula to 50.
    let mut late = late_join(&env, &guest, |_| {});
    until(&mut host, &mut late, |_, l| {
        l.joined_doc
            && !l.in_snapshot
            && l.wb.names.iter().any(|(n, _, f)| n == "TaxRate" && f == "0.5")
            && l.wb.sheet(0).value(0, 3) == Value::Number(50.0)
    });

    // 4) Host Undo -> reverts to 0.2 (20.0) on all peers; Redo -> back to 0.5 (50.0).
    assert!(host.apply_undo(false), "{}", host.status);
    until(&mut host, &mut guest, |h, g| {
        h.wb.sheet(0).value(0, 3) == Value::Number(20.0)
            && g.wb.sheet(0).value(0, 3) == Value::Number(20.0)
    });
    until(&mut host, &mut late, |_, l| {
        l.wb.sheet(0).value(0, 3) == Value::Number(20.0)
    });

    assert!(host.apply_undo(true), "{}", host.status);
    until(&mut host, &mut guest, |h, g| {
        h.wb.sheet(0).value(0, 3) == Value::Number(50.0)
            && g.wb.sheet(0).value(0, 3) == Value::Number(50.0)
    });

    // 5) Guest deletes DefinedName -> converges across host and guest.
    let del_idx = guest
        .wb
        .names
        .iter()
        .position(|(n, _, _)| n == "TaxRate")
        .unwrap();
    assert!(
        guest
            .submit_defined_name_intent(
                DefinedNameIntent::Remove { index: del_idx },
                "删除名称",
                (0, 0, 0),
            )
            .unwrap()
    );
    until(&mut host, &mut guest, |h, g| {
        !h.wb.names.iter().any(|(n, _, _)| n == "TaxRate")
            && !g.wb.names.iter().any(|(n, _, _)| n == "TaxRate")
    });
}

#[test]
fn phase5_tx_room_axis_permute_converges_and_fences_locks_and_rejoin_outbox() {
    use sheetcore::{
        charts::{Chart, ChartKind, Series},
        sparkline::Sparkline,
        Cell, Value,
    };
    let env = crate::test_env::TestEnv::new("p5-tx-room-axis-permute");
    let baseline = |wb: &mut sheetcore::Workbook| {
        let sh = wb.sheet_mut(0);
        sh.charts.push(Chart {
            kind: ChartKind::Column,
            title: "MoveChart".into(),
            anchor: (0, 5, 8, 10),
            source_range: "Sheet1!$A$1:$A$2".into(),
            series: vec![Series {
                name: "S1".into(),
                val_ref: "Sheet1!$A$1:$A$2".into(),
                ..Default::default()
            }],
            ..Default::default()
        });
        sh.sparklines.push(Sparkline {
            r: 0,
            c: 3,
            rect: (0, 0, 0, 1),
            ..Default::default()
        });
    };
    let (mut host, mut guest) = sort_apps_with_baseline(&env, baseline);
    // sort_apps_with_baseline seeds A1=2 (bold), A2=1; add A3=3 and B4='=A1*10+A2' (=21).
    host.wb.sheet_mut(0).set(2, 0, Cell { value: Value::Number(3.0), formula: None });
    host.wb.sheet_mut(0).set(
        3,
        1,
        Cell {
            value: Value::Empty,
            formula: Some("=A1*10+A2".into()),
        },
    );
    host.recalc_and_broadcast(&[(0, 2, 0), (0, 3, 1)]);
    until(&mut host, &mut guest, |h, g| {
        h.wb.sheet(0).value(3, 1) == Value::Number(21.0)
            && g.wb.sheet(0).value(2, 0) == Value::Number(3.0)
            && g.wb.sheet(0).value(3, 1) == Value::Number(21.0)
    });

    // 1) Host holds an active cell lock on row 0 -> guest AxisPermute across row 0 must be rejected!
    host.sel = (0, 0);
    host.sel_end = (0, 0);
    host.edit_sheet = Some(0);
    host.edit_sel = Some((0, 0));
    host.pending_edit = Some("2".into());
    host.net.as_ref().unwrap().request_lock(0, 0, 0, "H");
    until(&mut host, &mut guest, |h, g| {
        h.my_lock == Some((0, 0, 0)) && g.locks.is_locked_by_other(0, 0, 0, 2)
    });
    assert!(!guest.move_axis_headers(true, 0, 1, 3));
    assert!(
        guest.status.contains("正在编辑"),
        "expected lock conflict rejection, got: {}",
        guest.status
    );
    host.editing = None;
    host.pending_edit = None;
    host.edit_sel = None;
    host.edit_sheet = None;
    host.my_lock = None;
    host.locks.release(0, 0, 0, 1);
    host.net.as_ref().unwrap().release_lock(0, 0, 0);
    until(&mut host, &mut guest, |h, g| {
        !h.locks.is_locked_by_other(0, 0, 0, 2) && !g.locks.is_locked_by_other(0, 0, 0, 2)
    });

    // Place a remote cursor on row 0 from host so we verify cursor permutation too.
    host.net.as_ref().unwrap().broadcast_cursor(sheetcore::sync::Cursor {
        site: 1,
        name: "H".into(),
        color: 1,
        sheet: 0,
        row: 0,
        col: 0,
        end_row: Some(0),
        end_col: Some(0),
    });
    until(&mut host, &mut guest, |_, g| {
        g.remote.get(&1).is_some_and(|c| c.3 == 0 && c.4 == 0)
    });

    // 2) Guest executes move_axis_headers (move row 0 before drop=3 -> ends up at row 2!).
    let host_epoch_before = host.wb.coord_epoch();
    let guest_epoch_before = guest.wb.coord_epoch();
    assert!(guest.move_axis_headers(true, 0, 1, 3), "{}", guest.status);
    until(&mut host, &mut guest, |h, g| {
        h.wb.sheet(0).value(2, 0) == Value::Number(2.0)
            && g.wb.sheet(0).value(2, 0) == Value::Number(2.0)
            && !g.net.as_ref().unwrap().pending_structure()
    });
    assert!(host.wb.coord_epoch() > host_epoch_before);
    assert!(guest.wb.coord_epoch() > guest_epoch_before);
    assert!(guest.shift_intro.applied_seq > 0);
    assert!(host.shift_intro.applied_seq > 0);
    // Remote cursor on row 0 moved to row 2 on guest.
    assert_eq!(guest.remote.get(&1).map(|c| (c.3, c.4)), Some((2, 0)));

    for peer in [&host, &guest] {
        let sh = peer.wb.sheet(0);
        assert_eq!(sh.value(0, 0), Value::Number(1.0));
        assert_eq!(sh.value(1, 0), Value::Number(3.0));
        assert_eq!(sh.value(2, 0), Value::Number(2.0));
        // Bold style followed row 0 -> row 2.
        assert!(peer.wb.styles.get(sh.style_of(2, 0)).bold);
        assert!(!peer.wb.styles.get(sh.style_of(0, 0)).bold);
        // Formula =A1*10+A2 rewrote to =A3*10+A1 -> 2*10+1 = 21!
        assert_eq!(sh.get(3, 1).unwrap().formula.as_deref(), Some("=A3*10+A1"));
        assert_eq!(sh.value(3, 1), Value::Number(21.0));
        // Sparkline anchor & rect moved from row 0 to row 2.
        assert_eq!(sh.sparklines.len(), 1);
        assert_eq!((sh.sparklines[0].r, sh.sparklines[0].c), (2, 3));
        assert_eq!(sh.sparklines[0].rect, (2, 0, 2, 1));
    }

    // 3) Stale rejoin outbox captured at old shift_seq (0) is fenced & discarded cleanly.
    let stale_coord = crate::cell_shift::OutboxCoord {
        room_id: guest.net.as_ref().unwrap().connected_room_id(),
        epoch: crate::net::CoordEpoch {
            struct_rev: 0,
            shift_seq: 0,
        },
    };
    guest.rejoin_outbox = Some((
        guest.doc_uid,
        vec![sheetcore::sync::Op {
            site: 2,
            sheet: 0,
            row: 0,
            col: 0,
            ts: 9999,
            value: sheetcore::sync::ValueWire::Number(777.0),
            formula: None,
        }],
    ));
    guest.rejoin_outbox_meta = Some(crate::cell_shift::RejoinOutboxMeta {
        doc_uid: guest.doc_uid,
        coord: stale_coord,
    });
    assert_eq!(guest.discard_stale_rejoin_outboxes(), 1);
    assert!(guest.rejoin_outbox.is_none());

    // 4) Late joiner receives full authoritative post-permute state.
    let mut late = late_join(&env, &guest, baseline);
    until(&mut host, &mut late, |_, l| {
        l.joined_doc
            && !l.in_snapshot
            && l.wb.sheet(0).value(2, 0) == Value::Number(2.0)
            && l.wb.sheet(0).value(3, 1) == Value::Number(21.0)
            && l.wb.sheet(0).sparklines.first().is_some_and(|sp| sp.r == 2)
    });

    // 5) Guest Undo -> reverts row 2 back to row 0 across host and guest; Redo -> back to row 2.
    assert!(guest.apply_undo(false), "{}", guest.status);
    until(&mut host, &mut guest, |h, g| {
        h.wb.sheet(0).value(0, 0) == Value::Number(2.0)
            && g.wb.sheet(0).value(0, 0) == Value::Number(2.0)
            && h.wb.sheet(0).sparklines.first().is_some_and(|sp| sp.r == 0)
            && g.wb.sheet(0).sparklines.first().is_some_and(|sp| sp.r == 0)
    });
    assert!(guest.apply_undo(true), "{}", guest.status);
    until(&mut host, &mut guest, |h, g| {
        h.wb.sheet(0).value(2, 0) == Value::Number(2.0)
            && g.wb.sheet(0).value(2, 0) == Value::Number(2.0)
            && h.wb.sheet(0).sparklines.first().is_some_and(|sp| sp.r == 2)
            && g.wb.sheet(0).sparklines.first().is_some_and(|sp| sp.r == 2)
    });
}

#[test]
fn phase5_tx_room_cell_shift_updates_chart_and_sparkline_and_rejects_straddling_sparkline() {
    use sheetcore::{
        charts::{Chart, ChartKind, Series},
        sparkline::Sparkline,
        Cell, Value,
    };
    let env = crate::test_env::TestEnv::new("p5-tx-room-shift-chart-spark");
    let baseline = |wb: &mut sheetcore::Workbook| {
        let sh = wb.sheet_mut(0);
        // Chart referencing column A rows 2..4 (0-based 1..=3)
        sh.charts.push(Chart {
            kind: ChartKind::Line,
            title: "Trend".into(),
            anchor: (0, 4, 8, 10),
            source_range: "Sheet1!$A$2:$A$4".into(),
            series: vec![Series {
                name: "Values".into(),
                val_ref: "Sheet1!$A$2:$A$4".into(),
                ..Default::default()
            }],
            ..Default::default()
        });
        // Wholly-in-band vertical sparkline in column A (anchor at A6, data A2:A4)
        sh.sparklines.push(Sparkline {
            r: 5,
            c: 0,
            rect: (1, 0, 3, 0),
            ..Default::default()
        });
    };
    let (mut host, mut guest) = sort_apps_with_baseline(&env, baseline);
    host.wb.sheet_mut(0).set(2, 0, Cell { value: Value::Number(30.0), formula: None });
    host.wb.sheet_mut(0).set(3, 0, Cell { value: Value::Number(40.0), formula: None });
    host.recalc_and_broadcast(&[(0, 2, 0), (0, 3, 0)]);
    until(&mut host, &mut guest, |_, g| {
        g.wb.sheet(0).value(2, 0) == Value::Number(30.0)
            && g.wb.sheet(0).value(3, 0) == Value::Number(40.0)
    });

    // 1) Guest inserts a cell at A2 (shift down in column A): Chart val_ref -> $A$3:$A$5, Sparkline -> anchor (6,0), rect (2,0,4,0).
    guest.sel = (1, 0);
    guest.sel_end = (1, 0);
    guest.shift_cells(true, false);
    until(&mut host, &mut guest, |h, g| {
        h.wb.sheet(0).charts[0].series[0].val_ref == "Sheet1!$A$3:$A$5"
            && g.wb.sheet(0).charts[0].series[0].val_ref == "Sheet1!$A$3:$A$5"
            && h.wb.sheet(0).sparklines[0].r == 6
            && g.wb.sheet(0).sparklines[0].r == 6
            && h.wb.sheet(0).sparklines[0].rect == (2, 0, 4, 0)
            && g.wb.sheet(0).sparklines[0].rect == (2, 0, 4, 0)
            && !g.net.as_ref().unwrap().pending_structure()
    });

    // Late joiner sees updated chart ref and shifted sparkline via CellShiftOverlay.
    let mut late = late_join(&env, &guest, baseline);
    until(&mut host, &mut late, |_, l| {
        l.joined_doc
            && !l.in_snapshot
            && l.wb.sheet(0).charts[0].series[0].val_ref == "Sheet1!$A$3:$A$5"
            && l.wb.sheet(0).sparklines[0].r == 6
            && l.wb.sheet(0).sparklines[0].rect == (2, 0, 4, 0)
    });

    // 2) Undo & Redo restore chart series ref and sparkline identically across host & guest.
    assert!(guest.apply_undo(false), "{}", guest.status);
    until(&mut host, &mut guest, |h, g| {
        h.wb.sheet(0).charts[0].series[0].val_ref == "Sheet1!$A$2:$A$4"
            && g.wb.sheet(0).charts[0].series[0].val_ref == "Sheet1!$A$2:$A$4"
            && h.wb.sheet(0).sparklines[0].r == 5
            && g.wb.sheet(0).sparklines[0].r == 5
            && h.wb.sheet(0).sparklines[0].rect == (1, 0, 3, 0)
            && g.wb.sheet(0).sparklines[0].rect == (1, 0, 3, 0)
    });
    assert!(guest.apply_undo(true), "{}", guest.status);
    until(&mut host, &mut guest, |h, g| {
        h.wb.sheet(0).charts[0].series[0].val_ref == "Sheet1!$A$3:$A$5"
            && g.wb.sheet(0).charts[0].series[0].val_ref == "Sheet1!$A$3:$A$5"
            && h.wb.sheet(0).sparklines[0].r == 6
            && g.wb.sheet(0).sparklines[0].r == 6
    });

    // 3) Band-straddling horizontal sparkline (A10:C10) with vertical shift in column A only -> rejected cleanly!
    host.wb.sheet_mut(0).sparklines.push(Sparkline {
        r: 9,
        c: 4,
        rect: (9, 0, 9, 2),
        ..Default::default()
    });
    let epoch_before = host.wb.coord_epoch();
    host.sel = (8, 0);
    host.sel_end = (8, 0);
    host.shift_cells(true, false);
    assert!(
        host.status.contains("迷你图"),
        "straddling sparkline must reject with Chinese status: {}",
        host.status
    );
    assert_eq!(host.wb.coord_epoch(), epoch_before);
}

#[test]
fn phase5_tx_room_tx_reconnect_receipt_clears_pending_structure() {
    use sheetcore::sync::CellShiftPhase;
    use sheetcore::tx::{DefinedNameIntent, DefinedNamesPatch, RoomTxPatch};
    use std::sync::Arc;

    let env = crate::test_env::TestEnv::new("phase5-room-tx-receipt");
    let (mut host, mut guest) = sort_apps(&env);

    // Commit a guest-authored DefinedNames RoomTx with a known transaction id so host's
    // journal records it as guest's latest receipt.
    let intent = DefinedNameIntent::Define {
        name: "ReceiptRate".into(),
        scope: None,
        formula: "0.25".into(),
    };
    let patch = Arc::new(RoomTxPatch::DefinedNames(
        DefinedNamesPatch::prepare(&guest.wb, &intent).expect("prepare DefinedNamesPatch"),
    ));
    let tx_id = 0x5152_5354u64;
    guest
        .net
        .as_ref()
        .unwrap()
        .request_room_tx(tx_id, CellShiftPhase::Forward, patch.clone())
        .expect("send RoomTxRequest");
    until(&mut host, &mut guest, |h, g| {
        h.wb.names.iter().any(|(n, _, _)| n == "ReceiptRate")
            && g.wb.names.iter().any(|(n, _, _)| n == "ReceiptRate")
            && !g.net.as_ref().unwrap().pending_structure()
    });

    // Simulate guest having missed the live RoomTxCommitted before reconnecting:
    // clear undo history, re-arm room_tx_pending + pending_structure for tx_id, and
    // trigger a real snapshot resync so host delivers Msg::RoomTxReceipt.
    guest.undo.clear();
    guest
        .net
        .as_ref()
        .unwrap()
        .arm_room_tx_pending_for_test(tx_id, CellShiftPhase::Forward, patch);
    assert!(guest.net.as_ref().unwrap().pending_structure());
    guest.net.as_ref().unwrap().request_snapshot_resync();
    until(&mut host, &mut guest, |_, g| g.room_lost);
    until(&mut host, &mut guest, |_, g| {
        g.joined_doc
            && !g.in_snapshot
            && !g.room_lost
            && g.undo.depth() == (1, 0)
            && !g.net.as_ref().unwrap().pending_structure()
    });

    // Subsequent RoomTx from guest must NOT be stuck behind pending_structure.
    assert!(
        guest
            .submit_defined_name_intent(
                DefinedNameIntent::Define {
                    name: "AfterReceipt".into(),
                    scope: None,
                    formula: "0.5".into(),
                },
                "定义名称 AfterReceipt",
                (0, 0, 0),
            )
            .unwrap()
    );
    until(&mut host, &mut guest, |h, g| {
        h.wb.names.iter().any(|(n, _, _)| n == "AfterReceipt")
            && g.wb.names.iter().any(|(n, _, _)| n == "AfterReceipt")
            && !g.net.as_ref().unwrap().pending_structure()
    });
}

fn seed_phase6_a1_rich_metadata_baseline(wb: &mut sheetcore::Workbook) {
    use sheetcore::{
        charts::{Chart, ChartKind, Series},
        pivot::{Agg, GroupBy, PivotResult, PivotSpec},
        protection::ProtectedRange,
        scenarios::Scenario,
        sort_filter::Filter,
        sparkline::Sparkline,
        table::Table,
    };
    let sh = wb.sheet_mut(0);
    // Keep A1=2, A2=1 seeded by `sort_apps_with_baseline`; attach rich metadata matching A1:B2.
    sh.tables.push(Table {
        name: "Table1".into(),
        display: "Table1".into(),
        range: (0, 0, 1, 1),
        columns: vec!["Item".into(), "Val".into()],
        column_ids: vec![1, 2],
        header_row: true,
        totals_row: false,
        ..Default::default()
    });
    sh.rows.set(0, 60.0);
    sh.cols.set(0, 140.0);
    sh.hidden_rows_user.insert(1);
    sh.hidden_cols.insert(1);
    sh.freeze = Some((1, 1));
    sh.filter = Some(Filter::new(0, 0, 1, 1));
    sh.protected_ranges
        .push(ProtectedRange::new("EditA1B2", vec![(0, 0, 1, 1)], ""));
    let mut sc = Scenario::new("BaseCase");
    sc.cells = vec![((0, 0), "2".into()), ((1, 1), "42".into())];
    sh.scenarios.push(sc);
    sh.charts.push(Chart {
        kind: ChartKind::Column,
        title: "A1Chart".into(),
        anchor: (0, 5, 8, 10),
        source_range: "Sheet1!$A$1:$B$2".into(),
        series: vec![Series {
            name: "Val".into(),
            val_ref: "Sheet1!$B$1:$B$2".into(),
            ..Default::default()
        }],
        ..Default::default()
    });
    sh.sparklines.push(Sparkline {
        r: 0,
        c: 4,
        rect: (0, 1, 1, 1),
        ..Default::default()
    });
    wb.pivots_pending.push((
        0,
        PivotSpec {
            source: (0, 0, 1, 1),
            row_field: 0,
            col_field: None,
            value_field: 1,
            agg: Agg::Sum,
            group: GroupBy::None,
            exclude: Vec::new(),
        },
        PivotResult::default(),
        (6, 0, 8, 1),
        "PivotA1".into(),
    ));
}

fn seed_phase6_a1_live_cells(host: &mut SheetApp, guest: &mut SheetApp) {
    use sheetcore::{Cell, Value};
    host.wb.sheet_mut(0).set(0, 1, Cell { value: Value::Number(100.0), formula: None });
    host.wb.sheet_mut(0).set(1, 1, Cell { value: Value::Number(42.0), formula: None });
    host.wb.sheet_mut(0).set(2, 0, Cell { value: Value::Number(3.0), formula: None });
    host.wb.sheet_mut(0).set(2, 1, Cell { value: Value::Number(10.0), formula: None });
    host.wb.sheet_mut(0).set(3, 0, Cell { value: Value::Number(4.0), formula: None });
    host.wb.sheet_mut(0).set(3, 1, Cell { value: Value::Number(20.0), formula: None });
    host.wb.sheet_mut(0).set(4, 0, Cell { value: Value::Empty, formula: Some("=B2+B3".into()) });
    host.recalc_and_broadcast(&[
        (0, 0, 1),
        (0, 1, 1),
        (0, 2, 0),
        (0, 2, 1),
        (0, 3, 0),
        (0, 3, 1),
        (0, 4, 0),
    ]);
    until(host, guest, |h, g| {
        h.wb.sheet(0).value(4, 0) == Value::Number(52.0)
            && g.wb.sheet(0).value(4, 0) == Value::Number(52.0)
            && g.wb.sheet(0).value(3, 1) == Value::Number(20.0)
    });
}

fn assert_phase6_a1_metadata_matches_host(host: &SheetApp, peer: &SheetApp, label: &str) {
    let h = host.wb.sheet(0);
    let p = peer.wb.sheet(0);
    assert_eq!(p.tables, h.tables, "{label}: tables mismatch");
    for r in 0..8u32 {
        assert!(
            (p.rows.size(r) - h.rows.size(r)).abs() < 1e-4,
            "{label}: row {r} height mismatch (peer={}, host={})",
            p.rows.size(r),
            h.rows.size(r)
        );
    }
    for c in 0..8u32 {
        assert!(
            (p.cols.size(c) - h.cols.size(c)).abs() < 1e-4,
            "{label}: col {c} width mismatch (peer={}, host={})",
            p.cols.size(c),
            h.cols.size(c)
        );
    }
    assert_eq!(p.hidden_rows, h.hidden_rows, "{label}: hidden_rows mismatch");
    assert_eq!(
        p.hidden_rows_user, h.hidden_rows_user,
        "{label}: hidden_rows_user mismatch"
    );
    assert_eq!(p.hidden_cols, h.hidden_cols, "{label}: hidden_cols mismatch");
    assert_eq!(p.freeze, h.freeze, "{label}: freeze mismatch");
    assert_eq!(p.filter, h.filter, "{label}: filter mismatch");
    assert_eq!(
        p.protected_ranges, h.protected_ranges,
        "{label}: protected_ranges mismatch"
    );
    assert_eq!(p.scenarios, h.scenarios, "{label}: scenarios mismatch");
    assert_eq!(p.charts.len(), h.charts.len(), "{label}: charts len mismatch");
    for (pc, hc) in p.charts.iter().zip(h.charts.iter()) {
        assert_eq!(pc.anchor, hc.anchor, "{label}: chart anchor mismatch");
        assert_eq!(pc.source_range, hc.source_range, "{label}: chart source_range mismatch");
        assert_eq!(pc.series.len(), hc.series.len(), "{label}: chart series len mismatch");
        for (ps, hs) in pc.series.iter().zip(hc.series.iter()) {
            assert_eq!(ps.val_ref, hs.val_ref, "{label}: chart series val_ref mismatch");
        }
    }
    assert_eq!(p.sparklines, h.sparklines, "{label}: sparklines mismatch");
    assert_eq!(
        peer.wb.pivots_pending.len(),
        host.wb.pivots_pending.len(),
        "{label}: pivots_pending len mismatch"
    );
    for (pp, hp) in peer.wb.pivots_pending.iter().zip(host.wb.pivots_pending.iter()) {
        assert_eq!(pp.0, hp.0, "{label}: pivot sheet mismatch");
        assert_eq!(pp.1.source, hp.1.source, "{label}: pivot source mismatch");
        assert_eq!(pp.3, hp.3, "{label}: pivot output mismatch");
        assert_eq!(pp.4, hp.4, "{label}: pivot name mismatch");
    }
    // Verify live layout cache on peer reflects updated heights/widths & hidden axes.
    let (p_cols, p_rows) = peer.view_axes();
    let (h_cols, h_rows) = host.view_axes();
    for r in 0..8u32 {
        assert!(
            (p_rows.size(r) - h_rows.size(r)).abs() < 1e-4,
            "{label}: view_axes row {r} mismatch"
        );
    }
    for c in 0..8u32 {
        assert!(
            (p_cols.size(c) - h_cols.size(c)).abs() < 1e-4,
            "{label}: view_axes col {c} mismatch"
        );
    }
    for r in 0..6u32 {
        for c in 0..6u32 {
            assert_eq!(
                p.value(r, c),
                h.value(r, c),
                "{label}: cell ({r},{c}) value mismatch"
            );
            assert_eq!(
                p.formula_of(r, c),
                h.formula_of(r, c),
                "{label}: cell ({r},{c}) formula mismatch"
            );
        }
    }
}

#[test]
fn phase6_a1_axis_permute_late_join_restores_all_metadata_row_and_col_moves() {
    let env = crate::test_env::TestEnv::new("p6-a1-late-join-meta");
    let (mut host, mut guest) = sort_apps_with_baseline(&env, seed_phase6_a1_rich_metadata_baseline);
    seed_phase6_a1_live_cells(&mut host, &mut guest);

    // 1) Row move: move rows 0..=1 before drop=4 -> Table1 A1:B2 moves to A3:B4, row 0 height 60 -> row 2.
    assert!(guest.move_axis_headers(true, 0, 2, 4), "{}", guest.status);
    until(&mut host, &mut guest, |h, g| {
        h.wb.sheet(0).tables.first().is_some_and(|t| t.range == (2, 0, 3, 1))
            && g.wb.sheet(0).tables.first().is_some_and(|t| t.range == (2, 0, 3, 1))
            && !g.net.as_ref().unwrap().pending_structure()
    });
    assert!((host.wb.sheet(0).rows.size(2) - 60.0).abs() < 1e-4);
    assert!(host.wb.sheet(0).hidden_rows_user.contains(&3));
    assert!(!host.wb.sheet(0).hidden_rows_user.contains(&1));
    assert_phase6_a1_metadata_matches_host(&host, &guest, "online-after-row-move");

    // Third client joins from unsaved original disk baseline (host has NOT saved).
    let mut late_row = late_join(&env, &guest, seed_phase6_a1_rich_metadata_baseline);
    // Prime late_row's view_axes cache before snapshot completes to verify cache invalidation.
    let _ = late_row.view_axes();
    until(&mut host, &mut late_row, |_, l| {
        l.joined_doc && !l.in_snapshot && l.shift_intro.applied_seq > 0
    });
    assert!(!late_row.snapshot_is_delta);
    assert_eq!(late_row.wb.sheet(0).tables[0].range, (2, 0, 3, 1));
    assert!((late_row.wb.sheet(0).rows.size(2) - 60.0).abs() < 1e-4);
    assert_phase6_a1_metadata_matches_host(&host, &late_row, "late-join-after-row-move");

    // 2) Column move: move cols 0..=1 before drop=4 -> Table1 A3:B4 moves to C3:D4, col 0 width 140 -> col 2.
    let prev_seq = host.shift_intro.applied_seq;
    assert!(guest.move_axis_headers(false, 0, 2, 4), "{}", guest.status);
    until(&mut host, &mut guest, |h, g| {
        h.shift_intro.applied_seq > prev_seq
            && g.shift_intro.applied_seq > prev_seq
            && h.wb.sheet(0).tables[0].range == (2, 2, 3, 3)
            && g.wb.sheet(0).tables[0].range == (2, 2, 3, 3)
            && !g.net.as_ref().unwrap().pending_structure()
    });
    assert!((host.wb.sheet(0).cols.size(2) - 140.0).abs() < 1e-4);
    assert!(host.wb.sheet(0).hidden_cols.contains(&3));
    assert!(!host.wb.sheet(0).hidden_cols.contains(&1));
    assert_phase6_a1_metadata_matches_host(&host, &guest, "online-after-col-move");

    // Another late joiner from unsaved disk baseline sees BOTH row + col moves across all metadata.
    let mut late_col = {
        let mut app = SheetApp::new_with_context(&eframe::egui::Context::default(), None);
        app.wb.path = Some(env.path("late_col.xlsx"));
        seed_phase6_a1_rich_metadata_baseline(&mut app.wb);
        let addr = &guest.net.as_ref().unwrap().addr;
        app.net = Some(crate::net::join(4, "LateCol".into(), "doc".into(), addr, "sort".into()).unwrap());
        app.peer = Some(sheetcore::sync::Peer::new(4));
        app.undo.set_site(4);
        app
    };
    let _ = late_col.view_axes();
    until(&mut host, &mut late_col, |_, l| {
        l.joined_doc && !l.in_snapshot && l.shift_intro.applied_seq > prev_seq
    });
    assert_eq!(late_col.wb.sheet(0).tables[0].range, (2, 2, 3, 3));
    assert!((late_col.wb.sheet(0).rows.size(2) - 60.0).abs() < 1e-4);
    assert!((late_col.wb.sheet(0).cols.size(2) - 140.0).abs() < 1e-4);
    assert_phase6_a1_metadata_matches_host(&host, &late_col, "late-join-after-row-and-col-move");
}

#[test]
fn phase6_a1_axis_permute_reconnect_full_and_delta_converges_all_metadata() {
    use sheetcore::{Cell, Value};
    let env = crate::test_env::TestEnv::new("p6-a1-reconnect-meta");

    // 1) Disconnected guest missing AxisPermute reconnecting via explicit full resync AND
    //    via socket drop (delta-attempt handshake that safely upgrades to authoritative snapshot + overlay).
    for force_full in [true, false] {
        let (mut host, mut guest) =
            sort_apps_with_baseline(&env, seed_phase6_a1_rich_metadata_baseline);
        seed_phase6_a1_live_cells(&mut host, &mut guest);
        // Prime guest layout cache & transient pivot popup to verify SnapshotDone invalidates both.
        let _ = guest.view_axes();
        guest.pivot_ui.options_pivot_id = Some(99);

        if force_full {
            guest.net.as_ref().unwrap().request_snapshot_resync();
        } else {
            guest.net.as_ref().unwrap().disconnect_socket_for_test();
        }
        until(&mut host, &mut guest, |_, g| g.room_lost);

        // Host commits AxisPermute (rows 0..=1 -> rows 2..=3) while guest is offline.
        assert!(host.move_axis_headers(true, 0, 2, 4), "{}", host.status);
        until(&mut host, &mut guest, |h, _| {
            h.wb.sheet(0).tables[0].range == (2, 0, 3, 1)
                && !h.net.as_ref().unwrap().pending_structure()
        });
        let permute_seq = host.shift_intro.applied_seq;
        assert!(permute_seq > 0);

        until(&mut host, &mut guest, |_, g| {
            g.joined_doc
                && !g.in_snapshot
                && !g.room_lost
                && g.shift_intro.applied_seq >= permute_seq
        });
        assert!(
            guest.pivot_ui.options_pivot_id.is_none(),
            "SnapshotDone overlay must clear transient pivot popup (force_full={force_full})"
        );
        assert_eq!(guest.wb.sheet(0).tables[0].range, (2, 0, 3, 1));
        assert!((guest.wb.sheet(0).rows.size(2) - 60.0).abs() < 1e-4);
        assert_phase6_a1_metadata_matches_host(
            &host,
            &guest,
            if force_full {
                "missed-permute-full-resync"
            } else {
                "missed-permute-socket-drop-reconnect"
            },
        );
    }

    // 2) True delta snapshot reconnect (`snapshot_is_delta == true`) AFTER AxisPermute:
    //    host edits a cell and changes row height & Table1 while guest is briefly disconnected;
    //    guest resumes via delta snapshot + TxOverlay and converges cleanly.
    let (mut host, mut guest) =
        sort_apps_with_baseline(&env, seed_phase6_a1_rich_metadata_baseline);
    seed_phase6_a1_live_cells(&mut host, &mut guest);
    assert!(host.move_axis_headers(true, 0, 2, 4), "{}", host.status);
    until(&mut host, &mut guest, |h, g| {
        h.wb.sheet(0).tables[0].range == (2, 0, 3, 1)
            && g.wb.sheet(0).tables[0].range == (2, 0, 3, 1)
            && g.shift_intro.applied_seq == h.shift_intro.applied_seq
            && g.shift_intro.applied_seq > 0
    });
    // Prime guest's cached view_axes at height 60.0.
    assert!((guest.view_axes().1.size(2) - 60.0).abs() < 1e-4);

    guest.net.as_ref().unwrap().disconnect_socket_for_test();
    until(&mut host, &mut guest, |_, g| g.room_lost);

    // Host mutates a cell + row 2 height to 85.0 + freeze pane while guest is disconnected.
    host.wb.sheet_mut(0).set(
        5,
        0,
        Cell {
            value: Value::Number(555.0),
            formula: None,
        },
    );
    host.wb.sheet_mut(0).rows.set(2, 85.0);
    host.wb.sheet_mut(0).freeze = Some((2, 1));
    host.broadcast_cells(&[(0, 5, 0)]);

    until(&mut host, &mut guest, |_, g| {
        g.joined_doc
            && !g.in_snapshot
            && !g.room_lost
            && g.wb.sheet(0).value(5, 0) == Value::Number(555.0)
    });
    assert!(
        guest.snapshot_is_delta,
        "post-permute brief disconnect must exercise true delta snapshot path"
    );
    assert!((guest.wb.sheet(0).rows.size(2) - 85.0).abs() < 1e-4);
    assert!((guest.view_axes().1.size(2) - 85.0).abs() < 1e-4);
    assert_eq!(guest.wb.sheet(0).freeze, Some((2, 1)));
    assert_phase6_a1_metadata_matches_host(&host, &guest, "post-permute-true-delta-reconnect");
}

#[test]
fn phase6_a3_whole_axis_insert_rejects_tail_overflow_solo_and_room() {
    use sheetcore::{
        cf::{CfBlock, CfKind, CfOp, CfRule, Dxf},
        structural::{CellShiftConflict, Structural},
        table::Table,
        Cell, Value,
    };
    let env = crate::test_env::TestEnv::new("p6-a3-axis-overflow");

    // --- Part 1: Solo UI structural insert overflow + valid table/full-axis CF ---
    {
        let mut app = SheetApp::new_with_context(&eframe::egui::Context::default(), None);
        app.wb.sheets.push(sheetcore::model::Sheet::new("Sheet2"));
        app.wb.sheet_mut(0).set(
            1_048_575,
            0,
            Cell {
                value: Value::Number(7.0),
                formula: None,
            },
        );
        app.wb.sheet_mut(1).set(
            0,
            0,
            Cell {
                value: Value::Empty,
                formula: Some("=Sheet1!A1048576".into()),
            },
        );
        // Full-axis CF rule (0..=1_048_575) must NOT falsely trigger overflow once tail cell is cleared.
        app.wb.sheet_mut(0).cf.dxfs.push(Dxf::default());
        app.wb.sheet_mut(0).cf.blocks.push(CfBlock {
            ranges: vec![(0, 2, 1_048_575, 2)],
            rules: vec![CfRule {
                kind: CfKind::CellIs {
                    op: CfOp::Greater,
                    a: "0".into(),
                    b: String::new(),
                    dxf: 0,
                },
                stop: false,
                priority: 1,
            }],
        });
        app.wb.sheet_mut(0).tables.push(Table {
            name: "Table1".into(),
            display: "Table1".into(),
            range: (0, 0, 1, 1),
            columns: vec!["K".into(), "V".into()],
            column_ids: vec![1, 2],
            ..Default::default()
        });
        sheetcore::formula::recalc_all(&mut app.wb);
        assert_eq!(app.wb.sheet(1).value(0, 0), Value::Number(7.0));
        app.wb.dirty = false;

        // 1a) Tail row A1048576 occupied -> InsertRows rejects cleanly and mutates nothing.
        let epoch0 = app.wb.coord_epoch();
        app.sheet_idx = 0;
        app.structural(Structural::InsertRows { at: 0, n: 1 });
        assert_eq!(app.status, CellShiftConflict::Overflow.message());
        assert_eq!(app.wb.coord_epoch(), epoch0);
        assert_eq!(app.undo.depth(), (0, 0));
        assert!(!app.wb.dirty);
        assert_eq!(app.wb.sheet(0).value(1_048_575, 0), Value::Number(7.0));
        assert_eq!(
            app.wb.sheet(1).formula_of(0, 0),
            Some("=Sheet1!A1048576")
        );
        assert_eq!(app.wb.sheet(1).value(0, 0), Value::Number(7.0));
        assert_eq!(app.wb.sheet(0).tables[0].range, (0, 0, 1, 1));

        // 1b) Tail col XFD1 occupied -> InsertCols rejects cleanly and mutates nothing.
        app.wb.sheet_mut(0).set(
            0,
            16_383,
            Cell {
                value: Value::Number(9.0),
                formula: None,
            },
        );
        app.wb.dirty = false;
        app.structural(Structural::InsertCols { at: 0, n: 1 });
        assert_eq!(app.status, CellShiftConflict::Overflow.message());
        assert_eq!(app.wb.coord_epoch(), epoch0);
        assert_eq!(app.undo.depth(), (0, 0));
        assert!(!app.wb.dirty);
        assert_eq!(app.wb.sheet(0).value(0, 16_383), Value::Number(9.0));

        // 1c) Once A1048576 & XFD1 are cleared, whole-row InsertRows STILL succeeds with
        //     full-axis CF rule (0..=1_048_575) and moves Table1 cleanly to A2:B3!
        app.wb.sheet_mut(0).remove(1_048_575, 0);
        app.wb.sheet_mut(0).remove(0, 16_383);
        app.structural(Structural::InsertRows { at: 0, n: 1 });
        assert!(app.wb.coord_epoch() > epoch0);
        assert_eq!(app.undo.depth(), (1, 0));
        assert_eq!(app.wb.sheet(0).tables[0].range, (1, 0, 2, 1));
        assert_eq!(app.wb.sheet(0).cf.blocks[0].ranges, vec![(0, 2, 1_048_575, 2)]);
    }

    // --- Part 2: Room mode (host preflight + guest local preflight + host authoritative CheckStructural) ---
    {
        let baseline = |wb: &mut sheetcore::Workbook| {
            wb.sheets.push(sheetcore::model::Sheet::new("Sheet2"));
            wb.sheet_mut(0).cf.dxfs.push(Dxf::default());
            wb.sheet_mut(0).cf.blocks.push(CfBlock {
                ranges: vec![(0, 2, 1_048_575, 2)],
                rules: vec![CfRule {
                    kind: CfKind::CellIs {
                        op: CfOp::Greater,
                        a: "0".into(),
                        b: String::new(),
                        dxf: 0,
                    },
                    stop: false,
                    priority: 1,
                }],
            });
            wb.sheet_mut(0).tables.push(Table {
                name: "Table1".into(),
                display: "Table1".into(),
                range: (0, 0, 1, 1),
                columns: vec!["K".into(), "V".into()],
                column_ids: vec![1, 2],
                ..Default::default()
            });
        };
        let (mut host, mut guest) = sort_apps_with_baseline(&env, baseline);

        // Place A1048576 ONLY on host (simulating stale guest view) + cross-sheet ref on host.
        host.wb.sheet_mut(0).set(
            1_048_575,
            0,
            Cell {
                value: Value::Number(7.0),
                formula: None,
            },
        );
        host.wb.sheet_mut(1).set(
            0,
            0,
            Cell {
                value: Value::Empty,
                formula: Some("=Sheet1!A1048576".into()),
            },
        );
        sheetcore::formula::recalc_all(&mut host.wb);

        let host_epoch = host.wb.coord_epoch();
        let guest_epoch = guest.wb.coord_epoch();
        let host_undo = host.undo.depth();
        let guest_undo = guest.undo.depth();

        // 2a) Host local structural rejects BEFORE broadcasting.
        host.sheet_idx = 0;
        host.structural(Structural::InsertRows { at: 0, n: 1 });
        assert_eq!(host.status, CellShiftConflict::Overflow.message());
        assert_eq!(host.wb.coord_epoch(), host_epoch);
        assert_eq!(host.undo.depth(), host_undo);
        assert_eq!(host.net.as_ref().unwrap().structure_seen(), 0);

        // 2b) Stale guest sends InsertRows request -> Room Host rejects via CheckStructural + StructuralRejected!
        guest.sheet_idx = 0;
        guest.structural(Structural::InsertRows { at: 0, n: 1 });
        assert!(guest.net.as_ref().unwrap().pending_structure());
        until(&mut host, &mut guest, |_, g| {
            !g.net.as_ref().unwrap().pending_structure()
        });
        assert_eq!(guest.status, CellShiftConflict::Overflow.message());
        assert_eq!(host.wb.coord_epoch(), host_epoch);
        assert_eq!(guest.wb.coord_epoch(), guest_epoch);
        assert_eq!(host.undo.depth(), host_undo);
        assert_eq!(guest.undo.depth(), guest_undo);
        assert_eq!(host.net.as_ref().unwrap().structure_seen(), 0);
        assert_eq!(guest.net.as_ref().unwrap().structure_seen(), 0);
        assert_eq!(host.wb.sheet(0).value(1_048_575, 0), Value::Number(7.0));
        assert_eq!(
            host.wb.sheet(1).formula_of(0, 0),
            Some("=Sheet1!A1048576")
        );

        // 2c) Same authoritative host rejection for XFD1 on InsertCols.
        host.wb.sheet_mut(0).remove(1_048_575, 0);
        host.wb.sheet_mut(0).set(
            0,
            16_383,
            Cell {
                value: Value::Number(9.0),
                formula: None,
            },
        );
        guest.structural(Structural::InsertCols { at: 0, n: 1 });
        assert!(guest.net.as_ref().unwrap().pending_structure());
        until(&mut host, &mut guest, |_, g| {
            !g.net.as_ref().unwrap().pending_structure()
        });
        assert_eq!(guest.status, CellShiftConflict::Overflow.message());
        assert_eq!(host.wb.coord_epoch(), host_epoch);
        assert_eq!(guest.wb.coord_epoch(), guest_epoch);
        assert_eq!(host.wb.sheet(0).value(0, 16_383), Value::Number(9.0));

        // 2d) Once XFD1 is cleared, guest whole-row InsertRows succeeds across host & guest,
        //     moving Table1 to A2:B3 even with full-axis CF rule present!
        host.wb.sheet_mut(0).remove(0, 16_383);
        guest.structural(Structural::InsertRows { at: 0, n: 1 });
        until(&mut host, &mut guest, |h, g| {
            h.wb.sheet(0).tables[0].range == (1, 0, 2, 1)
                && g.wb.sheet(0).tables[0].range == (1, 0, 2, 1)
                && !g.net.as_ref().unwrap().pending_structure()
        });
        assert_eq!(host.wb.sheet(0).value(1, 0), Value::Number(2.0));
        assert_eq!(guest.wb.sheet(0).value(1, 0), Value::Number(2.0));
    }
}

#[test]
fn phase6_p2a_cancelled_offline_edits_preserved_in_local_conflict_draft() {
    use sheetcore::{Cell, Value};
    use std::time::Duration;

    let env = crate::test_env::TestEnv::new("p6-p2a-conflict-draft");
    let (mut host, mut guest) = sort_apps(&env);
    let room_id = guest.net.as_ref().unwrap().connected_room_id();
    let red = [235, 30, 30];

    guest.net.as_ref().unwrap().disconnect_socket_for_test();
    until(&mut host, &mut guest, |_, g| g.room_lost);

    // Make offline unacknowledged edits: numeric cell + formula cell + style + merge.
    let val_cell = Cell {
        value: Value::Number(77.0),
        formula: None,
    };
    let formula_cell = Cell {
        value: Value::Number(4.0),
        formula: Some("=A1*2".into()),
    };
    let _ = guest.peer.as_mut().unwrap().local_set(0, 6, 1, &val_cell);
    guest.wb.sheet_mut(0).set(6, 1, val_cell);
    let _ = guest.peer.as_mut().unwrap().local_set(0, 6, 2, &formula_cell);
    guest.wb.sheet_mut(0).set(6, 2, formula_cell);
    guest.sel = (6, 1);
    guest.sel_end = (6, 1);
    assert!(guest.apply_style_xy(move |st, _, _| st.fill = Some(red)));
    guest.sel = (7, 1);
    guest.sel_end = (7, 2);
    guest.toggle_merge(true);

    // Host commits partial CellShift advancing coordinate epoch while guest is offline.
    host.sel = (1, 0);
    host.sel_end = (1, 0);
    host.shift_cells(true, false);
    until(&mut host, &mut guest, |h, _| {
        h.wb.sheet(0).value(2, 0) == Value::Number(1.0) && h.shift_intro.applied_seq > 0
    });

    until(&mut host, &mut guest, |_, g| {
        g.joined_doc
            && !g.in_snapshot
            && !g.room_lost
            && g.shift_intro.applied_seq > 0
            && g.status.contains("已取消断线前未确认")
    });
    for _ in 0..10 {
        host.poll_net();
        guest.poll_net();
        std::thread::sleep(Duration::from_millis(10));
    }

    // 1) Offline edits were NOT replayed to old coordinates on host or guest.
    for app in [&host, &guest] {
        let sh = app.wb.sheet(0);
        assert_eq!(sh.value(6, 1), Value::Empty);
        assert_eq!(sh.value(6, 2), Value::Empty);
        assert_eq!(sh.formula_of(6, 2), None);
        assert_ne!(app.wb.styles.get(sh.style_of(6, 1)).fill, Some(red));
        assert!(!sh.merges.contains(&(7, 1, 7, 2)));
    }
    assert!(
        guest.status.contains("工作表坐标已改变")
            && guest.status.contains("已取消")
            && guest.status.contains("本地冲突草稿"),
        "unexpected status: {}",
        guest.status
    );

    // 2) Cancelled offline edits are preserved & deduplicated in `cancelled_offline_drafts`.
    assert_eq!(guest.cancelled_offline_drafts.len(), 3);
    assert_eq!(guest.cancelled_offline_drafts_dropped, 0);
    let b7 = guest
        .cancelled_offline_drafts
        .iter()
        .find(|d| (d.sheet, d.row, d.col, d.merge_rect) == (0, 6, 1, None))
        .expect("missing B7 value+style draft");
    assert_eq!(b7.room_id, room_id);
    assert_eq!(b7.sheet_name, "Sheet1");
    assert_eq!(
        b7.value,
        crate::cell_shift::CancelledDraftValue::Value {
            display: "77".into(),
            formula: None,
        }
    );
    assert!(b7.style_changed);

    let c7 = guest
        .cancelled_offline_drafts
        .iter()
        .find(|d| (d.sheet, d.row, d.col, d.merge_rect) == (0, 6, 2, None))
        .expect("missing C7 formula draft");
    assert_eq!(c7.sheet_name, "Sheet1");
    assert_eq!(
        c7.value,
        crate::cell_shift::CancelledDraftValue::Value {
            display: "4".into(),
            formula: Some("=A1*2".into()),
        }
    );
    assert!(!c7.style_changed);

    let m = guest
        .cancelled_offline_drafts
        .iter()
        .find(|d| d.merge_rect == Some((7, 1, 7, 2)))
        .expect("missing B8:C8 merge draft");
    assert_eq!((m.sheet, m.row, m.col), (0, 7, 1));
    assert_eq!(m.value, crate::cell_shift::CancelledDraftValue::Unchanged);
    assert_eq!(m.merge_on, Some(true));

    // 3) TSV export & clipboard copy work cleanly.
    let tsv = guest.export_cancelled_offline_drafts_tsv();
    let lines: Vec<&str> = tsv.lines().collect();
    assert_eq!(lines.len(), 4, "header + 3 drafts: {tsv}");
    assert!(lines[0].contains("工作表\t单元格\t原值\t公式"));
    assert!(tsv.contains("Sheet1\tB7\t77\t\t是\t\t工作表坐标已改变"), "{tsv}");
    assert!(tsv.contains("Sheet1\tC7\t4\t=A1*2\t否\t\t工作表坐标已改变"), "{tsv}");
    assert!(tsv.contains("B8:C8"), "{tsv}");

    crate::clipboard::clear_test_clipboard();
    assert!(guest.copy_cancelled_offline_drafts_to_clipboard());
    assert_eq!(crate::clipboard::read_system_clip().text, tsv);

    // 4) Switching / opening a new document tab clears cancelled_offline_drafts.
    guest.open_in_new_tab(std::path::PathBuf::new());
    assert!(guest.cancelled_offline_drafts.is_empty());
    assert_eq!(guest.cancelled_offline_drafts_dropped, 0);
    assert!(guest.export_cancelled_offline_drafts_tsv().is_empty());
}

fn tick_ui_frame(app: &mut SheetApp) {
    app.poll_net();
    app.poll_open();
    app.poll_reorder();
    app.sync_history();
    app.sync_coord_epoch_side_effects();
}

fn until_ui_frame(
    h: &mut SheetApp,
    g: &mut SheetApp,
    mut done: impl FnMut(&SheetApp, &SheetApp) -> bool,
) {
    let start = std::time::Instant::now();
    while !done(h, g) {
        tick_ui_frame(h);
        tick_ui_frame(g);
        assert!(start.elapsed().as_secs() < 10, "H={} G={}", h.status, g.status);
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

fn seed_phase7_r1_baseline(wb: &mut sheetcore::Workbook) {
    use sheetcore::{model::CellRef, table::Table, Cell, Value};
    let sh = wb.sheet_mut(0);
    sh.set(
        0,
        0,
        Cell {
            value: Value::Text("Header".into()),
            formula: None,
        },
    );
    sh.set(
        0,
        1,
        Cell {
            value: Value::Text("Amount".into()),
            formula: None,
        },
    );
    sh.set(
        1,
        0,
        Cell {
            value: Value::Text("Item1".into()),
            formula: None,
        },
    );
    sh.set(
        1,
        1,
        Cell {
            value: Value::Number(100.0),
            formula: None,
        },
    );
    sh.set(
        0,
        3,
        Cell {
            value: Value::Text("Header".into()),
            formula: Some("A1:B2".into()),
        },
    );
    sh.set(
        0,
        4,
        Cell {
            value: Value::Text("Amount".into()),
            formula: None,
        },
    );
    sh.set(
        1,
        3,
        Cell {
            value: Value::Text("Item1".into()),
            formula: None,
        },
    );
    sh.set(
        1,
        4,
        Cell {
            value: Value::Number(100.0),
            formula: None,
        },
    );
    sh.arrays.push((CellRef { row: 0, col: 3 }, (0, 3, 1, 4)));
    sh.spilled.insert(CellRef { row: 0, col: 4 });
    sh.spilled.insert(CellRef { row: 1, col: 3 });
    sh.spilled.insert(CellRef { row: 1, col: 4 });
    sh.spill_shapes.insert(CellRef { row: 0, col: 3 }, (2, 2));
    sh.rows.set(0, 60.0);
    sh.tables.push(Table {
        name: "Table1".into(),
        display: "Table1".into(),
        range: (0, 0, 1, 1),
        header_row: true,
        totals_row: false,
        style_name: "TableStyleMedium2".into(),
        columns: vec!["Header".into(), "Amount".into()],
        column_ids: vec![1, 2],
        ..Default::default()
    });
}

fn spawn_silent_listener() -> (u16, std::thread::JoinHandle<()>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = std::thread::spawn(move || {
        if let Ok((stream, _)) = listener.accept() {
            std::thread::sleep(std::time::Duration::from_millis(400));
            drop(stream);
        }
    });
    (port, handle)
}

fn ops_from_workbook(wb: &sheetcore::Workbook, site: u32) -> Vec<sheetcore::sync::Op> {
    let mut ops = Vec::new();
    let mut ts = 1u64;
    for (si, sh) in wb.sheets.iter().enumerate() {
        for (k, cell) in sh.iter_views() {
            let val = cell.value();
            ops.push(sheetcore::sync::Op {
                site,
                sheet: si,
                row: k.row,
                col: k.col,
                ts,
                value: val.as_ref().into(),
                formula: cell.formula().map(std::sync::Arc::from),
            });
            ts += 1;
        }
    }
    ops
}

fn array_rect_at(sh: &sheetcore::Sheet, row: u32, col: u32) -> Option<(u32, u32, u32, u32)> {
    sh.arrays
        .iter()
        .find(|(k, _)| k.row == row && k.col == col)
        .map(|(_, rect)| *rect)
}

#[test]
fn phase7_r1_same_batch_full_snapshot_with_structural_history_and_v1_overlay_no_double_shift() {
    use sheetcore::{
        cell_shift_patch::CellShiftOverlay,
        layout::DEFAULT_ROW_H,
        structural::Structural,
        sync::Peer,
        Value,
    };
    use std::sync::Arc;

    let env = crate::test_env::TestEnv::new("p7-r1-same-batch");

    // --- Part 1: Deterministic single-batch event drain + real UI frame sequence ---
    let mut host_wb = sheetcore::Workbook::new_blank();
    seed_phase7_r1_baseline(&mut host_wb);
    let op = Structural::InsertRows { at: 0, n: 1 };
    sheetcore::structural::apply_structural_wb(&mut host_wb, 0, &op);
    sheetcore::formula::recalc_all(&mut host_wb);
    assert_eq!(host_wb.sheet(0).tables[0].range, (1, 0, 2, 1));
    assert!((host_wb.sheet(0).rows.size(1) - 60.0).abs() < 1e-4);
    assert_eq!(array_rect_at(host_wb.sheet(0), 1, 3), Some((1, 3, 2, 4)));

    let v1_overlay = Arc::new(CellShiftOverlay::capture(&host_wb));
    assert!(v1_overlay.is_full_state());
    assert_eq!(
        v1_overlay.format_version(),
        sheetcore::cell_shift_patch::CURRENT_OVERLAY_FORMAT_VERSION
    );

    let (port, _srv) = spawn_silent_listener();
    let mut guest = SheetApp::new_with_context(&eframe::egui::Context::default(), None);
    guest.wb.path = Some(env.path("r1_same_batch_guest.xlsx"));
    seed_phase7_r1_baseline(&mut guest.wb);
    guest.net = Some(
        crate::net::join(
            2,
            "Guest".into(),
            "doc".into(),
            &format!("127.0.0.1:{port}"),
            "pw".into(),
        )
        .unwrap(),
    );
    guest.peer = Some(Peer::new(2));
    guest.undo.set_site(2);

    // Enqueue Welcome + StructuralHistory + Ops + v1 TxOverlay + SnapshotDone in ONE poll_net batch.
    {
        let net = guest.net.as_ref().unwrap();
        net.inject_event_for_test(crate::net::NetEvent::Welcome {
            peers: vec![(1, "Host".into(), 1)],
            delta_sync: false,
            clock: 10,
        });
        net.inject_event_for_test(crate::net::NetEvent::StructuralHistory(vec![(0, op.clone())]));
        net.inject_event_for_test(crate::net::NetEvent::Ops(ops_from_workbook(&host_wb, 1)));
        net.inject_event_for_test(crate::net::NetEvent::CellShiftOverlay {
            overlay: v1_overlay.clone(),
            seq: 0,
        });
        net.inject_event_for_test(crate::net::NetEvent::SnapshotDone);
    }

    // Run the EXACT real UI frame order: poll_net -> poll_open -> poll_reorder -> sync_history.
    tick_ui_frame(&mut guest);
    assert!(guest.joined_doc && !guest.in_snapshot);
    assert_eq!(guest.wb.sheet(0).tables[0].range, (1, 0, 2, 1), "Table1 must stay at A2:B3, not double-shift to A3:B4");
    assert!((guest.wb.sheet(0).rows.size(1) - 60.0).abs() < 1e-4, "row 1 height must stay 60");
    assert!((guest.wb.sheet(0).rows.size(0) - DEFAULT_ROW_H).abs() < 1e-4);
    assert!((guest.wb.sheet(0).rows.size(2) - DEFAULT_ROW_H).abs() < 1e-4, "row 2 height must NOT receive double-shifted 60");
    assert_eq!(guest.wb.sheet(0).value(1, 0), Value::Text("Header".into()));
    assert_eq!(guest.wb.sheet(0).value(2, 1), Value::Number(100.0));
    assert_eq!(array_rect_at(guest.wb.sheet(0), 1, 3), Some((1, 3, 2, 4)));
    assert!(guest.wb.sheet(0).spilled.contains(&sheetcore::model::CellRef { row: 2, col: 4 }));
    assert_eq!(guest.history_planted, guest.struct_history.len());
    assert_eq!(guest.history_last_epoch, guest.wb_epoch);

    // Additional UI frames must remain completely idempotent.
    for _ in 0..3 {
        tick_ui_frame(&mut guest);
    }
    assert_eq!(guest.wb.sheet(0).tables[0].range, (1, 0, 2, 1));
    assert!((guest.wb.sheet(0).rows.size(1) - 60.0).abs() < 1e-4);

    // --- Part 2: Real TCP host + unsaved InsertRows + late join under real UI frame ticks ---
    let (mut host, guest_seed) = sort_apps_with_baseline(&env, seed_phase7_r1_baseline);
    host.structural(Structural::InsertRows { at: 0, n: 1 });
    assert_eq!(host.wb.sheet(0).tables[0].range, (1, 0, 2, 1));
    assert!((host.wb.sheet(0).rows.size(1) - 60.0).abs() < 1e-4);

    let mut late = late_join(&env, &guest_seed, seed_phase7_r1_baseline);
    // Let host serve CaptureCellShiftOverlay and enqueue all intro frames before late's UI frame.
    let wait_start = std::time::Instant::now();
    while !late.net.as_ref().unwrap().is_live() {
        host.poll_net();
        assert!(wait_start.elapsed().as_secs() < 5);
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    for _ in 0..15 {
        host.poll_net();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    until_ui_frame(&mut host, &mut late, |_, l| l.joined_doc && !l.in_snapshot);
    for _ in 0..3 {
        tick_ui_frame(&mut late);
    }
    assert!(!late.snapshot_is_delta);
    assert_eq!(late.wb.sheet(0).tables[0].range, (1, 0, 2, 1));
    assert!((late.wb.sheet(0).rows.size(1) - 60.0).abs() < 1e-4);
    assert!((late.wb.sheet(0).rows.size(2) - DEFAULT_ROW_H).abs() < 1e-4);
    assert_eq!(late.wb.sheet(0).arrays, host.wb.sheet(0).arrays);
    assert_eq!(late.wb.sheet(0).spilled, host.wb.sheet(0).spilled);
    assert_eq!(late.wb.sheet(0).value(2, 1), Value::Number(100.0));
}

#[test]
fn phase7_r1_cross_frame_structural_history_and_legacy_v0_overlay_ordering() {
    use sheetcore::{
        cell_shift_patch::CellShiftOverlay,
        layout::DEFAULT_ROW_H,
        structural::Structural,
        sync::Peer,
        Value,
    };
    use std::sync::Arc;

    let env = crate::test_env::TestEnv::new("p7-r1-cross-frame");
    let mut host_wb = sheetcore::Workbook::new_blank();
    seed_phase7_r1_baseline(&mut host_wb);
    let op = Structural::InsertRows { at: 0, n: 1 };
    sheetcore::structural::apply_structural_wb(&mut host_wb, 0, &op);
    sheetcore::formula::recalc_all(&mut host_wb);

    let v1_overlay = Arc::new(CellShiftOverlay::capture(&host_wb));
    let legacy_v0_overlay: Arc<CellShiftOverlay> = {
        let mut val = serde_json::to_value(&*v1_overlay).unwrap();
        let obj = val.as_object_mut().unwrap();
        obj.remove("format_version");
        if let Some(sheets) = obj.get_mut("sheets").and_then(|s| s.as_array_mut()) {
            for sh in sheets {
                if let Some(so) = sh.as_object_mut() {
                    so.remove("tables");
                    so.remove("layout");
                    so.remove("arrays");
                    so.remove("spilled");
                    so.remove("spill_shapes");
                    so.remove("filter");
                }
            }
        }
        let ov: CellShiftOverlay = serde_json::from_value(val).unwrap();
        assert_eq!(ov.format_version(), 0);
        assert!(!ov.is_full_state());
        assert!(ov.wire_valid());
        Arc::new(ov)
    };

    // Test all three snapshot modes across frames:
    // 1) v1 full overlay, 2) legacy v0 partial overlay, 3) no overlay at all.
    let modes: [(&str, Option<Arc<CellShiftOverlay>>); 3] = [
        ("v1-full", Some(v1_overlay.clone())),
        ("legacy-v0", Some(legacy_v0_overlay)),
        ("no-overlay", None),
    ];

    for (label, maybe_ov) in modes {
        let (port, _srv) = spawn_silent_listener();
        let mut guest = SheetApp::new_with_context(&eframe::egui::Context::default(), None);
        guest.wb.path = Some(env.path(&format!("r1_cross_{label}.xlsx")));
        seed_phase7_r1_baseline(&mut guest.wb);
        guest.net = Some(
            crate::net::join(
                2,
                "Guest".into(),
                "doc".into(),
                &format!("127.0.0.1:{port}"),
                "pw".into(),
            )
            .unwrap(),
        );
        guest.peer = Some(Peer::new(2));
        guest.undo.set_site(2);

        // Frame 1: Welcome + StructuralHistory arrive BEFORE SnapshotDone.
        {
            let net = guest.net.as_ref().unwrap();
            net.inject_event_for_test(crate::net::NetEvent::Welcome {
                peers: vec![(1, "Host".into(), 1)],
                delta_sync: false,
                clock: 10,
            });
            net.inject_event_for_test(crate::net::NetEvent::StructuralHistory(vec![(
                0,
                op.clone(),
            )]));
        }
        tick_ui_frame(&mut guest);
        assert!(guest.in_snapshot, "{label}: still in snapshot on frame 1");
        assert_eq!(
            guest.wb.sheet(0).tables[0].range,
            (0, 0, 1, 1),
            "{label}: sync_history must be guarded while in_snapshot"
        );
        assert!(
            (guest.wb.sheet(0).rows.size(0) - 60.0).abs() < 1e-4,
            "{label}: row height must not shift prematurely in frame 1"
        );
        assert_eq!(guest.history_planted, 0, "{label}: history must stay unplanted until SnapshotDone");

        // Frame 2: Ops + optional overlay + SnapshotDone arrive.
        {
            let net = guest.net.as_ref().unwrap();
            net.inject_event_for_test(crate::net::NetEvent::Ops(ops_from_workbook(&host_wb, 1)));
            if let Some(ov) = maybe_ov {
                net.inject_event_for_test(crate::net::NetEvent::CellShiftOverlay {
                    overlay: ov,
                    seq: 0,
                });
            }
            net.inject_event_for_test(crate::net::NetEvent::SnapshotDone);
        }
        tick_ui_frame(&mut guest);
        assert!(guest.joined_doc && !guest.in_snapshot, "{label}: snapshot settled");
        assert_eq!(
            guest.wb.sheet(0).tables[0].range,
            (1, 0, 2, 1),
            "{label}: Table1 must shift exactly once to A2:B3"
        );
        assert!(
            (guest.wb.sheet(0).rows.size(1) - 60.0).abs() < 1e-4,
            "{label}: row 1 height must be 60"
        );
        assert!(
            (guest.wb.sheet(0).rows.size(2) - DEFAULT_ROW_H).abs() < 1e-4,
            "{label}: row 2 must NOT double-shift"
        );
        assert_eq!(
            guest.wb.sheet(0).value(2, 1),
            Value::Number(100.0),
            "{label}: authoritative snapshot cells must be intact"
        );
        assert_eq!(
            array_rect_at(guest.wb.sheet(0), 1, 3),
            Some((1, 3, 2, 4)),
            "{label}: array range must shift exactly once"
        );
        assert_eq!(guest.history_planted, guest.struct_history.len());

        // Frame 3: subsequent UI ticks must never shift again.
        for _ in 0..3 {
            tick_ui_frame(&mut guest);
        }
        assert_eq!(guest.wb.sheet(0).tables[0].range, (1, 0, 2, 1), "{label}: idempotent frame 3");
        assert!((guest.wb.sheet(0).rows.size(1) - 60.0).abs() < 1e-4, "{label}: idempotent row height");
    }
}

#[test]
fn phase7_r1_full_rejoin_after_structural_and_axis_permute_plus_live_structural_op() {
    use sheetcore::structural::Structural;
    let env = crate::test_env::TestEnv::new("p7-r1-rejoin-live");
    let (mut host, mut guest) =
        sort_apps_with_baseline(&env, seed_phase6_a1_rich_metadata_baseline);
    seed_phase6_a1_live_cells(&mut host, &mut guest);

    // 1) Live InsertRows while both peers are online.
    host.structural(Structural::InsertRows { at: 0, n: 1 });
    until_ui_frame(&mut host, &mut guest, |h, g| {
        h.wb.sheet(0).tables[0].range == (1, 0, 2, 1)
            && g.wb.sheet(0).tables[0].range == (1, 0, 2, 1)
    });
    assert_phase6_a1_metadata_matches_host(&host, &guest, "after-first-live-insert");

    // 2) Force full rejoin while host performs another InsertRows + an AxisPermute.
    guest.net.as_ref().unwrap().request_snapshot_resync();
    until_ui_frame(&mut host, &mut guest, |_, g| g.room_lost);

    host.structural(Structural::InsertRows { at: 0, n: 1 });
    assert_eq!(host.wb.sheet(0).tables[0].range, (2, 0, 3, 1));
    assert!((host.wb.sheet(0).rows.size(2) - 60.0).abs() < 1e-4);

    assert!(host.move_axis_headers(true, 2, 2, 6), "{}", host.status);
    until_ui_frame(&mut host, &mut guest, |h, _| {
        h.wb.sheet(0).tables[0].range == (4, 0, 5, 1)
            && !h.net.as_ref().unwrap().pending_structure()
    });
    let permute_seq = host.shift_intro.applied_seq;
    assert!(permute_seq > 0);
    assert!((host.wb.sheet(0).rows.size(4) - 60.0).abs() < 1e-4);

    // Wait for guest's full rejoin to settle under real UI frame order.
    until_ui_frame(&mut host, &mut guest, |_, g| {
        g.joined_doc
            && !g.in_snapshot
            && !g.room_lost
            && g.shift_intro.applied_seq >= permute_seq
    });
    for _ in 0..4 {
        tick_ui_frame(&mut guest);
    }
    assert!(!guest.snapshot_is_delta);
    assert_eq!(guest.wb.sheet(0).tables[0].range, (4, 0, 5, 1));
    assert!((guest.wb.sheet(0).rows.size(4) - 60.0).abs() < 1e-4);
    assert_eq!(guest.history_planted, guest.struct_history.len());
    assert_phase6_a1_metadata_matches_host(&host, &guest, "after-full-rejoin-structural-and-permute");

    // 3) Live Structural op AFTER SnapshotDone must shift ONCE and advance history_planted.
    let planted_before = guest.history_planted;
    host.structural(Structural::InsertRows { at: 0, n: 1 });
    until_ui_frame(&mut host, &mut guest, |h, g| {
        h.wb.sheet(0).tables[0].range == (5, 0, 6, 1)
            && g.wb.sheet(0).tables[0].range == (5, 0, 6, 1)
    });
    for _ in 0..4 {
        tick_ui_frame(&mut guest);
    }
    assert_eq!(guest.wb.sheet(0).tables[0].range, (5, 0, 6, 1));
    assert!((guest.wb.sheet(0).rows.size(5) - 60.0).abs() < 1e-4);
    assert_eq!(guest.history_planted, planted_before + 1);
    assert_eq!(guest.history_planted, guest.struct_history.len());
    assert_phase6_a1_metadata_matches_host(&host, &guest, "after-post-rejoin-live-insert");
}

#[test]
fn phase7_r4_conflict_drafts_clear_style_cross_epoch_and_10001_truncation() {
    use crate::cell_shift::{
        CancelledDraftValue, CancelledOfflineDraft, DraftArchiveOutcome,
        MAX_CANCELLED_OFFLINE_DRAFTS,
    };
    use crate::net::CoordEpoch;
    use sheetcore::{sync::Op, Value};

    let _env = crate::test_env::TestEnv::new("p7-r4-conflict-drafts");
    let mut app = SheetApp::new_with_context(&eframe::egui::Context::default(), None);
    let ep1 = CoordEpoch {
        struct_rev: 0,
        shift_seq: 1,
    };
    let ep2 = CoordEpoch {
        struct_rev: 1,
        shift_seq: 1,
    };
    let ep3 = CoordEpoch {
        struct_rev: 1,
        shift_seq: 2,
    };

    // 1) Explicit clear overwrites prior drafted value at the same (room, coord_epoch, sheet, row, col).
    let op_write = Op {
        site: 2,
        sheet: 0,
        row: 0,
        col: 0,
        ts: 10,
        value: (&Value::Text("old edit".into())).into(),
        formula: None,
    };
    assert_eq!(
        app.archive_cancelled_offline_op("room-1".into(), ep1, &op_write, "工作表坐标已改变".into()),
        DraftArchiveOutcome::Stored
    );
    let op_clear = Op {
        site: 2,
        sheet: 0,
        row: 0,
        col: 0,
        ts: 11,
        value: (&Value::Empty).into(),
        formula: None,
    };
    assert_eq!(
        app.archive_cancelled_offline_op("room-1".into(), ep1, &op_clear, "工作表坐标已改变".into()),
        DraftArchiveOutcome::Updated
    );
    assert_eq!(app.cancelled_offline_drafts.len(), 1);
    assert_eq!(
        app.cancelled_offline_drafts[0].value,
        CancelledDraftValue::Cleared
    );

    // 2) Style-only (`Unchanged`) preserves prior `Cleared` and prior `Value`.
    assert_eq!(
        app.push_cancelled_offline_draft(CancelledOfflineDraft {
            room_id: "room-1".into(),
            coord_epoch: ep1,
            sheet: 0,
            sheet_name: "Sheet1".into(),
            row: 0,
            col: 0,
            value: CancelledDraftValue::Unchanged,
            style_changed: true,
            merge_rect: None,
            merge_on: None,
            reason: "工作表坐标已改变".into(),
        }),
        DraftArchiveOutcome::Updated
    );
    assert_eq!(
        app.cancelled_offline_drafts[0].value,
        CancelledDraftValue::Cleared
    );
    assert!(app.cancelled_offline_drafts[0].style_changed);

    assert_eq!(
        app.push_cancelled_offline_draft(CancelledOfflineDraft {
            room_id: "room-1".into(),
            coord_epoch: ep1,
            sheet: 0,
            sheet_name: "Sheet1".into(),
            row: 1,
            col: 0,
            value: CancelledDraftValue::Value {
                display: "42".into(),
                formula: Some("=21*2".into()),
            },
            style_changed: false,
            merge_rect: None,
            merge_on: None,
            reason: "工作表坐标已改变".into(),
        }),
        DraftArchiveOutcome::Stored
    );
    assert_eq!(
        app.push_cancelled_offline_draft(CancelledOfflineDraft {
            room_id: "room-1".into(),
            coord_epoch: ep1,
            sheet: 0,
            sheet_name: "Sheet1".into(),
            row: 1,
            col: 0,
            value: CancelledDraftValue::Unchanged,
            style_changed: true,
            merge_rect: Some((1, 0, 1, 1)),
            merge_on: Some(true),
            reason: "工作表坐标已改变".into(),
        }),
        DraftArchiveOutcome::Updated
    );
    assert_eq!(
        app.cancelled_offline_drafts[1].value,
        CancelledDraftValue::Value {
            display: "42".into(),
            formula: Some("=21*2".into()),
        }
    );
    assert!(app.cancelled_offline_drafts[1].style_changed);
    assert_eq!(
        app.cancelled_offline_drafts[1].merge_rect,
        Some((1, 0, 1, 1))
    );

    // Pure style-only draft displays as `[仅样式/合并]`.
    assert_eq!(
        app.push_cancelled_offline_draft(CancelledOfflineDraft {
            room_id: "room-1".into(),
            coord_epoch: ep1,
            sheet: 0,
            sheet_name: "Sheet1".into(),
            row: 2,
            col: 0,
            value: CancelledDraftValue::Unchanged,
            style_changed: true,
            merge_rect: None,
            merge_on: None,
            reason: "工作表坐标已改变".into(),
        }),
        DraftArchiveOutcome::Stored
    );

    // 3) Cross-epoch separation at the same coordinate A1 (`(0, 0)`).
    assert_eq!(
        app.push_cancelled_offline_draft(CancelledOfflineDraft {
            room_id: "room-1".into(),
            coord_epoch: ep2,
            sheet: 0,
            sheet_name: "Sheet1".into(),
            row: 0,
            col: 0,
            value: CancelledDraftValue::Value {
                display: "epoch2-edit".into(),
                formula: None,
            },
            style_changed: false,
            merge_rect: None,
            merge_on: None,
            reason: "工作表坐标已改变".into(),
        }),
        DraftArchiveOutcome::Stored
    );
    assert_eq!(
        app.push_cancelled_offline_draft(CancelledOfflineDraft {
            room_id: "room-1".into(),
            coord_epoch: ep3,
            sheet: 0,
            sheet_name: "Sheet1".into(),
            row: 0,
            col: 0,
            value: CancelledDraftValue::Cleared,
            style_changed: false,
            merge_rect: None,
            merge_on: None,
            reason: "工作表坐标已改变".into(),
        }),
        DraftArchiveOutcome::Stored
    );
    assert_eq!(app.cancelled_offline_drafts.len(), 5);
    assert_eq!(app.cancelled_offline_drafts_dropped, 0);

    let tsv = app.export_cancelled_offline_drafts_tsv();
    assert!(tsv.contains("坐标纪元"), "TSV header must include coord_epoch column: {tsv}");
    assert!(tsv.contains("s0/m1\tSheet1\tA1\t[清空]\t\t是"), "{tsv}");
    assert!(tsv.contains("s0/m1\tSheet1\tA2\t42\t=21*2\t是\t合并 A2:B2"), "{tsv}");
    assert!(tsv.contains("s0/m1\tSheet1\tA3\t[仅样式/合并]\t\t是"), "{tsv}");
    assert!(tsv.contains("s1/m1\tSheet1\tA1\tepoch2-edit"), "{tsv}");
    assert!(tsv.contains("s1/m2\tSheet1\tA1\t[清空]"), "{tsv}");
    assert!(!tsv.contains("old edit"), "overwritten value must not remain after explicit clear");

    // 4) 10,001 distinct drafts: 10,000 retained + 1 dropped/truncated surfaced in UI & TSV.
    app.clear_cancelled_offline_drafts();
    assert!(app.cancelled_offline_drafts.is_empty());
    assert_eq!(app.cancelled_offline_drafts_dropped, 0);

    for i in 0..(MAX_CANCELLED_OFFLINE_DRAFTS + 1) {
        let outcome = app.push_cancelled_offline_draft(CancelledOfflineDraft {
            room_id: "room-cap".into(),
            coord_epoch: ep1,
            sheet: 0,
            sheet_name: "Sheet1".into(),
            row: (i / 100) as u32,
            col: (i % 100) as u32,
            value: CancelledDraftValue::Value {
                display: format!("v{i}"),
                formula: None,
            },
            style_changed: false,
            merge_rect: None,
            merge_on: None,
            reason: "工作表坐标已改变".into(),
        });
        if i < MAX_CANCELLED_OFFLINE_DRAFTS {
            assert_eq!(outcome, DraftArchiveOutcome::Stored);
        } else {
            assert_eq!(outcome, DraftArchiveOutcome::Truncated);
        }
    }
    assert_eq!(app.cancelled_offline_drafts.len(), MAX_CANCELLED_OFFLINE_DRAFTS);
    assert_eq!(app.cancelled_offline_drafts_dropped, 1);

    // Updating an already-retained coordinate while at capacity succeeds without incrementing dropped.
    assert_eq!(
        app.push_cancelled_offline_draft(CancelledOfflineDraft {
            room_id: "room-cap".into(),
            coord_epoch: ep1,
            sheet: 0,
            sheet_name: "Sheet1".into(),
            row: 0,
            col: 0,
            value: CancelledDraftValue::Cleared,
            style_changed: false,
            merge_rect: None,
            merge_on: None,
            reason: "工作表坐标已改变".into(),
        }),
        DraftArchiveOutcome::Updated
    );
    assert_eq!(app.cancelled_offline_drafts.len(), MAX_CANCELLED_OFFLINE_DRAFTS);
    assert_eq!(app.cancelled_offline_drafts_dropped, 1);
    assert_eq!(
        app.cancelled_offline_drafts[0].value,
        CancelledDraftValue::Cleared
    );

    let btn_label = app.conflict_drafts_button_label();
    assert!(
        btn_label.contains("10000") && btn_label.contains("已丢弃1"),
        "unexpected button label: {btn_label}"
    );
    let status_msg = app.format_cancelled_edits_status(MAX_CANCELLED_OFFLINE_DRAFTS + 1);
    assert!(
        status_msg.contains("保留 10000") && status_msg.contains("已丢弃 1"),
        "unexpected status message: {status_msg}"
    );
    let truncated_tsv = app.export_cancelled_offline_drafts_tsv();
    assert!(
        truncated_tsv.contains("已保留 10000") && truncated_tsv.contains("已丢弃 1"),
        "TSV must surface truncation summary"
    );

    crate::clipboard::clear_test_clipboard();
    assert!(app.copy_cancelled_offline_drafts_to_clipboard());
    assert!(
        app.status.contains("10000") && app.status.contains("1") && app.status.contains("丢弃"),
        "clipboard status must surface dropped count: {}",
        app.status
    );

    app.clear_cancelled_offline_drafts();
    assert!(app.cancelled_offline_drafts.is_empty());
    assert_eq!(app.cancelled_offline_drafts_dropped, 0);
}

#[test]
fn phase7_f2_new_guest_against_legacy_host_gates_axis_permute_and_handles_v0_snapshot() {
    use serde_json::json;
    use sheetcore::{
        cell_shift_patch::CellShiftOverlay,
        layout::DEFAULT_ROW_H,
        structural::Structural,
        sync::{Msg, Peer},
        Value,
    };
    use std::io::{BufRead, BufReader, Write};

    let env = crate::test_env::TestEnv::new("p7-f2-legacy-host");
    let mut host_wb = sheetcore::Workbook::new_blank();
    seed_phase7_r1_baseline(&mut host_wb);
    let op = Structural::InsertRows { at: 0, n: 1 };
    sheetcore::structural::apply_structural_wb(&mut host_wb, 0, &op);
    sheetcore::formula::recalc_all(&mut host_wb);

    // Add a legacy-covered metadata change (a note at A2) to prove the v0 overlay is actually applied.
    host_wb.sheet_mut(0).notes.insert(
        sheetcore::model::CellRef { row: 1, col: 0 },
        sheetcore::notes::Note {
            r: 1,
            c: 0,
            text: "legacy-v0-note".into(),
            author: "LegacyHost".into(),
        },
    );

    // Strip format_version + v1-only metadata to simulate a 1c84f4f host payload.
    let v0_bytes = {
        let mut val = serde_json::to_value(CellShiftOverlay::capture(&host_wb)).unwrap();
        let obj = val.as_object_mut().unwrap();
        obj.remove("format_version");
        if let Some(sheets) = obj.get_mut("sheets").and_then(|s| s.as_array_mut()) {
            for sh in sheets {
                if let Some(so) = sh.as_object_mut() {
                    so.remove("tables");
                    so.remove("layout");
                    so.remove("arrays");
                    so.remove("spilled");
                    so.remove("spill_shapes");
                    so.remove("filter");
                }
            }
        }
        serde_json::to_vec(&val).unwrap()
    };
    let snap_ops = ops_from_workbook(&host_wb, 1);

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let salt = [7u8; 16];
        let room_salt = [9u8; 16];
        let room_key = sheetcore::wire::RoomKey::derive("pw", &room_salt);
        let nonce = 4242u64;
        let challenge = Msg::ChallengeSalt {
            nonce,
            salt: sheetcore::wire::b64_encode(&salt),
            room_salt: sheetcore::wire::b64_encode(&room_salt),
        };
        let mut line = serde_json::to_string(&challenge).unwrap();
        line.push('\n');
        stream.write_all(line.as_bytes()).unwrap();
        stream.flush().unwrap();

        let mut sess = sheetcore::wire::Session::host_side(&room_key, &salt);
        let mut hello_line = String::new();
        reader.read_line(&mut hello_line).unwrap();
        let hello_plain = sess.open(hello_line.trim()).expect("open Hello");
        let hello_val: serde_json::Value = serde_json::from_str(&hello_plain).unwrap();
        assert_eq!(hello_val["t"], "Hello");
        assert_eq!(hello_val["full_tx_overlay"], json!(true));

        // Legacy 1c84f4f Welcome: room_tx=true, cell_shifts=true, NO full_tx_overlay field.
        let mut send_json = |v: serde_json::Value| {
            let mut f = sess.seal(&v.to_string());
            f.push('\n');
            stream.write_all(f.as_bytes()).unwrap();
            stream.flush().unwrap();
        };
        send_json(json!({
            "t": "Welcome",
            "site": 2,
            "doc": "doc",
            "peers": [[1, "LegacyHost", 1]],
            "delta_sync": false,
            "clock": 10,
            "batch_ops": true,
            "cell_shifts_v2": true,
            "room_tx": true,
            "sort_permutation": true,
            "structural_ack": true,
            "thread_changes": true,
            "palette_styles": true,
            "room_id": "legacy-room",
            "watermark": 1,
            "structure_revision": 1,
            "binary_v3": false,
            "compact_v3": false,
            "acked_op_ts": 0
        }));
        send_json(serde_json::to_value(Msg::StructuralLog { items: vec![(0, op)] }).unwrap());
        send_json(
            serde_json::to_value(Msg::SnapshotChunk {
                ops: snap_ops,
                last: false,
            })
            .unwrap(),
        );
        send_json(
            serde_json::to_value(Msg::CellShiftOverlayPart {
                seq: 0,
                index: 0,
                last: true,
                data: hex::encode(&v0_bytes),
            })
            .unwrap(),
        );
        send_json(serde_json::to_value(Msg::Snapshot { ops: Vec::new() }).unwrap());
        // Keep the connection open until guest shuts down so guest stays live for the AxisPermute gate check.
        let mut eof = String::new();
        let _ = reader.read_line(&mut eof);
    });

    let mut guest = SheetApp::new_with_context(&eframe::egui::Context::default(), None);
    guest.wb.path = Some(env.path("f2_new_guest.xlsx"));
    seed_phase7_r1_baseline(&mut guest.wb);
    guest.net = Some(
        crate::net::join(
            2,
            "NewGuest".into(),
            "doc".into(),
            &format!("127.0.0.1:{port}"),
            "pw".into(),
        )
        .unwrap(),
    );
    guest.peer = Some(Peer::new(2));
    guest.undo.set_site(2);

    let start = std::time::Instant::now();
    while !(guest.joined_doc && !guest.in_snapshot) {
        tick_ui_frame(&mut guest);
        assert!(start.elapsed().as_secs() < 5, "status={}", guest.status);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    for _ in 0..3 {
        tick_ui_frame(&mut guest);
    }

    // 1) Legacy v0 snapshot + StructuralLog settled cleanly ONCE (no double shift),
    //    and legacy-covered metadata (note from v0 overlay) was applied.
    assert!(guest.net.as_ref().unwrap().supports_room_tx());
    assert!(!guest.net.as_ref().unwrap().supports_full_tx_overlay());
    assert_eq!(guest.wb.sheet(0).tables[0].range, (1, 0, 2, 1));
    assert!((guest.wb.sheet(0).rows.size(1) - 60.0).abs() < 1e-4);
    assert!((guest.wb.sheet(0).rows.size(2) - DEFAULT_ROW_H).abs() < 1e-4);
    assert_eq!(guest.wb.sheet(0).value(2, 1), Value::Number(100.0));
    assert_eq!(
        guest
            .wb
            .sheet(0)
            .notes
            .get(&sheetcore::model::CellRef { row: 1, col: 0 })
            .map(|n| n.text.as_str()),
        Some("legacy-v0-note"),
        "legacy v0 overlay metadata must actually be installed"
    );

    // 2) Local AxisPermute initiation against the legacy host must be blocked with upgrade notice.
    let moved = guest.move_axis_headers(true, 1, 2, 5);
    assert!(!moved, "AxisPermute must be gated against legacy full_tx_overlay=false host");
    assert!(
        guest.status.contains("升级") && guest.status.contains("完整元数据快照"),
        "unexpected gate status: {}",
        guest.status
    );
    assert!(
        !guest.net.as_ref().unwrap().pending_structure(),
        "blocked AxisPermute must not enter pending_structure"
    );
    assert_eq!(guest.wb.sheet(0).tables[0].range, (1, 0, 2, 1));

    guest.net.take().unwrap().shutdown();
    let _ = server.join();
}

#[test]
fn phase7_r3_app_ai_rich_only_cell_batch_remote_edit_leave_session_undo_skips_cleanly() {
    use sheetcore::{richtext::RichRun, Cell, Value};

    let env = crate::test_env::TestEnv::new("p7-r3-ai-rich-only-leave");
    let ctx = eframe::egui::Context::default();

    let make_runs = || {
        let mut bold = RichRun::plain("total");
        bold.bold = true;
        vec![RichRun::plain("Revenue "), bold]
    };

    // Test all 4 peer replacement types from peer-review R3: text, number, formula, blank.
    let replacements: [(&str, Cell); 4] = [
        (
            "text",
            Cell {
                value: Value::Text("Peer replacement".into()),
                formula: None,
            },
        ),
        (
            "number",
            Cell {
                value: Value::Number(123.0),
                formula: None,
            },
        ),
        (
            "formula",
            Cell {
                value: Value::Number(4.0),
                formula: Some("2+2".into()),
            },
        ),
        (
            "blank",
            Cell {
                value: Value::Empty,
                formula: None,
            },
        ),
    ];

    for (label, peer_cell) in replacements {
        let (mut host, mut guest) = sort_apps(&env);
        let a1_runs = make_runs();
        let initial = Cell {
            value: Value::Text("Revenue total".into()),
            formula: None,
        };
        for app in [&mut host, &mut guest] {
            let sh = app.wb.sheet_mut(0);
            sh.set(0, 0, initial.clone());
            sh.rich.insert((0, 0), a1_runs.clone());
            if let Some(peer) = app.peer.as_mut() {
                peer.seed_cell(0, 0, 0, &initial);
            }
        }
        guest.ai.bound_doc_uid = Some(guest.doc_uid);
        guest.ai.messages.push(crate::ai_agent::AiMessage {
            role: crate::ai_agent::AiRole::Assistant,
            content: String::new(),
            reasoning: String::new(),
            reasoning_open: false,
            tool_logs: Vec::new(),
            error: None,
            undo_ids: Vec::new(),
        });

        // 1) AI set_cells duplicate-coordinate write strips RichRuns with zero net input/style diff.
        let tc = crate::ai_agent::ToolCallSpec {
            name: "set_cells".into(),
            arguments: serde_json::json!({
                "edits": [
                    {"cell": "A1", "value": 99},
                    {"cell": "A1", "value": "Revenue total"}
                ]
            }),
        };
        let _ = guest.execute_ai_tool(&tc, &ctx);
        assert_eq!(
            guest.wb.sheet(0).value(0, 0),
            Value::Text("Revenue total".into()),
            "{label}: duplicate AI write returns plain text"
        );
        assert!(
            !guest.wb.sheet(0).rich.contains_key(&(0, 0)),
            "{label}: intermediate numeric write strips RichRuns"
        );
        assert_eq!(guest.undo.depth().0, 1, "{label}: rich-only CellBatch recorded");

        // Let host observe guest's AI write so host's LWW clock advances past guest's op.
        let guest_ts = guest.peer.as_ref().unwrap().clock;
        until(&mut host, &mut guest, |h, _| {
            h.peer.as_ref().unwrap().clock >= guest_ts
        });

        // 2) Remote peer (host) edits A1 to `peer_cell` and broadcasts it to guest.
        host.wb.sheet_mut(0).set(0, 0, peer_cell.clone());
        host.wb.sheet_mut(0).rich.remove(&(0, 0));
        host.broadcast_cells(&[(0, 0, 0)]);
        until(&mut host, &mut guest, |_, g| {
            g.wb.sheet(0)
                .get(0, 0)
                .map_or_else(|| peer_cell.is_blank(), |cur| cur.same_input(&peer_cell))
        });
        assert!(!guest.wb.sheet(0).rich.contains_key(&(0, 0)));
        assert_eq!(guest.undo.depth().0, 1, "{label}: remote edit does not enter local undo stack");

        // 3) Local user executes "断开协作" (`leave_session`) — removes net/locks, keeps local CellBatch.
        guest.leave_session();
        assert!(guest.net.is_none());
        assert_eq!(guest.undo.depth().0, 1);

        // 4) Local user executes `apply_undo(false)` -> must skip cleanly without attaching stale RichRuns!
        let _ = guest.apply_undo(false);
        assert!(
            guest
                .wb
                .sheet(0)
                .get(0, 0)
                .map_or_else(|| peer_cell.is_blank(), |cur| cur.same_input(&peer_cell)),
            "{label}: peer cell input must remain intact"
        );
        assert!(
            !guest.wb.sheet(0).rich.contains_key(&(0, 0)),
            "{label}: stale RichRuns must NOT be attached onto peer's replacement cell"
        );
    }

    // 5) Unconflicted rich-only AI CellBatch after leave_session still roundtrips Undo/Redo cleanly.
    let (_host, mut guest) = sort_apps(&env);
    let a1_runs = make_runs();
    let initial = Cell {
        value: Value::Text("Revenue total".into()),
        formula: None,
    };
    guest.wb.sheet_mut(0).set(0, 0, initial.clone());
    guest.wb.sheet_mut(0).rich.insert((0, 0), a1_runs.clone());
    guest.ai.bound_doc_uid = Some(guest.doc_uid);
    guest.ai.messages.push(crate::ai_agent::AiMessage {
        role: crate::ai_agent::AiRole::Assistant,
        content: String::new(),
        reasoning: String::new(),
        reasoning_open: false,
        tool_logs: Vec::new(),
        error: None,
        undo_ids: Vec::new(),
    });
    let tc = crate::ai_agent::ToolCallSpec {
        name: "set_cells".into(),
        arguments: serde_json::json!({
            "edits": [
                {"cell": "A1", "value": 99},
                {"cell": "A1", "value": "Revenue total"}
            ]
        }),
    };
    let _ = guest.execute_ai_tool(&tc, &ctx);
    assert!(!guest.wb.sheet(0).rich.contains_key(&(0, 0)));
    guest.leave_session();
    assert!(guest.apply_undo(false), "{}", guest.status);
    assert_eq!(guest.wb.sheet(0).rich.get(&(0, 0)), Some(&a1_runs));
    assert!(guest.apply_undo(true), "{}", guest.status);
    assert!(!guest.wb.sheet(0).rich.contains_key(&(0, 0)));
}

#[test]
fn phase7_r1_aborted_snapshot_denied_or_disconnected_clears_struct_history() {
    use sheetcore::{
        cell_shift_patch::CellShiftOverlay,
        structural::Structural,
        sync::Peer,
    };
    use std::sync::Arc;

    let env = crate::test_env::TestEnv::new("p7-r1-aborted-snapshot");
    let op = Structural::InsertRows { at: 0, n: 1 };

    for abort_kind in ["denied", "disconnected"] {
        let (port, _srv) = spawn_silent_listener();
        let mut guest = SheetApp::new_with_context(&eframe::egui::Context::default(), None);
        guest.wb.path = Some(env.path(&format!("r1_abort_{abort_kind}.xlsx")));
        seed_phase7_r1_baseline(&mut guest.wb);
        let dummy_overlay = Arc::new(CellShiftOverlay::capture(&guest.wb));
        guest.net = Some(
            crate::net::join(
                2,
                "Guest".into(),
                "doc".into(),
                &format!("127.0.0.1:{port}"),
                "pw".into(),
            )
            .unwrap(),
        );
        guest.peer = Some(Peer::new(2));
        guest.undo.set_site(2);

        // Enqueue Welcome -> StructuralHistory -> CellShiftOverlay -> abort (Denied or Disconnected)
        // WITHOUT SnapshotDone, then run real UI frame order.
        {
            let net = guest.net.as_ref().unwrap();
            net.inject_event_for_test(crate::net::NetEvent::Welcome {
                peers: vec![(1, "Host".into(), 1)],
                delta_sync: false,
                clock: 5,
            });
            net.inject_event_for_test(crate::net::NetEvent::StructuralHistory(vec![(
                0,
                op.clone(),
            )]));
            net.inject_event_for_test(crate::net::NetEvent::CellShiftOverlay {
                overlay: dummy_overlay,
                seq: 1,
            });
            if abort_kind == "denied" {
                net.inject_event_for_test(crate::net::NetEvent::Denied("宿主拒绝连接".into()));
            } else {
                net.inject_event_for_test(crate::net::NetEvent::Disconnected("连接中断".into()));
            }
        }

        tick_ui_frame(&mut guest);
        assert!(!guest.in_snapshot, "{abort_kind}: in_snapshot must clear");
        assert!(!guest.joined_doc, "{abort_kind}: joined_doc must stay false");
        assert!(
            guest.struct_history.is_empty(),
            "{abort_kind}: aborted snapshot must clear struct_history"
        );
        assert_eq!(guest.history_planted, 0);
        assert!(guest.shift_intro.overlay.is_none());
        assert_eq!(guest.shift_intro.baseline_struct_rev, 0);
        assert_eq!(
            guest.wb.sheet(0).tables[0].range,
            (0, 0, 1, 1),
            "{abort_kind}: aborted room StructuralHistory must NOT shift local Table1"
        );
        assert!(
            (guest.wb.sheet(0).rows.size(0) - 60.0).abs() < 1e-4,
            "{abort_kind}: aborted room StructuralHistory must NOT shift local row height"
        );

        // Additional UI frames must also leave local metadata untouched.
        for _ in 0..3 {
            tick_ui_frame(&mut guest);
        }
        assert_eq!(guest.wb.sheet(0).tables[0].range, (0, 0, 1, 1));
        assert!((guest.wb.sheet(0).rows.size(0) - 60.0).abs() < 1e-4);
    }
}






