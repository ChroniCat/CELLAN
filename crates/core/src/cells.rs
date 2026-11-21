//! 插入/删除单元格（下方右移 / 右侧右移 / 上移 / 左移）。
//!
//! 只在当前已用区域里挪格子：值、样式、批注、富文本、超链接一起走，
//! 合并区域整块在受影响列/行内时跟着挪。公式引用不做改写（Excel 会改写；
//! 这里是已知限制，插删行列请走 structural）。

use crate::model::{CellRef, Sheet};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shift {
    /// 插入时向下、删除时向上
    Vertical,
    /// 插入时向右、删除时向左
    Horizontal,
}

fn used(sheet: &Sheet) -> Option<(u32, u32)> {
    let mut max_r: Option<u32> = None;
    let mut max_c: Option<u32> = None;
    let mut bump = |r: u32, c: u32| {
        max_r = Some(max_r.map_or(r, |m| m.max(r)));
        max_c = Some(max_c.map_or(c, |m| m.max(c)));
    };
    if let Some((_, _, r2, c2)) = sheet.used_bounds() {
        bump(r2, c2);
    }
    for k in sheet.styles.keys() {
        bump(k.row, k.col);
    }
    for k in sheet.notes.keys() {
        bump(k.row, k.col);
    }
    for &(r, c) in sheet.rich.keys() {
        bump(r, c);
    }
    for &(r, c) in sheet.hyperlinks.keys() {
        bump(r, c);
    }
    for &(_, _, r2, c2) in &sheet.merges {
        bump(r2, c2);
    }
    max_r.zip(max_c)
}

fn clear_slot(sheet: &mut Sheet, pos: (u32, u32)) {
    sheet.remove(pos.0, pos.1);
    let k = CellRef::new(pos.0, pos.1);
    sheet.styles.remove(&k);
    if sheet.notes.remove(&k).is_some() {
        sheet.notes_dirty = true;
    }
    if sheet.rich.remove(&pos).is_some() {
        sheet.rich_dirty = true;
    }
    if sheet.hyperlinks.remove(&pos).is_some() {
        sheet.hyperlinks_dirty = true;
    }
}

fn move_slot(sheet: &mut Sheet, from: (u32, u32), to: (u32, u32)) {
    if from == to {
        return;
    }
    if let Some(cell) = sheet.get(from.0, from.1).cloned() {
        sheet.remove(from.0, from.1);
        sheet.set(to.0, to.1, cell);
    } else {
        sheet.remove(to.0, to.1);
    }

    let k_from = CellRef::new(from.0, from.1);
    let k_to = CellRef::new(to.0, to.1);
    if let Some(st) = sheet.styles.remove(&k_from) {
        sheet.set_style(to.0, to.1, st);
    } else {
        sheet.styles.remove(&k_to);
    }

    if let Some(mut n) = sheet.notes.remove(&k_from) {
        n.r = to.0;
        n.c = to.1;
        sheet.notes.insert(k_to, n);
        sheet.notes_dirty = true;
    } else if sheet.notes.remove(&k_to).is_some() {
        sheet.notes_dirty = true;
    }

    if let Some(v) = sheet.rich.remove(&from) {
        sheet.rich.insert(to, v);
        sheet.rich_dirty = true;
    } else if sheet.rich.remove(&to).is_some() {
        sheet.rich_dirty = true;
    }

    if let Some(v) = sheet.hyperlinks.remove(&from) {
        sheet.hyperlinks.insert(to, v);
        sheet.hyperlinks_dirty = true;
    } else if sheet.hyperlinks.remove(&to).is_some() {
        sheet.hyperlinks_dirty = true;
    }
}

