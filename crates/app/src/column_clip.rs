//! Internal whole-column paste. Coordinates remain relative to the full sheet,
//! including empty source rows below its used range.
use super::SheetApp;
use sheetcore::sync::{MAX_COLS as COLS, MAX_ROWS as ROWS};
use sheetcore::{Sheet, Workbook, style::StyleTable, undo::CellDelta};
use std::collections::HashSet;

fn intersects_array_or_merge(sheet: &Sheet, first: u32, last: u32) -> bool {
    sheet
        .merges
        .iter()
        .any(|&(_, c, _, d)| c <= last && first <= d)
        || intersects_array(sheet, first, last)
}

pub(super) fn intersects_array(sheet: &Sheet, first: u32, last: u32) -> bool {
    sheet
        .arrays
        .iter()
        .any(|(_, (_, c, _, d))| *c <= last && first <= *d)
        || sheet
            .spilled
            .iter()
            .any(|k| (first..=last).contains(&k.col))
        || sheet.spill_shapes.iter().any(|(k, (_, width))| {
            k.col <= last && first <= k.col.saturating_add(width.saturating_sub(1))
        })
}

/// Count locked cells in an arbitrary rectangle `rect = (r1, c1, r2, c2)` in O(explicit styles)
/// rather than O(R * C). Explicit cell styles override row styles, which override column styles,
/// which fall back to style 0.
pub(crate) fn locked_count_in_rect(
    wb: &Workbook,
    sheet: &Sheet,
    rect: (u32, u32, u32, u32),
) -> u64 {
    let (r1, c1) = (rect.0.min(rect.2).min(ROWS - 1), rect.1.min(rect.3).min(COLS - 1));
    let (r2, c2) = (rect.0.max(rect.2).min(ROWS - 1), rect.1.max(rect.3).min(COLS - 1));
    let total_rows = u64::from(r2 - r1 + 1);
    let total_cols = u64::from(c2 - c1 + 1);
    let area = total_rows * total_cols;
    let is_locked = |id: u16| wb.styles.get(id).locked;
    if area <= 256 {
        let mut count = 0u64;
        for r in r1..=r2 {
            for c in c1..=c2 {
                if is_locked(sheet.style_of(r, c)) {
                    count += 1;
                }
            }
        }
        return count;
    }
    let mut row_styled = 0i64;
    let mut row_locked = 0i64;
    for (&r, &id) in &sheet.row_styles {
        if r >= r1 && r <= r2 {
            row_styled += 1;
            if is_locked(id) {
                row_locked += 1;
            }
        }
    }
    let unhandled_rows = total_rows as i64 - row_styled;
    let mut col_styled = 0i64;
    let mut col_locked = 0i64;
    for (&c, &id) in &sheet.col_styles {
        if c >= c1 && c <= c2 {
            col_styled += 1;
            if is_locked(id) {
                col_locked += 1;
            }
        }
    }
    let unhandled_cols = total_cols as i64 - col_styled;
    let effective_locked_cols =
        col_locked + if is_locked(0) { unhandled_cols } else { 0 };
    let mut locked =
        row_locked * (total_cols as i64) + unhandled_rows * effective_locked_cols;
    for (key, &explicit) in sheet.styles.iter() {
        if key.row >= r1 && key.row <= r2 && key.col >= c1 && key.col <= c2 {
            let base = sheet
                .row_styles
                .get(&key.row)
                .or_else(|| sheet.col_styles.get(&key.col))
                .copied()
                .unwrap_or(0);
            locked += i64::from(is_locked(explicit)) - i64::from(is_locked(base));
        }
    }
    locked.max(0) as u64
}

/// Sparse rectangle check: returns true if ANY cell in `rect` is locked on a protected sheet.
pub(crate) fn rect_has_any_locked(
    wb: &Workbook,
    sheet: &Sheet,
    rect: (u32, u32, u32, u32),
) -> bool {
    if sheet.protection.is_none() {
        return false;
    }
    locked_count_in_rect(wb, sheet, rect) > 0
}

