//! Local and host-confirmed partial cell insertion and deletion.

use super::SheetApp;
use sheetcore::{cell_shift_patch::CellShiftPatch, sync::CellShiftPhase};
use std::sync::Arc;

#[derive(Default)]
pub(super) struct ShiftIntro {
    pub(super) overlay: Option<(u64, Arc<sheetcore::cell_shift_patch::CellShiftOverlay>)>,
    pub(super) receipt: Option<(u64, CellShiftPhase, Arc<CellShiftPatch>)>,
    pub(super) applied_seq: u64,
    deferred_pastes: Vec<DeferredPasteWrites>,
}

struct DeferredPasteWrites {
    room: String,
    seq: u64,
    first_exclusive: u64,
    through: u64,
}

impl SheetApp {
    /// Unacknowledged writes can survive a dropped socket in the rejoin outbox.
    /// Remember which ones came from an asynchronous paste tied to a coordinate frame.
    pub(super) fn track_deferred_paste(&mut self, first_exclusive: u64, through: u64) {
        let Some(net) = self.net.as_ref().filter(|net| !net.hosting) else {
            return;
        };
        if through <= first_exclusive {
            return;
        }
        let room = net.connected_room_id();
        let acked = net.acked_op_ts();
        let seq = self.shift_intro.applied_seq;
        let pastes = &mut self.shift_intro.deferred_pastes;
        pastes.retain(|p| p.room != room || p.through > acked);
        if through > acked {
            pastes.push(DeferredPasteWrites {
                room,
                seq,
                first_exclusive,
                through,
            });
        }
    }

    pub(super) fn discard_stale_deferred_pastes(&mut self, seq: u64) -> usize {
        let Some(net) = &self.net else { return 0 };
        let room = net.connected_room_id();
        let pastes = &mut self.shift_intro.deferred_pastes;
        let stale: Vec<_> = pastes
            .iter()
            .filter(|p| p.room != room || p.seq < seq)
            .map(|p| (p.first_exclusive, p.through))
            .collect();
        let acked = net.acked_op_ts();
        pastes.retain(|p| p.room == room && p.seq >= seq && p.through > acked);
        let Some((uid, ops)) = &mut self.rejoin_outbox else {
            return 0;
        };
        if *uid != self.doc_uid || stale.is_empty() {
            return 0;
        }
        let before = ops.len();
        ops.retain(|op| {
            !stale
                .iter()
                .any(|&(prior, last)| prior < op.ts && op.ts <= last)
        });
        before - ops.len()
    }

