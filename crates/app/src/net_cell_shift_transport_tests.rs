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

fn handshake_room_tx_without_full_overlay(
    port: u16,
    site: u32,
) -> (BufReader<TcpStream>, TcpStream, sheetcore::wire::Session, serde_json::Value) {
    use serde_json::{json, Value};
    let writer = TcpStream::connect(("127.0.0.1", port)).unwrap();
    writer
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut reader = BufReader::new(writer.try_clone().unwrap());
    let challenge = read_line_bounded(&mut reader, MAX_PREAUTH_LINE)
        .unwrap()
        .unwrap();
    let challenge: Value = serde_json::from_str(&challenge).unwrap();
    assert_eq!(challenge["t"], "ChallengeSalt");
    let salt = sheetcore::wire::b64_decode(challenge["salt"].as_str().unwrap()).unwrap();
    let room_salt =
        sheetcore::wire::b64_decode(challenge["room_salt"].as_str().unwrap()).unwrap();
    let room = sheetcore::wire::RoomKey::derive("test-pw", &room_salt);
    let mut session = sheetcore::wire::Session::client_side(&room, &salt);
    // Simulates a 1c84f4f client: declares cell_shifts=true and room_tx=true,
    // but omits full_tx_overlay (defaults to false).
    let hello = json!({
        "t": "Hello",
        "site": site,
        "name": format!("LegacyRoomTx {site}"),
        "doc": "doc",
        "proof": room.proof(challenge["nonce"].as_u64().unwrap()),
        "auth": format!("legacy-rtx-{site}"),
        "base_hash": "",
        "cell_shifts_v2": true,
        "room_tx": true,
        "sort_permutation": true,
        "palette_styles": true,
        "resume_room": "",
        "resume_watermark": 0,
        "resume_struct_rev": 0,
        "binary_v3": false,
        "compact_v3": false
    });
    let mut w = writer;
    write_line(&mut w, &session.seal(&hello.to_string())).unwrap();
    let line = read_line_bounded(&mut reader, MAX_LINE).unwrap().unwrap();
    let response: Value =
        serde_json::from_str(&session.open(&line).expect("valid encrypted response")).unwrap();
    (reader, w, session, response)
}

#[test]
fn phase7_f2_new_host_gates_axis_permute_with_legacy_full_overlay_false_guest() {
    use sheetcore::{
        refs::Axis,
        tx::{AxisPermutePatch, DefinedNameIntent, DefinedNamesPatch, RoomTxPatch},
    };

    let _env = crate::test_env::TestEnv::new("p7-f2-new-host-legacy-guest");
    let (host, port) = tests::host_for_test(1, "Host", "doc", "test-pw");
    let (wb, _) = seed(&host);

    // 1) Legacy 1c84f4f peer (room_tx=true, full_tx_overlay omitted/false) is admitted before AxisPermute.
    let (mut legacy_reader, mut legacy_writer, mut legacy_sess, welcome) =
        handshake_room_tx_without_full_overlay(port, 2);
    assert_eq!(welcome["t"], "Welcome");
    assert_eq!(welcome["room_tx"], serde_json::json!(true));
    assert_eq!(welcome["full_tx_overlay"], serde_json::json!(true));
    wait_event(&host, |ev| matches!(ev, NetEvent::PeerJoined(2, ..)));

    let axis_patch = Arc::new(RoomTxPatch::AxisPermute(
        AxisPermutePatch::from_drop(&wb, 0, Axis::Row, 0, 1, 3).expect("axis permute patch"),
    ));

    // 2) Host attempting AxisPermute while full_tx_overlay=false peer is connected must be blocked.
    host.request_room_tx(101, CellShiftPhase::Forward, axis_patch.clone())
        .unwrap();
    wait_event(&host, |ev| {
        matches!(ev, NetEvent::StructuralRejected(msg) if msg.contains("完整元数据快照") && msg.contains("升级"))
    });
    host.finish_structure();

    // 3) The legacy peer itself attempting AxisPermute must also be rejected with upgrade message.
    let peer_req = Msg::RoomTxRequest {
        site: 2,
        id: 102,
        phase: CellShiftPhase::Forward,
        revision: 0,
        patch: axis_patch.clone(),
    };
    write_line(
        &mut legacy_writer,
        &legacy_sess.seal(&serde_json::to_string(&peer_req).unwrap()),
    )
    .unwrap();
    loop {
        let line = read_line_bounded(&mut legacy_reader, MAX_LINE)
            .unwrap()
            .unwrap();
        let msg: serde_json::Value =
            serde_json::from_str(&legacy_sess.open(&line).expect("open frame")).unwrap();
        if msg["t"] == "RoomTxRejected" {
            let reason = msg["reason"].as_str().unwrap_or("");
            assert!(
                reason.contains("完整元数据快照") && reason.contains("升级"),
                "unexpected rejection reason: {reason}"
            );
            break;
        }
    }

    // 4) DefinedNames (not requiring full AxisPermute overlay) still succeeds while room_tx=true peer is connected.
    let dn_patch = Arc::new(RoomTxPatch::DefinedNames(
        DefinedNamesPatch::prepare(
            &wb,
            &DefinedNameIntent::Define {
                name: "TaxRate".into(),
                scope: None,
                formula: "0.06".into(),
            },
        )
        .expect("prepare DefinedNames"),
    ));
    host.request_room_tx(103, CellShiftPhase::Forward, dn_patch.clone())
        .unwrap();
    let NetEvent::CheckRoomTx { reply, .. } =
        wait_event(&host, |ev| matches!(ev, NetEvent::CheckRoomTx { .. }))
    else {
        unreachable!()
    };
    reply.send(Ok(dn_patch)).unwrap();
    wait_event(&host, |ev| {
        matches!(ev, NetEvent::RoomTxCommitted { id: 103, .. })
    });
    host.finish_structure();

    // 5) Once legacy peer leaves, AxisPermute succeeds and commits.
    drop(legacy_reader);
    drop(legacy_writer);
    wait_event(&host, |ev| matches!(ev, NetEvent::PeerLeft(2)));

    host.request_room_tx(104, CellShiftPhase::Forward, axis_patch.clone())
        .unwrap();
    let NetEvent::CheckRoomTx { reply, .. } =
        wait_event(&host, |ev| matches!(ev, NetEvent::CheckRoomTx { .. }))
    else {
        unreachable!()
    };
    reply.send(Ok(axis_patch)).unwrap();
    wait_event(&host, |ev| {
        matches!(ev, NetEvent::RoomTxCommitted { id: 104, .. })
    });
    host.finish_structure();

    // 6) Legacy peer (room_tx=true, full_tx_overlay=false) trying to join/rejoin after AxisPermute must be DENIED.
    let (_, _, _, refusal) = handshake_room_tx_without_full_overlay(port, 2);
    assert_eq!(refusal["t"], "Denied", "expected Denied, got {refusal}");
    let reason = refusal["reason"].as_str().unwrap_or("");
    assert!(
        reason.contains("完整元数据快照") && reason.contains("升级"),
        "unexpected denial reason: {reason}"
    );
}