/// Sparse rectangle check: returns true if EVERY cell in `rect` is locked on a protected sheet.
pub(crate) fn rect_all_locked(
    wb: &Workbook,
    sheet: &Sheet,
    rect: (u32, u32, u32, u32),
) -> bool {
    if sheet.protection.is_none() {
        return false;
    }
    let (r1, c1) = (rect.0.min(rect.2).min(ROWS - 1), rect.1.min(rect.3).min(COLS - 1));
    let (r2, c2) = (rect.0.max(rect.2).min(ROWS - 1), rect.1.max(rect.3).min(COLS - 1));
    let area = u64::from(r2 - r1 + 1) * u64::from(c2 - c1 + 1);
    locked_count_in_rect(wb, sheet, rect) == area
}

/// Count locked rows rather than assuming unoccupied cells in a protected
/// selection are writable. Explicit cells override row and then column defaults.
fn has_locked_cells(wb: &Workbook, sheet: &Sheet, first: u32, last: u32) -> bool {
    rect_has_any_locked(wb, sheet, (0, first, ROWS - 1, last))
}

fn matching_style(from: &StyleTable, to: &StyleTable, id: u16) -> bool {
    usize::from(id) < from.len()
        && usize::from(id) < to.len()
        && from.get(id) == to.get(id)
        && from.theme_of(id) == to.theme_of(id)
        && from.pattern_of(id) == to.pattern_of(id)
        && from.diag_of(id) == to.diag_of(id)
        && from.quote_of(id) == to.quote_of(id)
}

/// CellDelta stores effective formats. If a write or its undo would turn an
/// inherited format into an explicit one, decline the operation before mutation.
fn check_style_change(
    source: &Sheet,
    target: &Sheet,
    row: u32,
    sc: u32,
    dc: u32,
    before: u16,
    after: u16,
) -> bool {
    let inherited = target
        .row_styles
        .get(&row)
        .or_else(|| target.col_styles.get(&dc))
        .copied();
    let representable = |id| (id != 0 || inherited.is_some()).then_some(id);
    let before_explicit = target.explicit_style_of(row, dc);
    let after_explicit = match source.explicit_style_of(row, sc) {
        Some(id) => Some(id),
        None if inherited.unwrap_or(0) == after => None,
        None => representable(after),
    };
    if before == after {
        return before_explicit == after_explicit;
    }
    representable(before) == before_explicit && representable(after) == after_explicit
}

fn append_explicit_rows(sheet: &Sheet, first: u32, last: u32, rows: &mut [Vec<u32>]) {
    for key in sheet.styles.keys() {
        if key.row < ROWS && (first..=last).contains(&key.col) {
            rows[(key.col - first) as usize].push(key.row);
        }
    }
}