    /// 插入/删除单元格（down_or_up: true=插入；horizontal: true=右移/左移）
    pub(super) fn shift_cells(&mut self, insert: bool, horizontal: bool) {
        if self.edit_blocked() {
            return;
        }
        self.commit_edit(None);
        let rect = self.sel_rect();
        // 整行/整列选择时直接走稀疏结构变更，避免展开千万格坐标卡死
        if rect.1 == 0 && rect.3 >= 16_383 {
            if insert {
                let n = rect.2 - rect.0 + 1;
                self.structural(sheetcore::structural::Structural::InsertRows { at: rect.0, n });
            } else {
                self.delete_rows_range(rect.0, rect.2);
            }
            return;
        }
        if rect.0 == 0 && rect.2 >= 1_048_575 {
            let n = rect.3 - rect.1 + 1;
            self.structural(if insert {
                sheetcore::structural::Structural::InsertCols { at: rect.1, n }
            } else {
                sheetcore::structural::Structural::DeleteCols { at: rect.1, n }
            });
            return;
        }
        let si = self.sheet_idx;
        let direction = if horizontal {
            sheetcore::cells::Shift::Horizontal
        } else {
            sheetcore::cells::Shift::Vertical
        };
        let op = sheetcore::structural::CellShift::new(rect, direction, insert);
        let sh = self.wb.sheet(si);
        if op.would_overflow(sh) {
            self.status = "插入/删除区域超出工作表边界，无法移动单元格".into();
            return;
        }

        let (r1, c1, last_row, last_col) =
            moved_extent(sh, rect, horizontal, if insert { op.count() } else { 0 });
        let area = u64::from(last_row - r1 + 1) * u64::from(last_col - c1 + 1);
        if area > 200_000 {
            self.status = "插入/删除单元格影响范围过大，请改用整行或整列插入/删除".into();
            return;
        }
        let mut keys = sheetcore::undo::rect_keys(r1, c1, last_row, last_col);
        keys.extend(op.affected_formula_positions(sh, &sh.name));
        keys.sort_unstable();
        keys.dedup();
        let label = if insert {
            "插入单元格"
        } else {
            "删除单元格"
        };
        if !self.write_preflight_on(si, &keys, label) {
            return;
        }
        if self.net.is_some() {
            self.request_room_shift(si, op);
            return;
        }
        let before = sheetcore::undo::snapshot_cell_shift_if_metadata(&self.wb, si, &op);
        let mut affected = vec![(si, keys)];
        if let Some(snapshot) = &before {
            affected.extend(snapshot.foreign_formula_positions());
        } else {
            for (idx, other) in self.wb.sheets.iter().enumerate() {
                if idx == si {
                    continue;
                }
                let keys = op.affected_formula_positions(other, &self.wb.sheet(si).name);
                if !keys.is_empty() {
                    affected.push((idx, keys));
                }
            }
        }
        let captured: Vec<_> = affected
            .into_iter()
            .map(|(idx, keys)| {
                (
                    idx,
                    sheetcore::undo::capture(self.wb.sheet(idx), idx, &keys),
                )
            })
            .collect();
        if !op.apply(&mut self.wb, si) {
            return;
        }
        let deltas: Vec<_> = captured
            .into_iter()
            .flat_map(|(idx, cells)| sheetcore::undo::finish(cells, self.wb.sheet(idx)))
            .collect();
        let focus = (si, self.sel.0, self.sel.1);
        let detached_source = before
            .as_ref()
            .is_some_and(|snapshot| snapshot.detached_source(&self.wb));
        let structure = before
            .filter(|s| s.cell_shift_changed_metadata(&self.wb))
            .map(|snapshot| {
                let after = snapshot.capture_after(&self.wb);
                (snapshot, after)
            });
        self.recalc_all_and_deps_adaptive();
        for group in deltas.chunk_by(|a, b| a.sheet == b.sheet) {
            self.record_revision(label, group);
        }
        if let Some((before, after)) = structure {
            if deltas.is_empty() {
                self.record_revision(label, &[]);
            }
            self.undo.push_structure(label, before, after, focus);
        } else if !deltas.is_empty() {
            self.undo
                .push_cell_shift(label, deltas, focus, &self.wb.sheets);
        }
        self.tasks.last_edit = Some(std::time::Instant::now());
        self.doc_gen = self.doc_gen.wrapping_add(1);
        self.wb.dirty = true;
        self.status = if insert {
            if horizontal {
                "已插入单元格（右侧右移）"
            } else {
                "已插入单元格（下方下移）"
            }
            .into()
        } else if horizontal {
            "已删除单元格（左侧左移）".into()
        } else {
            "已删除单元格（上方上移）".into()
        };
        if detached_source {
            self.status
                .push_str("；部分数据源的绑定区域已失效，请重新选择区域并导入");
        }
    }

    fn request_room_shift(&mut self, sheet: usize, op: sheetcore::structural::CellShift) {
        let Some(patch) = CellShiftPatch::prepare(&self.wb, sheet, op) else {
            self.status = "单元格位移范围无效，未执行操作".into();
            return;
        };
        if let Err(reason) = self.check_room_cell_shift(
            self.net.as_ref().unwrap().site,
            CellShiftPhase::Forward,
            &patch,
        ) {
            self.status = reason;
            return;
        }
        let Ok(id) = getrandom::u64() else {
            self.status = "无法创建本次协作操作，请重试".into();
            return;
        };
        if let Err(reason) = self.net.as_ref().unwrap().request_cell_shift(
            id,
            CellShiftPhase::Forward,
            Arc::new(patch),
        ) {
            self.status = reason;
        } else {
            self.status = "正在等待宿主确认单元格位移".into();
        }
    }

    pub(super) fn request_room_shift_replay(
        &mut self,
        id: u64,
        patch: Arc<CellShiftPatch>,
        redo: bool,
    ) -> bool {
        if !self
            .undo
            .room_shift_history_matches(&self.wb, redo, self.net.is_none())
        {
            self.status = "关联工作表或单元格已更改，已保留历史记录".into();
            return false;
        }
        let phase = if redo {
            CellShiftPhase::Redo
        } else {
            CellShiftPhase::Undo
        };
        let Some(net) = &self.net else {
            if !patch.matches_replica(&self.wb, redo) || !patch.apply_committed(&mut self.wb, redo)
            {
                self.status = "关联单元格、批注或数据源已更改，已保留历史记录".into();
                return false;
            }
            self.finish_shift_edit();
            self.finish_room_shift_history(id, phase, patch);
            self.status = if redo {
                "已重做单元格位移"
            } else {
                "已撤销单元格位移"
            }
            .into();
            return true;
        };
        let site = net.site;
        let result = self
            .check_room_cell_shift(site, phase, &patch)
            .and_then(|()| {
                self.net
                    .as_ref()
                    .unwrap()
                    .request_cell_shift(id, phase, patch)
            });
        if let Err(reason) = result {
            self.status = reason;
            return false;
        }
        self.status = "正在等待宿主确认单元格位移".into();
        true
    }

