use super::*;
use sheetcore::{Cell, Value, Workbook, cells::Shift, structural::CellShift};
use std::time::Instant;

fn wait_event(net: &Net, expected: impl Fn(&NetEvent) -> bool) -> NetEvent {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let event = net
            .events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("room event");
        if expected(&event) {
            return event;
        }
    }
}

fn seed(host: &Net) -> (Workbook, Arc<CellShiftPatch>) {
    let mut wb = Workbook::new_blank();
    let cell = Cell {
        value: Value::Number(7.),
        formula: None,
    };
    wb.sheet_mut(0).set(0, 0, cell.clone());
    let mut peer = sheetcore::sync::Peer::new(host.site);
    peer.seed_cell(0, 0, 0, &cell);
    host.seed_snapshot(
        &peer,
        &wb,
        Cursor {
            site: host.site,
            name: "Host".into(),
            color: 1,
            sheet: 0,
            row: 0,
            col: 0,
            end_row: None,
            end_col: None,
        },
    );
    let patch =
        CellShiftPatch::prepare(&wb, 0, CellShift::new((0, 0, 0, 0), Shift::Vertical, true))
            .unwrap();
    (wb, Arc::new(patch))
}

fn check(host: &Net) -> Sender<Result<Arc<CellShiftPatch>, String>> {
    let NetEvent::CheckCellShift { reply, .. } =
        wait_event(host, |ev| matches!(ev, NetEvent::CheckCellShift { .. }))
    else {
        unreachable!()
    };
    reply
}

fn send_overlay(host: &Net, wb: &Workbook) {
    let NetEvent::CaptureCellShiftOverlay(reply) = wait_event(host, |ev| {
        matches!(ev, NetEvent::CaptureCellShiftOverlay(_))
    }) else {
        unreachable!()
    };
    reply.send(CellShiftOverlay::capture(wb)).unwrap();
}

#[test]
fn partial_cell_shift_room_mixed_version_keeps_edits_and_requires_upgrade_for_new_coordinates() {
    let _env = crate::test_env::TestEnv::new("shift-mixed-room");
    let (host, port) = tests::host_for_test(1, "Host", "doc", "test-pw");
    let (_, patch) = seed(&host);
    let (mut legacy, welcome) = thread_compat_tests::LegacyPeer::handshake(port, 2, "", 0);
    assert_eq!(welcome["t"], "Welcome");
    let op = tests::op(12, 91., 20, 2);
    legacy.send(serde_json::json!({"t":"Op","site":op.site,"ts":op.ts,"row":op.row,"col":op.col,"sheet":op.sheet,"value":op.value,"formula":null}));
    wait_event(
        &host,
        |ev| matches!(ev,NetEvent::Ops(ops) if ops.iter().any(|o|o.row==12)),
    );
    host.request_cell_shift(1, CellShiftPhase::Forward, patch.clone())
        .unwrap();
    wait_event(
        &host,
        |ev| matches!(ev,NetEvent::StructuralRejected(msg) if msg.contains("旧版本")),
    );
    host.finish_structure();
    drop(legacy);
    wait_event(&host, |ev| matches!(ev, NetEvent::PeerLeft(2)));
    host.request_cell_shift(2, CellShiftPhase::Forward, patch.clone())
        .unwrap();
    check(&host).send(Ok(patch)).unwrap();
    wait_event(&host, |ev| {
        matches!(ev, NetEvent::CellShiftCommitted { id: 2, .. })
    });
    let (_, refusal) = thread_compat_tests::LegacyPeer::handshake(port, 2, "", 0);
    assert_eq!(refusal["t"], "Denied");
    assert!(refusal["reason"].as_str().unwrap().contains("升级"));
}