/// 在 rect 覆盖的列/行里插入 n 格
pub fn insert(sheet: &mut Sheet, rect: (u32, u32, u32, u32), shift: Shift, n: u32) {
    if n == 0 {
        return;
    }
    let Some((used_r2, used_c2)) = used(sheet) else {
        return;
    };
    let (r1, c1, r2, c2) = rect;
    match shift {
        Shift::Vertical => {
            if c1 > used_c2 {
                return;
            }
            let c2_eff = c2.min(used_c2);
            if c1 > c2_eff {
                return;
            }
            for c in c1..=c2_eff {
                if used_r2 >= r1 {
                    let mut r = used_r2;
                    loop {
                        move_slot(sheet, (r, c), (r + n, c));
                        if r == r1 {
                            break;
                        }
                        r -= 1;
                    }
                }
                // 清出插入的空白带
                for rr in r1..(r1 + n) {
                    clear_slot(sheet, (rr, c));
                }
            }
            let mut merges_changed = false;
            for m in sheet.merges.iter_mut() {
                if m.0 >= r1 && m.1 >= c1 && m.3 <= c2 {
                    m.0 += n;
                    m.2 += n;
                    merges_changed = true;
                }
            }
            if merges_changed {
                sheet.merges_dirty = true;
            }
            let _ = r2;
        }
        Shift::Horizontal => {
            if r1 > used_r2 {
                return;
            }
            let r2_eff = r2.min(used_r2);
            if r1 > r2_eff {
                return;
            }
            for r in r1..=r2_eff {
                if used_c2 >= c1 {
                    let mut c = used_c2;
                    loop {
                        move_slot(sheet, (r, c), (r, c + n));
                        if c == c1 {
                            break;
                        }
                        c -= 1;
                    }
                }
                for cc in c1..(c1 + n) {
                    clear_slot(sheet, (r, cc));
                }
            }
            let mut merges_changed = false;
            for m in sheet.merges.iter_mut() {
                if m.0 >= r1 && m.2 <= r2 && m.1 >= c1 {
                    m.1 += n;
                    m.3 += n;
                    merges_changed = true;
                }
            }
            if merges_changed {
                sheet.merges_dirty = true;
            }
            let _ = c2;
        }
    }
    sheet.edited = true;
}