    pub(super) fn validate_room_shift_request(
        &self,
        site: u32,
        phase: CellShiftPhase,
        patch: Arc<CellShiftPatch>,
    ) -> Result<Arc<CellShiftPatch>, String> {
        let canonical = if matches!(phase, CellShiftPhase::Forward) {
            Arc::new(
                patch
                    .canonical_forward(&self.wb)
                    .ok_or("目标区域已更改，请重新执行插删操作")?,
            )
        } else {
            patch
        };
        self.check_room_cell_shift(site, phase, &canonical)?;
        Ok(canonical)
    }

    fn check_room_cell_shift(
        &self,
        site: u32,
        phase: CellShiftPhase,
        patch: &CellShiftPatch,
    ) -> Result<(), String> {
        let forward = !matches!(phase, CellShiftPhase::Undo);
        let sheet = patch.target_sheet();
        let target = self
            .wb
            .sheets
            .get(sheet)
            .ok_or("目标工作表已更改，已保留历史记录")?;
        if self.deps_pending {
            return Err("工作簿正在建立依赖关系，请稍后重试".into());
        }
        if !matches!(phase, CellShiftPhase::Forward) {
            let matches = if self.net.as_ref().is_some_and(|net| net.hosting) {
                patch.matches(&self.wb, forward)
            } else {
                patch.matches_replica(&self.wb, forward)
            };
            if !matches {
                return Err("关联单元格、批注或数据源已更改，已保留历史记录".into());
            }
        }
        let (r1, c1, r2, c2) = patch.rect;
        let distance = if patch.insert == forward {
            if patch.horizontal {
                c2 - c1 + 1
            } else {
                r2 - r1 + 1
            }
        } else {
            0
        };
        let rect = moved_extent(target, patch.rect, patch.horizontal, distance);
        if Self::rect_has_protected_locked_cell(&self.wb, target, rect) {
            return Err("位移区域包含受保护的单元格".into());
        }
        if self.locks.iter().any(|((si, r, c), _)| {
            si == sheet
                && (rect.0..=rect.2).contains(&r)
                && (rect.1..=rect.3).contains(&c)
                && self.locks.is_locked_by_other(si, r, c, site)
        }) {
            return Err("位移区域正由其他成员编辑，已保留历史记录".into());
        }
        for cell in &patch.cells {
            let (si, r, c) = (cell.sheet, cell.row, cell.col);
            if (self.wb.sheet(si).protection.is_some() && self.cell_locked_on(si, r, c))
                || self.locks.is_locked_by_other(si, r, c, site)
            {
                return Err("关联单元格已锁定，已保留历史记录".into());
            }
            if !matches!(phase, CellShiftPhase::Forward)
                && self
                    .peer
                    .as_ref()
                    .and_then(|peer| peer.cells.get(&(si, r, c)))
                    .is_some_and(|op| op.site != site)
            {
                return Err("关联单元格已由其他成员修改，已保留历史记录".into());
            }
        }
        Ok(())
    }

    pub(super) fn commit_room_cell_shift(
        &mut self,
        site: u32,
        id: u64,
        phase: CellShiftPhase,
        patch: Arc<CellShiftPatch>,
        first_ts: u64,
    ) -> bool {
        let forward = !matches!(phase, CellShiftPhase::Undo);
        if !patch.can_install(&self.wb) {
            self.resync_room_cell_shift();
            return false;
        }
        let local = self.net.as_ref().is_some_and(|n| n.site == site);
        let sheet = patch.target_sheet();
        let label = if patch.insert {
            "插入单元格"
        } else {
            "删除单元格"
        };
        let mut captured = Vec::new();
        if local && matches!(phase, CellShiftPhase::Forward) {
            for group in patch.cells.chunk_by(|a, b| a.sheet == b.sheet) {
                let si = group[0].sheet;
                let keys: Vec<_> = group.iter().map(|c| (c.row, c.col)).collect();
                captured.push((si, sheetcore::undo::capture(self.wb.sheet(si), si, &keys)));
            }
        }
        let ops = crate::net::cell_shift_ops(site, first_ts, &patch, phase);
        let mut newer_on_target = std::collections::HashSet::new();
        for (cell, op) in patch.cells.iter().zip(ops) {
            let value = sheetcore::Cell {
                value: (&op.value).into(),
                formula: op.formula.clone(),
            };
            if self.peer.as_mut().is_none_or(|p| p.apply(op)) {
                let sheet = self.wb.sheet_mut(cell.sheet);
                if !sheet
                    .get(cell.row, cell.col)
                    .map_or_else(|| value.is_blank(), |old| old.same_input(&value))
                {
                    sheet.set(cell.row, cell.col, value);
                }
                cell.apply_style(&mut self.wb, forward);
            } else if cell.sheet == sheet {
                newer_on_target.insert((cell.row, cell.col));
            }
        }
        patch.replay_metadata_for_cells(&mut self.wb, forward, |row, col| {
            !newer_on_target.contains(&(row, col))
        });
        self.finish_shift_edit();
        if local {
            if let Some(net) = &self.net {
                net.finish_structure();
            }
            if matches!(phase, CellShiftPhase::Forward) {
                for (si, before) in captured {
                    let deltas = sheetcore::undo::finish(before, self.wb.sheet(si));
                    if !deltas.is_empty() {
                        self.record_revision(label, &deltas);
                    }
                }
            }
            self.finish_room_shift_history(id, phase, patch);
        } else {
            self.record_remote_note_revision(site, "远端单元格位移", sheet, label.into());
        }
        self.status = match phase {
            CellShiftPhase::Forward => format!("已{label}"),
            CellShiftPhase::Undo => format!("已撤销{label}"),
            CellShiftPhase::Redo => format!("已重做{label}"),
        };
        true
    }

