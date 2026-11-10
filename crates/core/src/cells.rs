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

fn used(sheet: &Sheet) -> (u32, u32) {
    sheet
        .used_bounds()
        .map(|(_, _, r2, c2)| (r2, c2))
        .unwrap_or((0, 0))
}

fn take_cell(sheet: &mut Sheet, r: u32, c: u32) -> Option<(crate::model::Cell, Option<u16>)> {
    let cell = sheet.get(r, c).cloned()?;
    let st = sheet.styles.get(&CellRef::new(r, c)).copied();
    Some((cell, st))
}

fn put_cell(sheet: &mut Sheet, r: u32, c: u32, cell: &crate::model::Cell, st: Option<u16>) {
    sheet.set(r, c, cell.clone());
    match st {
        Some(id) => {
            sheet.set_style(r, c, id);
        }
        None => {
            sheet.styles.remove(&CellRef::new(r, c));
        }
    }
}

fn move_side_maps(sheet: &mut Sheet, from: (u32, u32), to: (u32, u32)) {
    let k1 = CellRef::new(from.0, from.1);
    let k2 = CellRef::new(to.0, to.1);
    if let Some(n) = sheet.notes.remove(&k1) {
        sheet.notes.insert(k2, n);
    }
    if let Some(v) = sheet.rich.remove(&from) {
        sheet.rich.insert(to, v);
        sheet.rich_dirty = true;
    }
    if let Some(v) = sheet.hyperlinks.remove(&from) {
        sheet.hyperlinks.insert(to, v);
        sheet.hyperlinks_dirty = true;
    }
}

/// 在 rect 覆盖的列/行里插入 n 格
pub fn insert(sheet: &mut Sheet, rect: (u32, u32, u32, u32), shift: Shift, n: u32) {
    if n == 0 {
        return;
    }
    let (used_r2, used_c2) = used(sheet);
    let (r1, c1, r2, c2) = rect;
    match shift {
        Shift::Vertical => {
            let c1 = c1.min(used_c2);
            let c2 = c2.min(used_c2);
            if c1 > c2 {
                return;
            }
            for c in c1..=c2 {
                let mut r = used_r2;
                loop {
                    if let Some((cell, st)) = take_cell(sheet, r, c) {
                        let (cell, st) = (cell.clone(), st);
                        sheet.remove(r, c);
                        put_cell(sheet, r + n, c, &cell, st);
                        move_side_maps(sheet, (r, c), (r + n, c));
                    }
                    if r == r1 {
                        break;
                    }
                    if r == 0 {
                        break;
                    }
                    r -= 1;
                }
                // 清出插入的空白带
                for rr in r1..(r1 + n) {
                    sheet.remove(rr, c);
                    sheet.styles.remove(&CellRef::new(rr, c));
                    sheet.notes.remove(&CellRef::new(rr, c));
                    sheet.rich.remove(&(rr, c));
                    sheet.hyperlinks.remove(&(rr, c));
                }
            }
            for m in sheet.merges.iter_mut() {
                if m.0 >= r1 && m.1 >= c1 && m.3 <= c2 {
                    m.0 += n;
                    m.2 += n;
                }
            }
            let _ = r2;
        }
        Shift::Horizontal => {
            let r1 = r1.min(used_r2);
            let r2 = r2.min(used_r2);
            if r1 > r2 {
                return;
            }
            for r in r1..=r2 {
                let mut c = used_c2;
                loop {
                    if let Some((cell, st)) = take_cell(sheet, r, c) {
                        let (cell, st) = (cell.clone(), st);
                        sheet.remove(r, c);
                        put_cell(sheet, r, c + n, &cell, st);
                        move_side_maps(sheet, (r, c), (r, c + n));
                    }
                    if c == c1 {
                        break;
                    }
                    if c == 0 {
                        break;
                    }
                    c -= 1;
                }
                for cc in c1..(c1 + n) {
                    sheet.remove(r, cc);
                    sheet.styles.remove(&CellRef::new(r, cc));
                    sheet.notes.remove(&CellRef::new(r, cc));
                    sheet.rich.remove(&(r, cc));
                    sheet.hyperlinks.remove(&(r, cc));
                }
            }
            for m in sheet.merges.iter_mut() {
                if m.0 >= r1 && m.2 <= r2 && m.1 >= c1 {
                    m.1 += n;
                    m.3 += n;
                }
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
    let (used_r2, used_c2) = used(sheet);
    let (r1, c1, r2, c2) = rect;
    match shift {
        Shift::Vertical => {
            let c1 = c1.min(used_c2);
            let c2 = c2.min(used_c2);
            for c in c1..=c2 {
                let mut r = r1;
                while r <= used_r2 {
                    let src = r + n;
                    if src <= used_r2 {
                        if let Some((cell, st)) = take_cell(sheet, src, c) {
                            put_cell(sheet, r, c, &cell, st);
                            move_side_maps(sheet, (src, c), (r, c));
                        } else {
                            sheet.remove(r, c);
                            sheet.styles.remove(&CellRef::new(r, c));
                        }
                    } else {
                        sheet.remove(r, c);
                        sheet.styles.remove(&CellRef::new(r, c));
                        sheet.notes.remove(&CellRef::new(r, c));
                        sheet.rich.remove(&(r, c));
                        sheet.hyperlinks.remove(&(r, c));
                    }
                    r += 1;
                }
                for rr in (used_r2.saturating_sub(n) + 1)..=used_r2 {
                    sheet.remove(rr, c);
                    sheet.styles.remove(&CellRef::new(rr, c));
                    sheet.notes.remove(&CellRef::new(rr, c));
                    sheet.rich.remove(&(rr, c));
                    sheet.hyperlinks.remove(&(rr, c));
                }
            }
            for m in sheet.merges.iter_mut() {
                if m.0 >= r1 && m.1 >= c1 && m.3 <= c2 {
                    m.0 = m.0.saturating_sub(n);
                    m.2 = m.2.saturating_sub(n);
                }
            }
            let _ = r2;
        }
        Shift::Horizontal => {
            let r1 = r1.min(used_r2);
            let r2 = r2.min(used_r2);
            for r in r1..=r2 {
                let mut c = c1;
                while c <= used_c2 {
                    let src = c + n;
                    if src <= used_c2 {
                        if let Some((cell, st)) = take_cell(sheet, r, src) {
                            put_cell(sheet, r, c, &cell, st);
                            move_side_maps(sheet, (r, src), (r, c));
                        } else {
                            sheet.remove(r, c);
                            sheet.styles.remove(&CellRef::new(r, c));
                        }
                    } else {
                        sheet.remove(r, c);
                        sheet.styles.remove(&CellRef::new(r, c));
                        sheet.notes.remove(&CellRef::new(r, c));
                        sheet.rich.remove(&(r, c));
                        sheet.hyperlinks.remove(&(r, c));
                    }
                    c += 1;
                }
                for cc in (used_c2.saturating_sub(n) + 1)..=used_c2 {
                    sheet.remove(r, cc);
                    sheet.styles.remove(&CellRef::new(r, cc));
                    sheet.notes.remove(&CellRef::new(r, cc));
                    sheet.rich.remove(&(r, cc));
                    sheet.hyperlinks.remove(&(r, cc));
                }
            }
            for m in sheet.merges.iter_mut() {
                if m.0 >= r1 && m.2 <= r2 && m.1 >= c1 {
                    m.1 = m.1.saturating_sub(n);
                    m.3 = m.3.saturating_sub(n);
                }
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
}
