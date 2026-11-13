//! C01.4 错误值在协作同步与撤销/重做里的端到端行为。
//!
//! 模型迁移之后，`Value::Error` 必须能原样穿过三条会被序列化或快照的路径：
//! 网络操作（Op → JSON → 远端）、撤销栈（CellDelta 前后 Cell）、整表快照。

use sheetcore::formula::ErrKind;
use sheetcore::sync::{Op, Peer, ValueWire};
use sheetcore::undo::{self, Action, UndoStack};
use sheetcore::{Cell, Value, Workbook};

fn cell(v: Value, f: Option<&str>) -> Cell {
    Cell { value: v, formula: f.map(|s| s.into()) }
}

#[test]
fn an_error_op_survives_json_and_applies_as_an_error() {
    let mut a = Peer::new(1);
    let op = a.local_set(0, 0, 0, &cell(Value::Error(ErrKind::Div0), Some("=1/0")));

    // 真的走一遍序列化（网络上传的就是这个）
    let json = serde_json::to_string(&op).expect("序列化失败");
    let back: Op = serde_json::from_str(&json).expect("反序列化失败");
    assert_eq!(back.value, ValueWire::Error("#DIV/0!".into()));
    assert_eq!(back.formula.as_deref(), Some("=1/0"));

    let mut b = Peer::new(2);
    assert!(b.apply(back), "远端操作没被接受");
    let mut wb = Workbook::new_blank();
    b.apply_all_to_workbook(&mut wb);
    assert_eq!(
        wb.sheets[0].value(0, 0),
        Value::Error(ErrKind::Div0),
        "同步过来的错误变成了 {:?}",
        wb.sheets[0].value(0, 0)
    );
}

#[test]
fn text_that_looks_like_an_error_stays_text_on_the_wire() {
    let mut a = Peer::new(1);
    let op = a.local_set(0, 0, 0, &cell(Value::Text("#DIV/0!".into()), None));
    let back: Op = serde_json::from_str(&serde_json::to_string(&op).unwrap()).unwrap();
    let mut wb = Workbook::new_blank();
    let mut b = Peer::new(2);
    b.apply(back);
    b.apply_all_to_workbook(&mut wb);
    assert_eq!(wb.sheets[0].value(0, 0), Value::Text("#DIV/0!".into()));
}

#[test]
fn last_writer_wins_between_an_error_and_a_value() {
    let mut a = Peer::new(1);
    let mut b = Peer::new(2);
    let err_op = a.local_set(0, 0, 0, &cell(Value::Error(ErrKind::Na), None));
    b.apply(err_op);
    let num_op = b.local_set(0, 0, 0, &cell(Value::Number(3.0), None));
    assert!(a.apply(num_op.clone()), "更晚的写入应胜出");
    let mut wb = Workbook::new_blank();
    a.apply_all_to_workbook(&mut wb);
    assert_eq!(wb.sheets[0].value(0, 0), Value::Number(3.0));
    // 反向：错误更晚
    let mut c = Peer::new(3);
    c.apply(a.local_set(0, 0, 0, &cell(Value::Number(9.0), None)));
    let late_err = c.local_set(0, 0, 0, &cell(Value::Error(ErrKind::Ref), None));
    assert!(a.apply(late_err));
    a.apply_all_to_workbook(&mut wb);
    assert_eq!(wb.sheets[0].value(0, 0), Value::Error(ErrKind::Ref));
}

#[test]
fn undo_and_redo_restore_the_error_cell_verbatim() {
    let mut sheets = vec![sheetcore::model::Sheet::new("S")];
    sheets[0].set(0, 0, cell(Value::Error(ErrKind::Div0), Some("=1/0")));

    let before = undo::capture(&sheets[0], 0, &[(0, 0)]);
    sheets[0].set(0, 0, cell(Value::Number(1.0), None));
    let deltas = undo::finish(before, &sheets[0]);
    assert_eq!(deltas.len(), 1);
    assert_eq!(deltas[0].before.value, Value::Error(ErrKind::Div0));

    let mut stack = UndoStack::new();
    stack.push(Action {
        label: "输入".into(),
        merges: None,
        deltas,
        focus: (0, 0, 0),
        snap_before: None,
        snap_after: None,
        by_site: 0,
        pictures: None,
        shapes: None,
        refresh_sizes: None,
    });

    assert!(stack.undo(&mut sheets, |_, _, _, _| true).is_some());
    assert_eq!(sheets[0].value(0, 0), Value::Error(ErrKind::Div0), "撤销后错误值没回来");
    assert_eq!(
        sheets[0].get(0, 0).unwrap().formula.as_deref(),
        Some("=1/0"),
        "撤销后公式没回来"
    );
    assert!(stack.redo(&mut sheets, |_, _, _, _| true).is_some());
    assert_eq!(sheets[0].value(0, 0), Value::Number(1.0));
}

#[test]
fn sheet_snapshot_roundtrips_error_cells() {
    let mut sheet = sheetcore::model::Sheet::new("S");
    sheet.set(0, 0, cell(Value::Error(ErrKind::Num), None));
    sheet.set(1, 0, cell(Value::Text("#NUM!".into()), None));
    let snap = undo::snapshot_sheet(&sheet, 0);
    let mut fresh = sheetcore::model::Sheet::new("S");
    undo::restore_sheet(&mut fresh, &snap);
    assert_eq!(fresh.value(0, 0), Value::Error(ErrKind::Num));
    assert_eq!(fresh.value(1, 0), Value::Text("#NUM!".into()));
}

#[test]
fn the_network_envelope_carries_the_error_type() {
    // net.rs 发出去的就是 serde_json::to_string(&Msg::Op(op))，这里测同一层信封
    let mut a = Peer::new(7);
    let op = a.local_set(0, 0, 0, &cell(Value::Error(ErrKind::Div0), Some("=1/0")));
    let line = serde_json::to_string(&sheetcore::sync::Msg::Op(op)).expect("信封序列化失败");
    let back: sheetcore::sync::Msg = serde_json::from_str(&line).expect("信封反序列化失败");
    match back {
        sheetcore::sync::Msg::Op(op) => {
            assert_eq!(op.value, ValueWire::Error("#DIV/0!".into()));
            let mut wb = Workbook::new_blank();
            let mut b = Peer::new(8);
            assert!(b.apply(op));
            b.apply_all_to_workbook(&mut wb);
            assert_eq!(wb.sheets[0].value(0, 0), Value::Error(ErrKind::Div0));
        }
        other => panic!("解出来的不是 Op：{other:?}"),
    }
}