#[test]
fn partial_cell_shift_room_reconnect_recovers_accepted_receipt_and_rejects_missing_undo() {
    let _env = crate::test_env::TestEnv::new("shift-reconnect-receipt");
    let (host, port) = tests::host_for_test(1, "Host", "doc", "test-pw");
    let (mut wb, patch) = seed(&host);
    let guest = join(
        2,
        "Guest".into(),
        "doc".into(),
        &format!("127.0.0.1:{port}"),
        "test-pw".into(),
    )
    .unwrap();
    wait_event(&guest, |e| matches!(e, NetEvent::SnapshotDone));
    guest
        .request_cell_shift(21, CellShiftPhase::Forward, patch.clone())
        .unwrap();
    check(&host).send(Ok(patch.clone())).unwrap();
    let NetEvent::CellShiftCommitted { seq, .. } = wait_event(&host, |ev| {
        matches!(ev, NetEvent::CellShiftCommitted { id: 21, .. })
    }) else {
        unreachable!()
    };
    assert!(patch.apply_committed(&mut wb, true));
    host.ack_cell_shift(seq);
    // Lose the receipt at the UI boundary. Retain the original transaction identity.
    guest.request_snapshot_resync();
    wait_event(&guest, |ev| matches!(ev, NetEvent::Disconnected(_)));
    send_overlay(&host, &wb);
    let receipt = wait_event(&guest, |ev| {
        matches!(
            ev,
            NetEvent::CellShiftCommitted { snapshot: true, .. } | NetEvent::SnapshotDone
        )
    });
    assert!(
        matches!(receipt,NetEvent::CellShiftCommitted {id:21,site:2,phase:CellShiftPhase::Forward,seq:recovered,patch:returned,..} if seq==recovered && returned==patch)
    );
    assert!(matches!(
        wait_event(&guest, |ev| matches!(
            ev,
            NetEvent::CellShiftCommitted { .. } | NetEvent::SnapshotDone
        )),
        NetEvent::SnapshotDone
    ));
    guest.finish_structure();
    guest.ack_cell_shift(seq);
    guest
        .request_cell_shift(21, CellShiftPhase::Undo, patch)
        .unwrap();
    let pending = check(&host);
    assert!(
        lock_unpoison(&guest.shift_pending)
            .as_ref()
            .is_some_and(|(id, phase, _)| *id == 21 && *phase == CellShiftPhase::Undo)
    );
    // Stop both ends of the old socket before releasing the uncommitted host vote.
    host.disconnect_socket_for_test();
    guest.request_snapshot_resync();
    drop(pending);
    let mut refusals = Vec::new();
    loop {
        match wait_event(&guest, |ev| {
            matches!(
                ev,
                NetEvent::Disconnected(_)
                    | NetEvent::StructuralRejected(_)
                    | NetEvent::CellShiftCommitted { .. }
            )
        }) {
            NetEvent::Disconnected(_) => break,
            NetEvent::StructuralRejected(reason) => refusals.push(reason),
            event => panic!("uncommitted undo appeared on the old connection: {event:?}"),
        }
    }
    send_overlay(&host, &wb);
    let mut saw_overlay = false;
    loop {
        match wait_event(&guest, |ev| {
            matches!(
                ev,
                NetEvent::StructuralRejected(_)
                    | NetEvent::CellShiftOverlay { .. }
                    | NetEvent::CellShiftCommitted { .. }
                    | NetEvent::SnapshotDone
            )
        }) {
            NetEvent::StructuralRejected(reason) => refusals.push(reason),
            NetEvent::CellShiftOverlay { seq: sequence, .. } => {
                assert_eq!(sequence, seq);
                saw_overlay = true;
            }
            NetEvent::SnapshotDone => break,
            event => panic!("uncommitted undo appeared in restored history: {event:?}"),
        }
    }
    assert!(saw_overlay);
    assert_eq!(
        refusals.len(),
        1,
        "old and new socket outcomes: {refusals:?}"
    );
    assert!(lock_unpoison(&guest.shift_pending).is_none());
    guest.finish_structure();
    assert!(!guest.pending_structure());
}
#[test]
fn partial_cell_shift_room_reconnect_clears_a_request_that_never_reached_host() {
    let _env = crate::test_env::TestEnv::new("shift-no-wire-receipt");
    let (host, port) = tests::host_for_test(1, "Host", "doc", "test-pw");
    let (_, patch) = seed(&host);
    let guest = join(
        2,
        "Guest".into(),
        "doc".into(),
        &format!("127.0.0.1:{port}"),
        "test-pw".into(),
    )
    .unwrap();
    wait_event(&guest, |e| matches!(e, NetEvent::SnapshotDone));
    // The command queue was lost with the connection immediately after local registration.
    // The host never saw this id and therefore has no operation to replay.
    *lock_unpoison(&guest.shift_pending) = Some((93, CellShiftPhase::Forward, patch));
    guest.pending_structure.store(true, Ordering::Release);
    guest.request_snapshot_resync();
    wait_event(&guest, |ev| matches!(ev, NetEvent::Disconnected(_)));
    let mut saw_overlay = false;
    let mut rejected = 0;
    loop {
        match wait_event(&guest, |ev| {
            matches!(
                ev,
                NetEvent::CellShiftOverlay { .. }
                    | NetEvent::StructuralRejected(_)
                    | NetEvent::CellShiftCommitted { .. }
                    | NetEvent::SnapshotDone
            )
        }) {
            NetEvent::CellShiftOverlay { seq: 0, .. } => saw_overlay = true,
            NetEvent::StructuralRejected(reason) => {
                assert!(reason.contains("未确认"));
                rejected += 1;
            }
            NetEvent::SnapshotDone => break,
            event => panic!("unsent operation appeared on recovery: {event:?}"),
        }
    }
    assert!(saw_overlay);
    assert_eq!(rejected, 1);
    assert!(lock_unpoison(&guest.shift_pending).is_none());
    guest.finish_structure();
    assert!(!guest.pending_structure());
}