    pub(super) fn resync_room_cell_shift(&mut self) {
        self.status = "工作表与宿主快照不一致，正在重新同步".into();
        if let Some(net) = &self.net {
            net.request_snapshot_resync();
        }
    }

    fn finish_shift_edit(&mut self) {
        self.recalc_all_and_deps_adaptive();
        self.wb.dirty = true;
        self.doc_gen = self.doc_gen.wrapping_add(1);
        self.cf_stats_gen = u64::MAX;
        self.invalid_gen = u64::MAX;
        self.tasks.last_edit = Some(std::time::Instant::now());
    }

    pub(super) fn discard_room_peer(&mut self) {
        if let Some(peer) = self.peer.take() {
            self.undo.preserve_room_shift_ownership(&peer);
        }
    }

    pub(super) fn finish_room_shift_history(
        &mut self,
        id: u64,
        phase: CellShiftPhase,
        patch: Arc<CellShiftPatch>,
    ) {
        if matches!(phase, CellShiftPhase::Forward) {
            if patch.can_install(&self.wb) {
                self.undo.push_room_cell_shift(id, patch, &self.wb);
            }
            return;
        }
        let redo = matches!(phase, CellShiftPhase::Redo);
        let entry = if redo {
            self.undo.top_redo_id()
        } else {
            self.undo.top_undo_id()
        };
        if !self.undo.confirm_room_cell_shift(id, redo) {
            return;
        }
        if let Some(entry) = entry {
            if redo {
                self.audit_on_redo(entry);
            } else {
                self.audit_on_undo(entry);
            }
        }
        if patch.can_install(&self.wb) {
            if self.sheet_idx != patch.target_sheet() {
                self.switch_sheet_view(patch.target_sheet());
            }
            self.extra_sel.clear();
            self.sel = (patch.rect.0, patch.rect.1);
            self.sel_end = self.sel;
            self.nav_rect = None;
            self.ensure_visible();
        }
    }
}

fn moved_extent(
    sh: &sheetcore::Sheet,
    rect: (u32, u32, u32, u32),
    horizontal: bool,
    distance: u32,
) -> (u32, u32, u32, u32) {
    // Only content and metadata in the selected band contribute to its movable extent.
    let (r1, c1, r2, c2) = rect;
    let ray = if horizontal {
        (r1, c1, r2, sheetcore::sync::MAX_COLS - 1)
    } else {
        (r1, c1, sheetcore::sync::MAX_ROWS - 1, c2)
    };
    let mut last = if horizontal { c2 } else { r2 };
    let mut include = |r, c| {
        if (ray.0..=ray.2).contains(&r) && (ray.1..=ray.3).contains(&c) {
            last = last.max((if horizontal { c } else { r }) + distance);
        }
    };
    for (key, _) in sh.iter_rect_views(ray) {
        include(key.row, key.col);
    }
    for (key, _) in sh.styles.iter_rect(ray) {
        include(key.row, key.col);
    }
    for key in sh.notes.keys().chain(sh.threads.keys()) {
        include(key.row, key.col);
    }
    for &(r, c) in sh.rich.keys().chain(sh.hyperlinks.keys()) {
        include(r, c);
    }
    for &(top, left, bottom, right) in &sh.merges {
        if (horizontal && r1 <= top && bottom <= r2 && left >= c1)
            || (!horizontal && c1 <= left && right <= c2 && top >= r1)
        {
            include(bottom, right);
        }
    }
    if horizontal {
        (r1, c1, r2, last)
    } else {
        (r1, c1, last, c2)
    }
}
