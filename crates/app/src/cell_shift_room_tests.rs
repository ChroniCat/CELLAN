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