#[test]
fn phase7_f2_rejects_unknown_future_overlay_format_version() {
    use serde_json::json;
    use std::io::{BufRead, Write};

    let _env = crate::test_env::TestEnv::new("p7-f2-future-overlay-ver");
    let wb = Workbook::new_blank();
    let future_bytes = {
        let mut val = serde_json::to_value(CellShiftOverlay::capture(&wb)).unwrap();
        val.as_object_mut()
            .unwrap()
            .insert("format_version".into(), json!(99));
        serde_json::to_vec(&val).unwrap()
    };

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let salt = [3u8; 16];
        let room_salt = [5u8; 16];
        let room_key = sheetcore::wire::RoomKey::derive("pw", &room_salt);
        let challenge = Msg::ChallengeSalt {
            nonce: 77,
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
        let _ = sess.open(hello_line.trim()).expect("open Hello");

        let mut send_msg = |msg: &Msg| {
            let mut f = sess.seal(&serde_json::to_string(msg).unwrap());
            f.push('\n');
            stream.write_all(f.as_bytes()).unwrap();
            stream.flush().unwrap();
        };
        send_msg(&Msg::Welcome {
            site: 2,
            doc: "doc".into(),
            peers: vec![(1, "FutureHost".into(), 1)],
            delta_sync: false,
            clock: 1,
            batch_ops: true,
            cell_shifts: true,
            room_tx: true,
            full_tx_overlay: true,
            sort_permutation: true,
            structural_ack: true,
            thread_changes: true,
            palette_styles: true,
            room_id: "future-room".into(),
            watermark: 1,
            structure_revision: 0,
            binary_v3: false,
            compact_v3: false,
            acked_op_ts: 0,
        });
        send_msg(&Msg::CellShiftOverlayPart {
            seq: 1,
            index: 0,
            last: true,
            data: hex::encode(&future_bytes),
        });
        send_msg(&Msg::Snapshot { ops: Vec::new() });
        // Keep socket open until guest closes/denies so Windows TCP does not RST unread frames.
        let mut eof = String::new();
        let _ = reader.read_line(&mut eof);
    });

    let guest = join(
        2,
        "Guest".into(),
        "doc".into(),
        &format!("127.0.0.1:{port}"),
        "pw".into(),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut got_denied = None;
    while Instant::now() < deadline {
        match guest
            .events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            Ok(NetEvent::Denied(reason)) => {
                got_denied = Some(reason);
                break;
            }
            Ok(NetEvent::CellShiftOverlay { .. }) => {
                panic!("unknown future overlay format_version must NEVER be accepted");
            }
            Ok(NetEvent::SnapshotDone) => {
                panic!("snapshot must NOT complete after unknown future overlay format_version");
            }
            Ok(_) => {}
            Err(_) => break,
        }
    }
    let reason = got_denied.expect("expected NetEvent::Denied for unknown future format_version");
    assert!(
        reason.contains("v99") && reason.contains("升级"),
        "unexpected denial reason: {reason}"
    );
    guest.shutdown();
    let _ = server.join();
}

