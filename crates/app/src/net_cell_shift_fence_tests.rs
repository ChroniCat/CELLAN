use super::*;
use sheetcore::{
    Cell, Value, Workbook, cells::Shift, structural::CellShift, style::CellStyle, sync::ValueWire,
};
use std::time::Instant;

impl Net {
    pub(crate) fn received_watermark_for_test(&self) -> u64 {
        self.last_watermark_seq.load(Ordering::Acquire)
    }
}

fn next_matching(net: &Net, matches: impl Fn(&NetEvent) -> bool) -> NetEvent {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let event = net
            .events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("timed out waiting for room event");
        assert!(
            !matches!(event, NetEvent::StructuralRejected(_)),
            "{event:?}"
        );
        if matches(&event) {
            return event;
        }
    }
}

#[test]
fn partial_cell_shift_room_rejects_and_repairs_stale_cells_and_styles_until_guest_ack() {
    let _env = crate::test_env::TestEnv::new("cell-shift-ack-fence");
    let (host, port) = host_for_test(1, "Owner", "doc", "pw");
    let mut workbook = Workbook::new_blank();
    let mut peer = sheetcore::sync::Peer::new(1);
    let input = Cell {
        value: Value::Number(11.0),
        formula: None,
    };
    workbook.sheet_mut(0).set(1, 0, input.clone());
    peer.seed_cell(0, 1, 0, &input);
    let moved_style = CellStyle {
        bold: true,
        ..Default::default()
    };
    let style_id = workbook.styles.intern(moved_style.clone());
    workbook.sheet_mut(0).set_style(1, 0, style_id);
    host.seed_snapshot(
        &peer,
        &workbook,
        Cursor {
            site: 1,
            name: "Owner".into(),
            color: 1,
            sheet: 0,
            row: 0,
            col: 0,
            end_row: None,
            end_col: None,
        },
    );
    let guest = join(
        2,
        "Guest".into(),
        "doc".into(),
        &format!("127.0.0.1:{port}"),
        "pw".into(),
    )
    .unwrap();
    next_matching(&guest, |event| matches!(event, NetEvent::SnapshotDone));

    let shift = CellShift::new((1, 0, 1, 0), Shift::Vertical, true);
    let patch = Arc::new(CellShiftPatch::prepare(&workbook, 0, shift).unwrap());
    host.request_cell_shift(5, CellShiftPhase::Forward, patch.clone())
        .unwrap();
    let NetEvent::CheckCellShift { reply, .. } = next_matching(&host, |event| {
        matches!(event, NetEvent::CheckCellShift { .. })
    }) else {
        unreachable!()
    };
    reply.send(Ok(patch)).unwrap();
    let NetEvent::CellShiftCommitted { seq, .. } = next_matching(&guest, |event| {
        matches!(event, NetEvent::CellShiftCommitted { id: 5, .. })
    }) else {
        unreachable!()
    };
    // The socket has delivered the new coordinates, but this guest UI has not accepted them.
    let cell = |value, ts| Op {
        site: 2,
        sheet: 0,
        row: 2,
        col: 0,
        value: ValueWire::Number(value),
        formula: None,
        ts,
    };
    let attempted_style = CellStyle {
        italic: true,
        ..Default::default()
    };
    guest.broadcast_ops(vec![cell(99.0, 100)]);
    guest.broadcast_style(0, vec![(2, 0, attempted_style.clone())]);
    let (mut cell_repaired, mut style_repaired) = (false, false);
    while !cell_repaired || !style_repaired {
        let NetEvent::WriteRejected {
            sheet,
            row,
            col,
            revert_ops,
            revert_styles,
            ..
        } = next_matching(&guest, |event| {
            matches!(event, NetEvent::WriteRejected { .. })
        })
        else {
            unreachable!()
        };
        assert_eq!((sheet, row, col), (0, 2, 0));
        for (r, c, op) in revert_ops {
            assert_eq!((r, c), (2, 0));
            let op = op.unwrap();
            assert_eq!((op.site, op.value), (1, ValueWire::Number(11.0)));
            cell_repaired = true;
        }
        for (r, c, style) in revert_styles {
            assert_eq!((r, c, style.as_ref()), (2, 0, Some(&moved_style)));
            style_repaired = true;
        }
    }
    let host_value = || {
        latest_cell_op(
            host.host_baseline.as_ref(),
            host.state.as_ref().unwrap(),
            0,
            2,
            0,
        )
        .unwrap()
        .value
    };
    let host_style = || {
        latest_cell_style(
            host.host_baseline.as_ref(),
            None,
            host.host_styles.as_ref().unwrap(),
            host.delta_styles.as_ref(),
            0,
            2,
            0,
        )
    };
    assert_eq!(host_value(), ValueWire::Number(11.0));
    assert_eq!(host_style(), Some(moved_style));
    let deadline = Instant::now() + Duration::from_secs(10);
    while guest.acked_op_ts() < 100 {
        assert!(
            Instant::now() < deadline,
            "rejected edit would be replayed on reconnect"
        );
        std::thread::yield_now();
    }

    guest.ack_cell_shift(seq);
    guest.broadcast_ops(vec![cell(77.0, 101)]);
    guest.broadcast_style(0, vec![(2, 0, attempted_style.clone())]);
    next_matching(
        &host,
        |event| matches!(event, NetEvent::Ops(ops) if ops.iter().any(|op| op.ts == 101 && op.value == ValueWire::Number(77.0))),
    );
    next_matching(
        &host,
        |event| matches!(event, NetEvent::Styles { site: 2, items, .. } if items.iter().any(|(s,r,c,style)| (*s,*r,*c) == (0,2,0) && style == &attempted_style)),
    );
    assert_eq!(host_value(), ValueWire::Number(77.0));
    assert_eq!(host_style(), Some(attempted_style));
}
