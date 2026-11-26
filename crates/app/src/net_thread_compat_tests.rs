use super::*;
use serde_json::{Value, json};
use sheetcore::notes::{Note, ThreadEntry, ThreadedComment};
use sheetcore::sync::SheetMetaKind;

/// A v1.83.0 text-frame client. Its Hello and comment operations deliberately
/// bypass the current Msg and SheetMetaKind serializers.
struct LegacyPeer {
    reader: BufReader<TcpStream>,
    writer: TcpStream,
    session: sheetcore::wire::Session,
}

impl LegacyPeer {
    fn connect(port: u16, site: u32, room_id: &str, watermark: u64) -> (Self, Value) {
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
        let mut peer = Self {
            reader,
            writer,
            session: sheetcore::wire::Session::client_side(&room, &salt),
        };
        peer.send(json!({
            "t": "Hello", "site": site, "name": format!("Guest {site}"), "doc": "doc",
            "proof": room.proof(challenge["nonce"].as_u64().unwrap()), "auth": format!("old-{site}"),
            "base_hash": "", "sort_permutation": false, "palette_styles": false,
            "resume_room": room_id, "resume_watermark": watermark, "resume_struct_rev": 0,
            "binary_v3": false, "compact_v3": false
        }));
        let welcome = peer.read();
        assert_eq!(welcome["t"], "Welcome", "{welcome}");
        (peer, welcome)
    }

    fn send(&mut self, value: Value) {
        write_line(&mut self.writer, &self.session.seal(&value.to_string())).unwrap();
    }

    fn read(&mut self) -> Value {
        let line = read_line_bounded(&mut self.reader, MAX_LINE)
            .unwrap()
            .unwrap();
        serde_json::from_str(
            &self
                .session
                .open(&line)
                .expect("valid encrypted text frame"),
        )
        .unwrap()
    }

    fn next_of(&mut self, tag: &str) -> Value {
        loop {
            let msg = self.read();
            if msg["t"] == tag {
                return msg;
            }
        }
    }
}

#[test]
fn legacy_comment_frames_preserve_replies_and_replay_demotion_on_reconnect() {
    let _env = crate::test_env::TestEnv::new("old-thread-wire");
    let (host, port) = tests::host_for_test(1, "Host", "doc", "test-pw");
    let entry = |id: &str| ThreadEntry {
        id: id.into(),
        author: id.into(),
        timestamp: String::new(),
        text: id.into(),
    };
    let current = ThreadedComment {
        r: 3,
        c: 5,
        resolved: false,
        entries: vec![entry("root"), entry("reply")],
    };
    let set = SheetMetaKind::Thread {
        row: 3,
        col: 5,
        thread: Some(current.clone()),
    };
    assert!(host.broadcast_sheet_meta(0, set.clone()));
    let canonical = serde_json::to_value(set).unwrap();
    let (mut legacy, welcome) = LegacyPeer::connect(port, 2, "", 0);
    assert_eq!(welcome["delta_sync"], false);
    assert_eq!(
        legacy.next_of("SheetMetaSnapshot")["items"],
        json!([[0, canonical]])
    );

    // Two normal pre-upgrade mutations can arrive after a concurrent reply.
    let stale = json!({"Thread":{"row":3,"col":5,"thread":{"r":3,"c":5,"resolved":false,
        "entries":[{"id":"root","author":"root","timestamp":"","text":"old text"}]}}});
    let erase = json!({"Note":{"row":3,"col":5,"text":null,"author":""}});
    for kind in [stale, erase] {
        legacy.send(json!({"t":"SheetMetaOp","site":2,"sheet":0,"ts":0,"kind":kind}));
        let echo = legacy.next_of("SheetMetaOp");
        assert_eq!(echo["site"], 0, "the host must correct the sender");
        assert_eq!(echo["kind"], canonical);
    }

    let note = Note {
        r: 3,
        c: 5,
        author: "Host".into(),
        text: "restored note".into(),
    };
    let demotion = SheetMetaKind::ThreadChange {
        row: 3,
        col: 5,
        before: Some(current),
        after: None,
        before_note: None,
        legacy_note: Some(note),
    };
    assert!(host.broadcast_sheet_meta(0, demotion));
    let removed = json!({"Thread":{"row":3,"col":5,"thread":null}});
    let restored = json!({"Note":{"row":3,"col":5,"author":"Host","text":"restored note"}});
    assert_eq!(legacy.next_of("SheetMetaOp")["kind"], removed);
    assert_eq!(legacy.next_of("SheetMetaOp")["kind"], restored);

    drop(legacy);
    assert!(tests::wait_for(&host, 5, |ev| matches!(
        ev,
        NetEvent::PeerLeft(2)
    )));
    let (mut resumed, replay) = LegacyPeer::connect(
        port,
        2,
        welcome["room_id"].as_str().unwrap(),
        welcome["watermark"].as_u64().unwrap(),
    );
    assert_eq!(replay["delta_sync"], true);
    assert_eq!(
        resumed.next_of("SheetMetaSnapshot")["items"],
        json!([[0, removed], [0, restored]])
    );
}