/// 删除 rect 里的 n 格：Vertical=下方上移，Horizontal=右侧左移
pub fn delete(sheet: &mut Sheet, rect: (u32, u32, u32, u32), shift: Shift, n: u32) {
    if n == 0 {
        return;
    }
    let Some((used_r2, used_c2)) = used(sheet) else {
        return;
    };
    let (r1, c1, r2, c2) = rect;
    match shift {
        Shift::Vertical => {
            if c1 > used_c2 || r1 > used_r2 {
                return;
            }
            let c2_eff = c2.min(used_c2);
            for c in c1..=c2_eff {
                for r in r1..=used_r2 {
                    let src = r + n;
                    if src <= used_r2 {
                        move_slot(sheet, (src, c), (r, c));
                    } else {
                        clear_slot(sheet, (r, c));
                    }
                }
            }
            let before_len = sheet.merges.len();
            let mut merges_changed = false;
            sheet.merges.retain_mut(|m| {
                if m.1 >= c1 && m.3 <= c2 {
                    if m.0 >= r1 && m.2 < r1 + n {
                        return false;
                    }
                    if m.0 >= r1 + n {
                        m.0 -= n;
                        m.2 -= n;
                        merges_changed = true;
                    }
                }
                true
            });
            if merges_changed || sheet.merges.len() != before_len {
                sheet.merges_dirty = true;
            }
            let _ = r2;
        }
        Shift::Horizontal => {
            if r1 > used_r2 || c1 > used_c2 {
                return;
            }
            let r2_eff = r2.min(used_r2);
            for r in r1..=r2_eff {
                for c in c1..=used_c2 {
                    let src = c + n;
                    if src <= used_c2 {
                        move_slot(sheet, (r, src), (r, c));
                    } else {
                        clear_slot(sheet, (r, c));
                    }
                }
            }
            let before_len = sheet.merges.len();
            let mut merges_changed = false;
            sheet.merges.retain_mut(|m| {
                if m.0 >= r1 && m.2 <= r2 {
                    if m.1 >= c1 && m.3 < c1 + n {
                        return false;
                    }
                    if m.1 >= c1 + n {
                        m.1 -= n;
                        m.3 -= n;
                        merges_changed = true;
                    }
                }
                true
            });
            if merges_changed || sheet.merges.len() != before_len {
                sheet.merges_dirty = true;
            }
            let _ = c2;
        }
    }
    sheet.edited = true;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Cell, Value};

    #[test]
    fn insert_down_moves_values_and_styles() {
        let mut sh = Sheet::new("t");
        for r in 0..3u32 {
            sh.set(r, 0, Cell { value: Value::Number(r as f64), formula: None });
        }
        sh.set_style(2, 0, 7);
        insert(&mut sh, (1, 0, 1, 0), Shift::Vertical, 1);
        assert_eq!(sh.value(0, 0), Value::Number(0.0));
        assert_eq!(sh.value(1, 0), Value::Empty, "插入空白带");
        assert_eq!(sh.value(2, 0), Value::Number(1.0));
        assert_eq!(sh.value(3, 0), Value::Number(2.0));
        assert_eq!(sh.style_of(3, 0), 7, "样式跟着走");
    }

    #[test]
    fn delete_up_pulls_cells() {
        let mut sh = Sheet::new("t");
        for r in 0..3u32 {
            sh.set(r, 0, Cell { value: Value::Number(r as f64), formula: None });
        }
        delete(&mut sh, (0, 0, 0, 0), Shift::Vertical, 1);
        assert_eq!(sh.value(0, 0), Value::Number(1.0));
        assert_eq!(sh.value(1, 0), Value::Number(2.0));
        assert_eq!(sh.value(2, 0), Value::Empty);
    }

    #[test]
    fn insert_right_shifts_row() {
        let mut sh = Sheet::new("t");
        for c in 0..3u32 {
            sh.set(0, c, Cell { value: Value::Number(c as f64), formula: None });
        }
        insert(&mut sh, (0, 1, 0, 1), Shift::Horizontal, 1);
        assert_eq!(sh.value(0, 1), Value::Empty);
        assert_eq!(sh.value(0, 2), Value::Number(1.0));
        assert_eq!(sh.value(0, 3), Value::Number(2.0));
    }

    #[test]
    fn insert_and_delete_preserve_notes_on_empty_cells_and_sync_coords() {
        let mut sh = Sheet::new("t");
        sh.notes.insert(
            CellRef::new(1, 0),
            crate::notes::Note {
                r: 1,
                c: 0,
                text: "空单元格批注".into(),
                author: "测试员".into(),
            },
        );
        sh.notes_dirty = false;

        insert(&mut sh, (1, 0, 1, 0), Shift::Vertical, 2);
        assert!(!sh.notes.contains_key(&CellRef::new(1, 0)));
        let moved = sh.notes.get(&CellRef::new(3, 0)).expect("空单元格上的批注必须随插入下移");
        assert_eq!((moved.r, moved.c), (3, 0), "Note 内部坐标必须同步更新");
        assert!(sh.notes_dirty);

        sh.notes_dirty = false;
        delete(&mut sh, (1, 0, 1, 0), Shift::Vertical, 2);
        let back = sh.notes.get(&CellRef::new(1, 0)).expect("删除空白带后批注应上移归位");
        assert_eq!((back.r, back.c), (1, 0));
        assert!(!sh.notes.contains_key(&CellRef::new(3, 0)));
        assert!(sh.notes_dirty);

        // 直接删除含批注的空格（下方为空格时）：旧批注必须清除干净，不得残留
        sh.notes_dirty = false;
        delete(&mut sh, (1, 0, 1, 0), Shift::Vertical, 1);
        assert!(sh.notes.is_empty(), "删除含批注单元格后批注不得残留");
        assert!(sh.notes_dirty);
    }
}
