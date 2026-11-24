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
    if let Some((r2, c2)) = sheet.styles.max_coords() {
        bump(r2, c2);
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
    if let (Some(mr), Some(mc)) = (sheet.max_merge_row(), sheet.max_merge_col()) {
        bump(mr, mc);
    }
    max_r.zip(max_c)
}

fn shift_ray_metadata(
    sheet: &mut Sheet,
    ray: (u32, u32, u32, u32),
    map_coord: impl Fn(u32, u32) -> Option<(u32, u32)>,
) {
    let (r1, c1, r2, c2) = ray;
    if r1 > r2 || c1 > c2 {
        return;
    }

    if !sheet.notes.is_empty() {
        let keys: Vec<CellRef> = sheet
            .notes
            .keys()
            .filter(|k| k.row >= r1 && k.row <= r2 && k.col >= c1 && k.col <= c2)
            .copied()
            .collect();
        if !keys.is_empty() {
            sheet.notes_dirty = true;
            let mut moved = Vec::with_capacity(keys.len());
            for k in keys {
                if let Some(mut note) = sheet.notes.remove(&k)
                    && let Some((nr, nc)) = map_coord(k.row, k.col)
                {
                    note.r = nr;
                    note.c = nc;
                    moved.push((CellRef::new(nr, nc), note));
                }
            }
            for (nk, note) in moved {
                sheet.notes.insert(nk, note);
            }
        }
    }

    if !sheet.rich.is_empty() {
        let keys: Vec<(u32, u32)> = sheet
            .rich
            .keys()
            .filter(|&&(r, c)| r >= r1 && r <= r2 && c >= c1 && c <= c2)
            .copied()
            .collect();
        if !keys.is_empty() {
            sheet.rich_dirty = true;
            let mut moved = Vec::with_capacity(keys.len());
            for pos in keys {
                if let Some(runs) = sheet.rich.remove(&pos)
                    && let Some(dest) = map_coord(pos.0, pos.1)
                {
                    moved.push((dest, runs));
                }
            }
            for (dest, runs) in moved {
                sheet.rich.insert(dest, runs);
            }
        }
    }

    if !sheet.hyperlinks.is_empty() {
        let keys: Vec<(u32, u32)> = sheet
            .hyperlinks
            .keys()
            .filter(|&&(r, c)| r >= r1 && r <= r2 && c >= c1 && c <= c2)
            .copied()
            .collect();
        if !keys.is_empty() {
            sheet.hyperlinks_dirty = true;
            let mut moved = Vec::with_capacity(keys.len());
            for pos in keys {
                if let Some(link) = sheet.hyperlinks.remove(&pos)
                    && let Some(dest) = map_coord(pos.0, pos.1)
                {
                    moved.push((dest, link));
                }
            }
            for (dest, link) in moved {
                sheet.hyperlinks.insert(dest, link);
            }
        }
    }
}

fn shift_sparse_ray(
    sheet: &mut Sheet,
    ray: (u32, u32, u32, u32),
    map_coord: impl Fn(u32, u32) -> Option<(u32, u32)>,
) {
    let (r1, c1, r2, c2) = ray;
    if r1 > r2 || c1 > c2 {
        return;
    }
    let cells: Vec<(CellRef, crate::model::Cell)> = sheet
        .iter_rect_views(ray)
        .map(|(k, c)| (k, c.to_cell()))
        .collect();
    for (k, _) in &cells {
        sheet.remove(k.row, k.col);
    }
    for (k, cell) in cells {
        if let Some((nr, nc)) = map_coord(k.row, k.col) {
            sheet.set(nr, nc, cell);
        }
    }

    let styles: Vec<(CellRef, u16)> = sheet.styles.iter_rect(ray).collect();
    if !styles.is_empty() {
        for &(k, _) in &styles {
            if sheet.styles.remove(&k).is_some() {
                sheet.style_edits.insert(k);
            }
        }
        sheet.styles_dirty = true;
        for (k, st) in styles {
            if let Some((nr, nc)) = map_coord(k.row, k.col) {
                sheet.set_style(nr, nc, st);
            }
        }
    }

    shift_ray_metadata(sheet, ray, map_coord);
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
            if used_r2 >= r1 {
                sheet.shift_col_band_rows(c1, c2_eff, r1, n, false);
                shift_ray_metadata(sheet, (r1, c1, used_r2, c2_eff), |r, c| Some((r + n, c)));
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
            if used_c2 >= c1 {
                shift_sparse_ray(sheet, (r1, c1, r2_eff, used_c2), |r, c| Some((r, c + n)));
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
            sheet.shift_col_band_rows(c1, c2_eff, r1, n, true);
            shift_ray_metadata(sheet, (r1, c1, used_r2, c2_eff), |r, c| {
                (r >= r1 + n).then(|| (r - n, c))
            });
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
            shift_sparse_ray(sheet, (r1, c1, r2_eff, used_c2), |r, c| {
                (c >= c1 + n).then(|| (r, c - n))
            });
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