fn plan(
    wb: &Workbook,
    index: usize,
    copied: &Workbook,
    first: u32,
    last: u32,
    dest: u32,
    values_only: bool,
) -> Result<Vec<CellDelta>, &'static str> {
    let source = copied.sheet(0);
    let target = wb.sheet(index);
    let width = last - first + 1;
    let dest_last = dest + width - 1;
    if intersects_array_or_merge(target, dest, dest_last)
        || (!values_only && intersects_array_or_merge(source, first, last))
    {
        return Err("整列区域含合并单元格或数组公式，请改选具体范围");
    }
    if !source.hidden_rows.is_empty() {
        return Err("整列区域含筛选隐藏行，请改选具体范围复制可见行");
    }
    let mut style_rows = vec![Vec::new(); width as usize];
    let mut row_defaults = Vec::new();
    let mut checked_styles = HashSet::new();
    if !values_only {
        for offset in 0..width {
            let default = source
                .col_styles
                .get(&(first + offset))
                .copied()
                .unwrap_or(0);
            if source.col_styles.get(&(first + offset)) != target.col_styles.get(&(dest + offset))
                || (checked_styles.insert(default)
                    && !matching_style(&copied.styles, &wb.styles, default))
            {
                return Err(
                    "源列和目标列的整列默认格式不同，整列粘贴无法保留撤销，请改选具体范围或仅粘贴值",
                );
            }
        }
        append_explicit_rows(source, first, last, &mut style_rows);
        append_explicit_rows(target, dest, dest_last, &mut style_rows);
        for &r in source.row_styles.keys().chain(target.row_styles.keys()) {
            if r < ROWS {
                let from = source.row_styles.get(&r);
                if let Some(&id) = from {
                    if checked_styles.insert(id) && !matching_style(&copied.styles, &wb.styles, id)
                    {
                        return Err("源与目标工作簿的行样式不同，请仅粘贴值或改选具体范围");
                    }
                }
                if from != target.row_styles.get(&r) {
                    row_defaults.push(r);
                }
            }
        }
        row_defaults.sort_unstable();
        row_defaults.dedup();
    }
    let mut deltas = Vec::new();
    let col_shift = i64::from(dest) - i64::from(first);
    for (i, mut rows) in style_rows.into_iter().enumerate() {
        let sc = first + i as u32;
        let dc = dest + i as u32;
        rows.extend(
            source
                .iter_rect((0, sc, ROWS - 1, sc))
                .map(|(key, _)| key.row),
        );
        rows.extend(
            target
                .iter_rect((0, dc, ROWS - 1, dc))
                .map(|(key, _)| key.row),
        );
        rows.extend_from_slice(&row_defaults);
        rows.sort_unstable();
        rows.dedup();
        for row in rows {
            let before = target.get(row, dc);
            let new_cell = source.get(row, sc);
            let before_style = target.style_of(row, dc);
            let after_style = if values_only {
                before_style
            } else {
                source.style_of(row, sc)
            };
            if !values_only {
                if checked_styles.insert(after_style)
                    && !matching_style(&copied.styles, &wb.styles, after_style)
                {
                    return Err("源与目标工作簿的样式表不同，请仅粘贴值或改选具体范围");
                }
                if !check_style_change(source, target, row, sc, dc, before_style, after_style) {
                    return Err("整列粘贴会改变继承格式且无法完整撤销，请仅粘贴值或改选具体范围");
                }
            }
            if before == new_cell
                && before_style == after_style
                && new_cell.is_none_or(|cell| cell.formula.is_none())
            {
                continue;
            }
            let mut after = new_cell.cloned().unwrap_or_default();
            if values_only {
                after.formula = None;
            } else if col_shift != 0 {
                if let Some(f) = &after.formula {
                    after.formula =
                        Some(sheetcore::refs::translate(f, 0, col_shift, &target.name).into());
                }
            }
            let before = before.cloned().unwrap_or_default();
            if before == after && before_style == after_style {
                continue;
            }
            if deltas.len() >= sheetcore::undo::MAX_DENSE_RECT_KEYS {
                return Err("整列粘贴实际变更超过 50 万格，请分批粘贴");
            }
            deltas.push(CellDelta {
                sheet: index,
                row,
                col: dc,
                before,
                after,
                before_style,
                after_style,
            });
        }
    }
    Ok(deltas)
}

