//! C3：撤销只退自己的动作

use sheetcore::model::{Cell, Sheet, Value, Workbook};
use sheetcore::undo::{self, Action, UndoStack};

fn wb_with(value: &str) -> Workbook {
    let mut s = Sheet::new("S");
    s.set(0, 0, Cell { value: Value::Text(value.into()), formula: None });
    let mut wb = Workbook::default();
    wb.sheets.push(s);
    wb
}

fn change_to(wb: &mut Workbook, label: &str, text: &str) -> Action {
    let keys = undo::rect_keys(0, 0, 0, 0);
    let before = undo::capture(&wb.sheets[0], 0, &keys);
    wb.sheets[0].set(0, 0, Cell { value: Value::Text(text.into()), formula: None });
    let deltas = undo::finish(before, &wb.sheets[0]);
    Action {
        label: label.into(),
        deltas,
        focus: (0, 0, 0),
        snap_before: None,
        snap_after: None,
        by_site: 0,
            shapes: None,
            refresh_sizes: None,
    }
}

fn value_at(wb: &Workbook) -> String {
    wb.sheets[0].get(0, 0).map(|c| c.value.display()).unwrap_or_default()
}

#[test]
fn my_action_is_undoable() {
    let mut wb = wb_with("原始");
    let mut stack = UndoStack::default();
    stack.set_site(111);
    let a = change_to(&mut wb, "我改的", "我的");
    stack.push(a);
    assert_eq!(stack.depth(), (1, 0));

    let applied = stack.undo(&mut wb.sheets, |_, _, _, _| true).expect("应该能撤");
    assert_eq!(applied.label, "我改的");
    assert_eq!(value_at(&wb), "原始");
}

#[test]
fn foreign_action_is_not_undone() {
    let mut wb = wb_with("原始");
    let mut stack = UndoStack::default();
    stack.set_site(111);

    // 别人的动作（by_site 已写好，push 不会覆盖）
    let mut a = change_to(&mut wb, "别人改的", "他的");
    a.by_site = 999;
    stack.push(a);

    assert!(stack.undo(&mut wb.sheets, |_, _, _, _| true).is_none(), "不该撤别人的动作");
    assert_eq!(value_at(&wb), "他的", "撤销动了别人的格子");
    assert_eq!(stack.depth(), (1, 0), "动作应该还在栈里，没被丢掉");
}

#[test]
fn switching_site_clears_history() {
    let mut wb = wb_with("原始");
    let mut stack = UndoStack::default();
    stack.set_site(111);
    stack.push(change_to(&mut wb, "我改的", "我的"));
    assert_eq!(stack.depth(), (1, 0));

    // 退出房间 → 站点归零，历史不该留给下一个人
    stack.set_site(0);
    assert_eq!(stack.depth(), (0, 0));
    assert!(stack.undo(&mut wb.sheets, |_, _, _, _| true).is_none());
    assert_eq!(value_at(&wb), "我的");
}

#[test]
fn collaboration_guard_skips_cells_changed_by_others() {
    let mut wb = wb_with("原始");
    let mut stack = UndoStack::default();
    stack.push(change_to(&mut wb, "我改的", "我的"));

    // 别人把我改过的格又改了 → 撤销时跳过，不覆盖他
    wb.sheets[0].set(0, 0, Cell { value: Value::Text("同事的".into()), formula: None });
    let applied = stack.undo(&mut wb.sheets, |_, _, _, _| false).expect("动作还在");
    assert_eq!(applied.skipped, 1, "应该报告跳过 1 格");
    assert_eq!(value_at(&wb), "同事的", "撤销覆盖了同事的改动");
}
