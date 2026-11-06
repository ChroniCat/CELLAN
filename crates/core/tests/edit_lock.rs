//! 同一格禁止两人同时编辑
use sheetcore::sync::{LockOutcome, LockTable};

#[test]
fn second_editor_is_denied() {
    let mut t = LockTable::new(20_000);
    assert_eq!(t.request(0, 3, 2, 1, "张三", 1_000), LockOutcome::Granted);
    // 别人来抢，被挡
    match t.request(0, 3, 2, 2, "李四", 2_000) {
        LockOutcome::Denied { holder, name } => {
            assert_eq!(holder, 1);
            assert_eq!(name, "张三");
        }
        other => panic!("应该被拒绝，实际 {other:?}"),
    }
    // 自己再请求 = 续期
    assert_eq!(t.request(0, 3, 2, 1, "张三", 3_000), LockOutcome::Refreshed);
    // 不同格子互不影响
    assert_eq!(t.request(0, 4, 2, 2, "李四", 3_000), LockOutcome::Granted);
    assert_eq!(t.request(0, 3, 3, 2, "李四", 3_000), LockOutcome::Granted);
    assert_eq!(t.len(), 3);
}

#[test]
fn release_lets_the_next_person_in() {
    let mut t = LockTable::new(20_000);
    t.request(0, 0, 0, 7, "甲", 0);
    assert!(t.is_locked_by_other(0, 0, 0, 8));
    // 非持有者释放无效
    assert!(!t.release(0, 0, 0, 8));
    assert!(t.release(0, 0, 0, 7));
    assert!(!t.is_locked_by_other(0, 0, 0, 8));
    assert_eq!(t.request(0, 0, 0, 8, "乙", 10), LockOutcome::Granted);
}

#[test]
fn crashed_editor_expires() {
    let mut t = LockTable::new(20_000);
    t.request(0, 1, 1, 3, "丙", 0);
    // 19.9 秒还在
    assert!(t.is_locked_by_other(0, 1, 1, 4));
    // 20 秒后清掉
    let dead = t.expire(20_000);
    assert_eq!(dead, vec![(0, 1, 1, 3)]);
    assert_eq!(t.request(0, 1, 1, 4, "丁", 20_001), LockOutcome::Granted);
}

#[test]
fn lock_survives_ops_and_only_one_holder_per_cell() {
    let mut t = LockTable::new(60_000);
    // 100 个站点抢同一格，只能有一个成功
    let mut granted = 0;
    for site in 0..100u32 {
        if t.request(2, 9, 9, site, "x", 100) == LockOutcome::Granted {
            granted += 1;
        }
    }
    assert_eq!(granted, 1);
    // 同一时刻只有一个人
    assert_eq!(t.holder(2, 9, 9).map(|h| h.site), Some(0));
}

#[test]
fn style_and_merge_messages_round_trip() {
    use sheetcore::style::{Border, CellStyle};
    use sheetcore::sync::Msg;
    let mut st = CellStyle::default();
    st.bold = true;
    st.border[0] = Border { style: 1, color: [255, 0, 0] };
    let msg = Msg::StyleOp { site: 3, sheet: 1, ts: 5, items: vec![(2, 3, st.clone())] };
    let line = serde_json::to_string(&msg).unwrap();
    match serde_json::from_str::<Msg>(&line).unwrap() {
        Msg::StyleOp { sheet, items, .. } => {
            assert_eq!(sheet, 1);
            assert_eq!(items.len(), 1);
            assert!(items[0].2.bold);
            assert_eq!(items[0].2.border[0].color, [255, 0, 0]);
        }
        _ => panic!("解析成了别的消息"),
    }
    let m = Msg::MergeOp { site: 1, sheet: 0, ts: 1, r1: 0, c1: 0, r2: 2, c2: 3, on: true };
    let line = serde_json::to_string(&m).unwrap();
    assert!(line.contains("\"t\":\"MergeOp\""), "标签不对: {line}");
    // 分片消息
    let c = Msg::SnapshotChunk { ops: vec![], last: true };
    let line = serde_json::to_string(&c).unwrap();
    assert!(serde_json::from_str::<Msg>(&line).is_ok());
}