impl SheetApp {
    pub(super) fn paste_full_columns(
        &mut self,
        copied: &Workbook,
        first: u32,
        last: u32,
        values_only: bool,
    ) {
        let (r, dest) = self.sel;
        if r != 0 || first > last || last >= COLS || dest >= COLS || last - first >= COLS - dest {
            self.status = "整列粘贴须从首行开始，且目标列不能超出工作表边界".into();
            return;
        }
        let si = self.sheet_idx;
        if !self.write_preflight_on(si, &[], "整列粘贴") {
            return;
        }
        let dest_last = dest + (last - first);
        let rect = (0, dest, ROWS - 1, dest_last);
        let conflict = self
            .net
            .as_ref()
            .and_then(|net| net.host_lock_conflict_rect(si, rect))
            .map(|(_, name, row, col)| (name, row, col))
            .or_else(|| {
                let site = self.net.as_ref().map_or(0, |net| net.site);
                self.locks
                    .conflicts_rect(si, 0, dest, ROWS - 1, dest_last, site, self.lock_now())
                    .map(|((row, col), holder)| (holder.name.clone(), row, col))
            });
        if let Some((name, row, col)) = conflict {
            self.status = format!(
                "{name} 正在编辑 {}，不能整列粘贴",
                sheetcore::cell_name(row, col)
            );
            return;
        }
        if has_locked_cells(&self.wb, self.wb.sheet(si), dest, dest_last) {
            self.status = "工作表受保护：目标列含锁定单元格，不能粘贴".into();
            return;
        }
        let changes = match plan(&self.wb, si, copied, first, last, dest, values_only) {
            Ok(changes) => changes,
            Err(message) => {
                self.status = message.into();
                return;
            }
        };
        self.last_paste = None;
        let count = changes.len();
        if count > 0 {
            let mut value_keys = Vec::new();
            let sheet = self.wb.sheet_mut(si);
            for delta in &changes {
                if delta.before != delta.after {
                    sheet.set(delta.row, delta.col, delta.after.clone());
                    value_keys.push((si, delta.row, delta.col));
                }
                if delta.before_style != delta.after_style {
                    sheet.set_style(delta.row, delta.col, delta.after_style);
                }
            }
            self.wb.dirty = true;
            self.doc_gen = self.doc_gen.wrapping_add(1);
            self.tasks.last_edit = Some(std::time::Instant::now());
            self.fast_recalc_cells(&value_keys);
            self.broadcast_sheet_deltas(si, &changes);
            self.record_deltas("整列粘贴", changes);
        }
        self.status = format!("已粘贴 {} 整列，更新 {count} 个单元格", last - first + 1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{net, test_env::TestEnv};
    use eframe::egui;
    use sheetcore::{
        Cell, Value,
        sync::{Peer, ValueWire},
    };

    fn num(n: f64) -> Cell {
        Cell {
            value: Value::Number(n),
            formula: None,
        }
    }

    fn connect(app: &mut SheetApp) -> net::Net {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let host = net::host_on(
            listener,
            88,
            "host".into(),
            "test".into(),
            "test-only".into(),
        )
        .unwrap();
        let remote = net::join(
            89,
            "peer".into(),
            "test".into(),
            &addr.to_string(),
            "test-only".into(),
        )
        .unwrap();
        wait_event(&remote, |e| matches!(e, net::NetEvent::SnapshotDone));
        app.net = Some(host);
        app.peer = Some(Peer::new(88));
        app.undo.set_site(88);
        remote
    }

    fn wait_event(remote: &net::Net, matches: impl Fn(&net::NetEvent) -> bool) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            let event = remote
                .events
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .expect("missing collaboration message");
            if matches(&event) {
                return;
            }
        }
    }

    fn wait_style(remote: &net::Net, sheet: usize, row: u32, col: u32, bold: bool) {
        wait_event(remote, |event| {
            matches!(event, net::NetEvent::Styles { items, .. }
            if items.iter().any(|(s, r, c, style)| (*s, *r, *c, style.bold) == (sheet, row, col, bold)))
        });
    }