#[test]
fn phase7_f2_direction_b_rejects_post_axis_permute_v0_overlay_from_legacy_host() {
    use serde_json::json;
    use std::io::{BufRead, Write};

    let _env = crate::test_env::TestEnv::new("p7-f2-dir-b-legacy-v0-seq1");
    let wb = Workbook::new_blank();
    // Legacy 1c84f4f host payload: format_version omitted (defaults to 0) and missing v1 tables/layout.
    let v0_bytes = {
        let mut val = serde_json::to_value(CellShiftOverlay::capture(&wb)).unwrap();
        let obj = val.as_object_mut().unwrap();
        obj.remove("format_version");
        if let Some(sheets) = obj.get_mut("sheets").and_then(|s| s.as_array_mut()) {
            for sh in sheets {
                if let Some(so) = sh.as_object_mut() {
                    so.remove("tables");
                    so.remove("layout");
                }
            }
        }
        serde_json::to_vec(&val).unwrap()
    };

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let salt = [11u8; 16];
        let room_salt = [13u8; 16];
        let room_key = sheetcore::wire::RoomKey::derive("pw", &room_salt);
        let challenge = Msg::ChallengeSalt {
            nonce: 91,
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
        let _ = sess.open(hello_line.trim()).expect("open Hello");

        let mut send_json = |v: serde_json::Value| {
            let mut f = sess.seal(&v.to_string());
            f.push('\n');
            stream.write_all(f.as_bytes()).unwrap();
            stream.flush().unwrap();
        };
        // Legacy 1c84f4f Welcome: room_tx=true, cell_shifts=true, NO full_tx_overlay field.
        send_json(json!({
            "t": "Welcome",
            "site": 2,
            "doc": "doc",
            "peers": [[1, "LegacyHost", 1]],
            "delta_sync": false,
            "clock": 5,
            "batch_ops": true,
            "cell_shifts_v2": true,
            "room_tx": true,
            "sort_permutation": true,
            "structural_ack": true,
            "thread_changes": true,
            "palette_styles": true,
            "room_id": "legacy-room-after-permute",
            "watermark": 2,
            "structure_revision": 0,
            "binary_v3": false,
            "compact_v3": false,
            "acked_op_ts": 0
        }));
        // Legacy host already performed an unsaved coordinate transform (`seq: 1`) and sends v0 overlay.
        send_json(
            serde_json::to_value(Msg::CellShiftOverlayPart {
                seq: 1,
                index: 0,
                last: true,
                data: hex::encode(&v0_bytes),
            })
            .unwrap(),
        );
        send_json(serde_json::to_value(Msg::Snapshot { ops: Vec::new() }).unwrap());
        // Keep socket open until guest closes/denies so Windows TCP does not RST unread frames.
        let mut eof = String::new();
        let _ = reader.read_line(&mut eof);
    });

    let guest = join(
        2,
        "NewGuest".into(),
        "doc".into(),
        &format!("127.0.0.1:{port}"),
        "pw".into(),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut got_denied = None;
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        match guest
            .events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        {
            Ok(NetEvent::Denied(reason)) => {
                got_denied = Some(reason);
                break;
            }
            Ok(NetEvent::CellShiftOverlay { .. }) => {
                panic!("legacy v0 overlay with seq>0 from full_tx_overlay=false host must NOT be accepted");
            }
            Ok(NetEvent::SnapshotDone) => {
                panic!("snapshot must NOT complete after rejecting legacy post-permute v0 overlay");
            }
            Ok(ev) => {
                seen.push(format!("{ev:?}"));
            }
            Err(_) => break,
        }
    }
    let reason = got_denied.unwrap_or_else(|| {
        panic!("expected NetEvent::Denied for Direction B legacy post-permute v0 overlay; seen={seen:?}")
    });
    assert!(
        reason.contains("完整元数据快照") && reason.contains("升级"),
        "unexpected denial reason: {reason}"
    );
    guest.shutdown();
    let _ = server.join();
}