#[test]
fn legacy_welcome_keeps_normal_notes_available_and_rejects_conditional_thread_updates() {
    let _env = crate::test_env::TestEnv::new("old-host-thread-wire");
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let addr = listener.local_addr().unwrap();
    let old_host = std::thread::spawn(move || {
        let (mut writer, _) = listener.accept().unwrap();
        writer
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(writer.try_clone().unwrap());
        let salt = [7u8; 16];
        let room_salt = [9u8; 16];
        let key = sheetcore::wire::RoomKey::derive("test-pw", &room_salt);
        let mut session = sheetcore::wire::Session::host_side(&key, &salt);
        let challenge = json!({"t":"ChallengeSalt", "nonce":42, "salt":sheetcore::wire::b64_encode(&salt),
            "room_salt":sheetcore::wire::b64_encode(&room_salt)});
        write_line(&mut writer, &challenge.to_string()).unwrap();
        let hello = read_line_bounded(&mut reader, MAX_PREAUTH_LINE)
            .unwrap()
            .unwrap();
        let hello: Value = serde_json::from_str(&session.open(&hello).unwrap()).unwrap();
        assert_eq!(hello["proof"], key.proof(42));
        let welcome = r#"{"t":"Welcome","site":2,"doc":"doc","peers":[[1,"Host",1]],"delta_sync":false,"batch_ops":true,"clock":1,"binary_v3":false,"compact_v3":false}"#;
        write_line(&mut writer, &session.seal(welcome)).unwrap();
        write_line(&mut writer, &session.seal(r#"{"t":"Snapshot","ops":[]}"#)).unwrap();
        loop {
            let line = read_line_bounded(&mut reader, MAX_LINE).unwrap().unwrap();
            let msg: Value = serde_json::from_str(&session.open(&line).unwrap()).unwrap();
            if msg["t"] == "SheetMetaOp" {
                return msg["kind"].clone();
            }
        }
    });
    let guest = join(
        2,
        "Guest".into(),
        "doc".into(),
        &addr.to_string(),
        "test-pw".into(),
    )
    .unwrap();
    assert!(tests::wait_for(&guest, 5, |ev| matches!(
        ev,
        NetEvent::SnapshotDone
    )));
    assert!(!guest.supports_thread_changes());
    let entry = ThreadEntry {
        id: "root".into(),
        author: "Guest".into(),
        timestamp: String::new(),
        text: "root".into(),
    };
    let existing = ThreadedComment {
        r: 3,
        c: 5,
        resolved: false,
        entries: vec![entry],
    };
    assert!(!guest.broadcast_sheet_meta(
        0,
        SheetMetaKind::ThreadChange {
            row: 3,
            col: 5,
            before: Some(existing),
            after: None,
            before_note: None,
            legacy_note: None,
        }
    ));
    assert!(guest.broadcast_sheet_meta(
        0,
        SheetMetaKind::Note {
            row: 3,
            col: 5,
            author: "Guest".into(),
            text: Some("note".into()),
        }
    ));
    assert_eq!(
        old_host.join().unwrap(),
        json!({"Note":{"row":3,"col":5,"author":"Guest","text":"note"}})
    );
}