    #[test]
    fn full_columns_keep_snapshot_clear_tail_and_restore_remote_undo() {
        let _env = TestEnv::new("whole-columns");
        let ctx = egui::Context::default();
        let mut app = SheetApp::new_with_context(&ctx, None);
        let mut bold = app.wb.styles.get(0).clone();
        bold.bold = true;
        let id = app.wb.styles.intern(bold);
        app.wb.sheets.push(Sheet::new("Dest"));
        app.wb.sheets[0].set(0, 0, num(7.0));
        app.wb.sheets[0].set(
            1,
            1,
            Cell {
                value: Value::Number(7.0),
                formula: Some("=A1".into()),
            },
        );
        app.wb.sheets[0].set_style(900_000, 2, id);
        app.wb.sheets[1].set(ROWS - 1, 4, num(49.0));
        app.wb.sheets[1].set_style(ROWS - 1, 4, id);
        app.wb.sheets[1].set(ROWS - 1, 6, num(99.0));
        app.sel = (0, 0);
        app.sel_end = (ROWS - 1, 2);
        app.clip_copy(false, &ctx);
        assert!(
            app.clip.as_ref().is_some_and(|c| c.deferred.is_some()),
            "{}",
            app.status
        );
        app.wb.sheets[0].set(0, 0, num(8.0));
        app.sheet_idx = 1;
        app.sel = (0, 3);
        app.sel_end = app.sel;
        let remote = connect(&mut app);
        app.clip_paste(false, None);
        let dest = &app.wb.sheets[1];
        assert_eq!(dest.value(0, 3), Value::Number(7.0), "{}", app.status);
        assert_eq!(dest.get(1, 4).unwrap().formula.as_deref(), Some("=D1"));
        assert_eq!(dest.explicit_style_of(900_000, 5), Some(id));
        assert!(dest.get(ROWS - 1, 4).is_none());
        assert!(dest.explicit_style_of(ROWS - 1, 4).is_none());
        assert_eq!(dest.value(ROWS - 1, 6), Value::Number(99.0));
        let remote_tail = |app: &SheetApp| {
            app.peer.as_ref().unwrap().cells[&(1, ROWS - 1, 4)]
                .value
                .clone()
        };
        assert_eq!(remote_tail(&app), ValueWire::Empty);
        wait_style(&remote, 1, 900_000, 5, true);
        app.do_undo(false);
        assert_eq!(app.wb.sheets[1].value(ROWS - 1, 4), Value::Number(49.0));
        assert_eq!(remote_tail(&app), ValueWire::Number(49.0));
        assert_eq!(app.wb.sheets[1].explicit_style_of(ROWS - 1, 4), Some(id));
        assert!(app.wb.sheets[1].explicit_style_of(900_000, 5).is_none());
        wait_style(&remote, 1, 900_000, 5, false);
        assert_eq!(app.wb.sheets[0].value(0, 0), Value::Number(8.0));
        app.do_undo(true);
        assert_eq!(remote_tail(&app), ValueWire::Empty);
        wait_style(&remote, 1, 900_000, 5, true);
    }

    #[test]
    fn full_columns_preflight_includes_empty_tail_and_rejects_partial_writes() {
        let _env = TestEnv::new("whole-column-guards");
        let ctx = egui::Context::default();
        let mut app = SheetApp::new_with_context(&ctx, None);
        app.wb.sheets[0].set(0, 0, num(4.0));
        let source = app.wb.clone();
        app.sel = (0, 4);
        app.locks
            .request(0, ROWS - 1, 6, 42, "peer", app.lock_now());
        app.paste_full_columns(&source, 0, 2, false);
        assert!(app.status.contains("peer"));
        assert!(app.wb.sheets[0].get(0, 4).is_none());
        app.locks.release(0, ROWS - 1, 6, 42);
        app.wb.sheets[0].protection = Some(Default::default());
        app.paste_full_columns(&source, 0, 2, false);
        assert!(app.status.contains("受保护"));
        app.wb.sheets[0].protection = None;
        app.wb.sheets[0].merges.push((ROWS - 2, 6, ROWS - 1, 7));
        app.paste_full_columns(&source, 0, 2, true);
        assert!(app.status.contains("合并"));
        assert!(app.wb.sheets[0].get(0, 4).is_none());
        app.wb.sheets[0].merges.clear();
        app.sel = (0, COLS - 2);
        app.paste_full_columns(&source, 0, 2, false);
        assert!(app.status.contains("边界"));
        assert!(app.undo.undo_label().is_none());
        app.sel = (0, 1);
        app.sel_end = (ROWS - 1, 3);
        app.wb.sheets[0]
            .arrays
            .push((sheetcore::CellRef::new(0, 0), (0, 0, 4, 1)));
        app.clip_copy(false, &ctx);
        assert!(app.status.contains("数组公式"));
        assert!(app.clip.is_none());
    }

    #[test]
    fn full_columns_plan_row_defaults_and_different_column_defaults() {
        let _env = TestEnv::new("whole-column-inherited");
        let mut wb = Workbook::new_blank();
        let mut colored = wb.styles.get(0).clone();
        colored.fill = Some([3, 4, 5]);
        let id = wb.styles.intern(colored);
        wb.sheets.push(Sheet::new("Dest"));
        wb.sheets[0].set_row_style(ROWS - 1, id);
        wb.sheets[0].set(0, 0, num(6.0));
        let changes = plan(&wb, 1, &wb, 0, 2, 3, false).unwrap();
        assert_eq!(
            changes
                .iter()
                .filter(|d| d.row == ROWS - 1 && d.after_style == id)
                .count(),
            3
        );
        wb.sheets[1].set_col_style(5, id);
        assert!(plan(&wb, 1, &wb, 0, 2, 3, false).is_err());
        let values = plan(&wb, 1, &wb, 0, 2, 3, true).unwrap();
        assert_eq!(values.len(), 1);
        assert_eq!(values[0].after, num(6.0));
        wb.sheets[1].col_styles.clear();
        wb.sheets[0].set_style(ROWS - 1, 0, 0);
        assert!(plan(&wb, 1, &wb, 0, 2, 3, false).is_err());
    }

    #[test]
    fn full_columns_check_blank_inherited_styles_across_books_and_preserve_value_only_undo() {
        let _env = TestEnv::new("whole-columns-cross-book");
        let mut app = SheetApp::new_with_context(&egui::Context::default(), None);
        let mut copied = Workbook::new_blank();
        let mut blue = copied.styles.get(0).clone();
        blue.fill = Some([0, 0, 200]);
        let mut red = app.wb.styles.get(0).clone();
        red.fill = Some([200, 0, 0]);
        let blue = copied.styles.intern(blue);
        let red = app.wb.styles.intern(red);
        assert_eq!(blue, red);
        copied.sheets[0].set_row_style(ROWS - 1, blue);
        app.wb.sheets[0].set_row_style(ROWS - 1, red);
        copied.sheets[0].set(0, 0, num(7.0));
        app.sel = (0, 3);
        app.paste_full_columns(&copied, 0, 2, false);
        assert!(app.status.contains("行样式"));
        assert!(app.wb.sheets[0].get(0, 3).is_none());
        copied.sheets[0].set_col_style(0, blue);
        app.wb.sheets[0].set_col_style(3, red);
        app.paste_full_columns(&copied, 0, 2, false);
        assert!(app.status.contains("整列默认格式"));
        app.paste_full_columns(&copied, 0, 2, true);
        for state in [Some(7.0), None, Some(7.0)] {
            let sh = &app.wb.sheets[0];
            assert_eq!(sh.get(0, 3).cloned(), state.map(num));
            assert_eq!(sh.style_of(0, 3), red);
            assert_eq!(sh.explicit_style_of(0, 3), None);
            app.do_undo(state.is_none());
        }
    }

    #[test]
    fn sorting_and_dedup_send_format_only_cells_and_empty_tails_to_a_peer() {
        let _env = TestEnv::new("reorder-remote");
        let mut app = SheetApp::new_with_context(&egui::Context::default(), None);
        let mut bold = app.wb.styles.get(0).clone();
        bold.bold = true;
        let id = app.wb.styles.intern(bold);
        let sh = &mut app.wb.sheets[0];
        for (row, n) in [2.0, 1.0, 1.0].into_iter().enumerate() {
            sh.set(row as u32, 0, num(n));
            sh.set(row as u32, 1, num(10.0));
        }
        sh.set_style(0, 1, id);
        let remote = connect(&mut app);
        let rect = (0, 0, 2, 1);
        app.sort_region(rect, 0, true);
        wait_style(&remote, 0, 2, 1, true);
        assert_eq!(app.peer.as_ref().unwrap().cells[&(0, 2, 1)].site, 88);
        app.do_undo(false);
        wait_style(&remote, 0, 2, 1, false);
        assert_eq!(app.wb.sheets[0].style_of(0, 1), id);
        app.apply_dedup(rect, false, &[0]);
        wait_event(&remote, |e| {
            matches!(e, net::NetEvent::Ops(ops)
            if ops.iter().any(|op| (op.sheet, op.row, op.col) == (0, 2, 1) && op.value == ValueWire::Empty))
        });
        app.do_undo(false);
        assert_eq!(app.wb.sheets[0].get(2, 1).cloned(), Some(num(10.0)));
    }
}
