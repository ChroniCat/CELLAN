//! Local and host-confirmed partial cell insertion and deletion.

use super::SheetApp;
use crate::net::CoordEpoch;
use sheetcore::{
    cell_shift_patch::CellShiftPatch,
    refs::Axis,
    sync::CellShiftPhase,
    tx::{
        AxisPermutePatch, DefinedNameIntent, DefinedNamesPatch, RecalcScope, RoomTxPatch,
        TxEffects, TxError, TxIntent, TxPatch, WorkbookTx,
    },
};
use std::sync::Arc;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(super) struct OutboxCoord {
    pub(super) room_id: String,
    pub(super) epoch: CoordEpoch,
}

impl OutboxCoord {
    pub(super) fn matches_room(&self, cur_room: &str) -> bool {
        self.room_id.is_empty() || cur_room.is_empty() || self.room_id == cur_room
    }

    pub(super) fn is_stale(&self, cur_room: &str, cur_epoch: CoordEpoch) -> bool {
        let room_changed = !self.room_id.is_empty() && self.room_id != cur_room;
        room_changed
            || self.epoch.shift_seq != cur_epoch.shift_seq
            || self.epoch.struct_rev != cur_epoch.struct_rev
    }
}

#[derive(Clone, Debug)]
pub(super) struct RejoinOutboxMeta {
    pub(super) doc_uid: u64,
    pub(super) coord: OutboxCoord,
}

#[derive(Clone, Debug)]
pub(super) struct RejoinStyleEntry {
    pub(super) doc_uid: u64,
    pub(super) coord: OutboxCoord,
    pub(super) sheet: usize,
    pub(super) items: Vec<(u32, u32, sheetcore::style::CellStyle)>,
}

#[derive(Clone, Debug)]
pub(super) struct RejoinMergeEntry {
    pub(super) doc_uid: u64,
    pub(super) coord: OutboxCoord,
    pub(super) sheet: usize,
    pub(super) rect: (u32, u32, u32, u32),
    pub(super) on: bool,
}

pub(crate) const MAX_CANCELLED_OFFLINE_DRAFTS: usize = 10_000;

/// Local recovery record for unacknowledged offline edits that were safely
/// cancelled (rather than mis-replayed to shifted coordinates or a different room).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CancelledOfflineDraft {
    pub room_id: String,
    pub sheet: usize,
    pub sheet_name: String,
    pub row: u32,
    pub col: u32,
    pub value_display: String,
    pub formula: Option<String>,
    pub style_changed: bool,
    pub merge_rect: Option<(u32, u32, u32, u32)>,
    pub reason: String,
}

fn sanitize_tsv_field(s: &str) -> String {
    s.chars()
        .map(|ch| match ch {
            '\t' | '\r' | '\n' => ' ',
            other => other,
        })
        .collect()
}

#[derive(Default)]
pub(super) struct ShiftIntro {
    pub(super) overlay: Option<(u64, Arc<sheetcore::cell_shift_patch::CellShiftOverlay>)>,
    pub(super) receipt: Option<(u64, CellShiftPhase, Arc<CellShiftPatch>)>,
    pub(super) room_tx_receipt: Option<(u64, CellShiftPhase, Arc<RoomTxPatch>)>,
    pub(super) applied_seq: u64,
    pub(super) last_struct_rev: usize,
    pub(super) room_id: String,
    /// Local ops in `peer.cells` with `ts <= pre_shift_stale_ts` were created before
    /// a live `CellShiftCommitted` advanced `applied_seq`, so if still unacked they
    /// belong to a stale coordinate epoch and must not be replayed post-shift.
    pub(super) pre_shift_stale_ts: u64,
    /// Inclusive `(first, last)` logical timestamp ranges of live `CellShiftCommitted`
    /// ops authored by this site so they are never misclassified as unacked local edits.
    pub(super) committed_shift_ts_ranges: Vec<(u64, u64)>,
    /// Count of stale unconfirmed edits discarded across a delta->full resync cycle.
    pub(super) pending_cancelled_edits: usize,
    /// True when stale optimistic local writes were discarded before a delta snapshot
    /// settled, so `SnapshotDone` must trigger a full authoritative resync.
    pub(super) needs_delta_resync_for_stale: bool,
    deferred_pastes: Vec<DeferredPasteWrites>,
}

impl ShiftIntro {
    pub(super) fn begin_welcome(&mut self, room_id: String) {
        self.overlay = None;
        self.receipt = None;
        self.room_tx_receipt = None;
        self.applied_seq = 0;
        self.last_struct_rev = 0;
        self.room_id = room_id;
        self.clear_peer_shift_tracking();
    }

    pub(super) fn clear_peer_shift_tracking(&mut self) {
        self.pre_shift_stale_ts = 0;
        self.committed_shift_ts_ranges.clear();
    }

    pub(super) fn sync_struct_rev(&mut self, rev: usize) {
        self.last_struct_rev = rev;
    }

    pub(super) fn is_committed_shift_ts(&self, ts: u64) -> bool {
        self.committed_shift_ts_ranges
            .iter()
            .any(|&(first, last)| first <= ts && ts <= last)
    }

    pub(super) fn record_live_shift(
        &mut self,
        local_author: bool,
        first_ts: u64,
        cell_count: usize,
        seq: u64,
        old_seq: u64,
        pre_clock: u64,
    ) {
        if seq != old_seq && pre_clock > 0 {
            self.pre_shift_stale_ts = self.pre_shift_stale_ts.max(pre_clock);
        }
        if local_author && cell_count > 0 {
            let last_ts = first_ts.saturating_add(cell_count as u64 - 1);
            self.committed_shift_ts_ranges.push((first_ts, last_ts));
        }
        self.applied_seq = seq;
    }
}

struct DeferredPasteWrites {
    coord: OutboxCoord,
    first_exclusive: u64,
    through: u64,
}

pub(super) fn shift_cell_shift_coord(
    r: u32,
    c: u32,
    rect: (u32, u32, u32, u32),
    horizontal: bool,
    effective_insert: bool,
) -> Option<(u32, u32)> {
    let (r1, c1, r2, c2) = rect;
    if horizontal {
        if r < r1 || r > r2 {
            return Some((r, c));
        }
        let n = c2 - c1 + 1;
        if effective_insert {
            if c < c1 {
                Some((r, c))
            } else {
                c.checked_add(n)
                    .filter(|&nc| nc < sheetcore::sync::MAX_COLS)
                    .map(|nc| (r, nc))
            }
        } else if c < c1 {
            Some((r, c))
        } else if c <= c2 {
            None
        } else {
            Some((r, c - n))
        }
    } else {
        if c < c1 || c > c2 {
            return Some((r, c));
        }
        let n = r2 - r1 + 1;
        if effective_insert {
            if r < r1 {
                Some((r, c))
            } else {
                r.checked_add(n)
                    .filter(|&nr| nr < sheetcore::sync::MAX_ROWS)
                    .map(|nr| (nr, c))
            }
        } else if r < r1 {
            Some((r, c))
        } else if r <= r2 {
            None
        } else {
            Some((r - n, c))
        }
    }
}

impl SheetApp {
    pub(super) fn current_coord_epoch(&self) -> CoordEpoch {
        if let Some(net) = self.net.as_ref() {
            CoordEpoch {
                struct_rev: net.structure_seen(),
                shift_seq: self.shift_intro.applied_seq,
            }
        } else {
            CoordEpoch {
                struct_rev: self.shift_intro.last_struct_rev,
                shift_seq: self.shift_intro.applied_seq,
            }
        }
    }

    pub(super) fn current_outbox_coord(&self) -> OutboxCoord {
        let room_id = if !self.shift_intro.room_id.is_empty() {
            self.shift_intro.room_id.clone()
        } else {
            self.net
                .as_ref()
                .map(|n| n.connected_room_id())
                .unwrap_or_default()
        };
        OutboxCoord {
            room_id,
            epoch: self.current_coord_epoch(),
        }
    }

    /// Unacknowledged writes can survive a dropped socket in the rejoin outbox.
    /// Remember which ones came from an asynchronous paste tied to a coordinate frame.
    pub(super) fn track_deferred_paste(&mut self, first_exclusive: u64, through: u64) {
        let Some(net) = self.net.as_ref().filter(|net| !net.hosting) else {
            return;
        };
        if through <= first_exclusive {
            return;
        }
        let acked = net.acked_op_ts();
        let coord = self.current_outbox_coord();
        let pastes = &mut self.shift_intro.deferred_pastes;
        pastes.retain(|p| p.coord.room_id != coord.room_id || p.through > acked);
        if through > acked {
            pastes.push(DeferredPasteWrites {
                coord,
                first_exclusive,
                through,
            });
        }
    }

    pub(super) fn advance_deferred_paste_struct_rev(
        &mut self,
        cur_room: &str,
        target_rev: Option<usize>,
    ) {
        for p in &mut self.shift_intro.deferred_pastes {
            if p.coord.matches_room(cur_room) {
                if let Some(rev) = target_rev {
                    if p.coord.epoch.struct_rev < rev {
                        p.coord.epoch.struct_rev = rev;
                    }
                } else {
                    p.coord.epoch.struct_rev = p.coord.epoch.struct_rev.saturating_add(1);
                }
            }
        }
    }

    fn draft_sheet_name(&self, sheet: usize) -> String {
        self.wb
            .sheets
            .get(sheet)
            .map(|s| s.name.clone())
            .unwrap_or_else(|| format!("Sheet{}", sheet + 1))
    }

    fn cancellation_reason(coord: &OutboxCoord, cur_room: &str) -> String {
        if !coord.room_id.is_empty() && !cur_room.is_empty() && coord.room_id != cur_room {
            "协作房间已切换".into()
        } else {
            "工作表坐标已改变".into()
        }
    }

    fn push_cancelled_offline_draft(&mut self, draft: CancelledOfflineDraft) {
        if let Some(existing) = self.cancelled_offline_drafts.iter_mut().find(|d| {
            d.room_id == draft.room_id
                && d.sheet == draft.sheet
                && d.row == draft.row
                && d.col == draft.col
                && d.merge_rect == draft.merge_rect
        }) {
            if !draft.value_display.is_empty() || draft.formula.is_some() {
                existing.value_display = draft.value_display;
                existing.formula = draft.formula;
            }
            existing.style_changed |= draft.style_changed;
            if existing.sheet_name.is_empty() {
                existing.sheet_name = draft.sheet_name;
            }
            existing.reason = draft.reason;
            return;
        }
        if self.cancelled_offline_drafts.len() >= MAX_CANCELLED_OFFLINE_DRAFTS {
            return;
        }
        self.cancelled_offline_drafts.push(draft);
    }

    pub(super) fn archive_cancelled_offline_op(
        &mut self,
        room_id: String,
        op: &sheetcore::sync::Op,
        reason: String,
    ) {
        let sheet_name = self.draft_sheet_name(op.sheet);
        let value_display = sheetcore::Value::from(&op.value).display();
        let formula = op.formula.as_deref().map(str::to_string);
        self.push_cancelled_offline_draft(CancelledOfflineDraft {
            room_id,
            sheet: op.sheet,
            sheet_name,
            row: op.row,
            col: op.col,
            value_display,
            formula,
            style_changed: false,
            merge_rect: None,
            reason,
        });
    }

    pub(crate) fn clear_cancelled_offline_drafts(&mut self) {
        self.cancelled_offline_drafts.clear();
    }

    pub(crate) fn export_cancelled_offline_drafts_tsv(&self) -> String {
        if self.cancelled_offline_drafts.is_empty() {
            return String::new();
        }
        use std::fmt::Write as _;
        let mut out = String::from("房间\t工作表\t单元格\t原值\t公式\t样式变更\t合并区域\t原因");
        for d in &self.cancelled_offline_drafts {
            let cell = sheetcore::cell_name(d.row, d.col);
            let merge = d
                .merge_rect
                .map(|(r1, c1, r2, c2)| {
                    format!(
                        "{}:{}",
                        sheetcore::cell_name(r1, c1),
                        sheetcore::cell_name(r2, c2)
                    )
                })
                .unwrap_or_default();
            let style = if d.style_changed { "是" } else { "否" };
            let formula = d.formula.as_deref().unwrap_or("");
            let _ = write!(
                out,
                "\n{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                sanitize_tsv_field(&d.room_id),
                sanitize_tsv_field(&d.sheet_name),
                cell,
                sanitize_tsv_field(&d.value_display),
                sanitize_tsv_field(formula),
                style,
                merge,
                sanitize_tsv_field(&d.reason),
            );
        }
        out
    }

    pub(crate) fn copy_cancelled_offline_drafts_to_clipboard(&mut self) -> bool {
        let tsv = self.export_cancelled_offline_drafts_tsv();
        if tsv.is_empty() {
            return false;
        }
        #[cfg(test)]
        {
            crate::clipboard::set_test_clipboard(&tsv);
            self.status = format!(
                "已复制 {} 条本地冲突草稿到剪贴板（TSV）",
                self.cancelled_offline_drafts.len()
            );
            true
        }
        #[cfg(not(test))]
        {
            match arboard::Clipboard::new().and_then(|mut c| c.set_text(tsv)) {
                Ok(()) => {
                    self.status = format!(
                        "已复制 {} 条本地冲突草稿到剪贴板（TSV）",
                        self.cancelled_offline_drafts.len()
                    );
                    true
                }
                Err(_) => {
                    self.status = "复制本地冲突草稿失败：系统剪贴板不可用".into();
                    false
                }
            }
        }
    }

    /// Discard ALL stale unacknowledged outbox entries (ordinary cell edits, style edits,
    /// merge edits, and deferred background pastes) whose room or unified coordinate epoch
    /// no longer matches the settled snapshot, archiving them into `cancelled_offline_drafts`.
    pub(super) fn discard_stale_rejoin_outboxes(&mut self) -> usize {
        let Some(net) = &self.net else { return 0 };
        if net.hosting {
            return 0;
        }
        let uid = self.doc_uid;
        let cur_room = net.connected_room_id();
        let cur_epoch = self.current_coord_epoch();
        let acked = net.acked_op_ts();
        let mut discarded = 0usize;

        // 1. Check ordinary cell outbox against its bound room & unified CoordEpoch.
        let (ops_stale, ops_room, ops_reason) = match (&self.rejoin_outbox, &self.rejoin_outbox_meta)
        {
            (Some((owner, _)), Some(meta)) if *owner == uid && meta.doc_uid == uid => {
                let stale = meta.coord.is_stale(&cur_room, cur_epoch);
                let room = if meta.coord.room_id.is_empty() {
                    cur_room.clone()
                } else {
                    meta.coord.room_id.clone()
                };
                let reason = Self::cancellation_reason(&meta.coord, &cur_room);
                (stale, room, reason)
            }
            (Some((owner, _)), None) if *owner == uid => (
                cur_epoch.shift_seq != 0,
                cur_room.clone(),
                "工作表坐标已改变".to_string(),
            ),
            _ => (false, String::new(), String::new()),
        };
        if ops_stale {
            if let Some((_, ops)) = self.rejoin_outbox.take() {
                for op in ops {
                    if op.ts > acked {
                        discarded += 1;
                        self.archive_cancelled_offline_op(
                            ops_room.clone(),
                            &op,
                            ops_reason.clone(),
                        );
                    }
                }
            }
            self.rejoin_outbox_meta = None;
        } else if self
            .rejoin_outbox_meta
            .as_ref()
            .is_some_and(|meta| meta.doc_uid == uid && meta.coord.is_stale(&cur_room, cur_epoch))
        {
            self.rejoin_outbox_meta = None;
        }

        // 2. Check deferred background pastes (in case a paste predated a shift while other ops didn't).
        let pastes = &mut self.shift_intro.deferred_pastes;
        let stale_pastes: Vec<_> = pastes
            .iter()
            .filter(|p| p.coord.is_stale(&cur_room, cur_epoch))
            .map(|p| {
                let room = if p.coord.room_id.is_empty() {
                    cur_room.clone()
                } else {
                    p.coord.room_id.clone()
                };
                let reason = Self::cancellation_reason(&p.coord, &cur_room);
                (p.first_exclusive, p.through, room, reason)
            })
            .collect();
        pastes.retain(|p| !p.coord.is_stale(&cur_room, cur_epoch) && p.through > acked);
        if !stale_pastes.is_empty() {
            let mut removed_paste_ops = Vec::new();
            if let Some((owner, ops)) = &mut self.rejoin_outbox {
                if *owner == uid {
                    let mut keep = Vec::with_capacity(ops.len());
                    for op in ops.drain(..) {
                        if let Some((_, _, room, reason)) = stale_pastes
                            .iter()
                            .find(|(prior, last, _, _)| *prior < op.ts && op.ts <= *last)
                        {
                            discarded += 1;
                            if op.ts > acked {
                                removed_paste_ops.push((room.clone(), op, reason.clone()));
                            }
                        } else {
                            keep.push(op);
                        }
                    }
                    *ops = keep;
                    if ops.is_empty() {
                        self.rejoin_outbox = None;
                        self.rejoin_outbox_meta = None;
                    }
                }
            }
            for (room, op, reason) in removed_paste_ops {
                self.archive_cancelled_offline_op(room, &op, reason);
            }
        }

        // 3. Check style outbox entries against their bound room & unified CoordEpoch.
        if !self.rejoin_style_outbox.is_empty() {
            let mut keep = Vec::with_capacity(self.rejoin_style_outbox.len());
            let mut archived = Vec::new();
            for entry in std::mem::take(&mut self.rejoin_style_outbox) {
                if entry.doc_uid == uid && entry.coord.is_stale(&cur_room, cur_epoch) {
                    discarded += entry.items.len();
                    let room = if entry.coord.room_id.is_empty() {
                        cur_room.clone()
                    } else {
                        entry.coord.room_id.clone()
                    };
                    let reason = Self::cancellation_reason(&entry.coord, &cur_room);
                    archived.push((room, entry.sheet, entry.items, reason));
                } else {
                    keep.push(entry);
                }
            }
            self.rejoin_style_outbox = keep;
            for (room, sheet, items, reason) in archived {
                let sheet_name = self.draft_sheet_name(sheet);
                for (row, col, _) in items {
                    self.push_cancelled_offline_draft(CancelledOfflineDraft {
                        room_id: room.clone(),
                        sheet,
                        sheet_name: sheet_name.clone(),
                        row,
                        col,
                        value_display: String::new(),
                        formula: None,
                        style_changed: true,
                        merge_rect: None,
                        reason: reason.clone(),
                    });
                }
            }
        }

        // 4. Check merge outbox entries against their bound room & unified CoordEpoch.
        if !self.rejoin_merge_outbox.is_empty() {
            let mut keep = Vec::with_capacity(self.rejoin_merge_outbox.len());
            let mut archived = Vec::new();
            for entry in std::mem::take(&mut self.rejoin_merge_outbox) {
                if entry.doc_uid == uid && entry.coord.is_stale(&cur_room, cur_epoch) {
                    discarded += 1;
                    let room = if entry.coord.room_id.is_empty() {
                        cur_room.clone()
                    } else {
                        entry.coord.room_id.clone()
                    };
                    let reason = Self::cancellation_reason(&entry.coord, &cur_room);
                    archived.push((room, entry.sheet, entry.rect, entry.on, reason));
                } else {
                    keep.push(entry);
                }
            }
            self.rejoin_merge_outbox = keep;
            for (room, sheet, rect, on, reason) in archived {
                let sheet_name = self.draft_sheet_name(sheet);
                self.push_cancelled_offline_draft(CancelledOfflineDraft {
                    room_id: room,
                    sheet,
                    sheet_name,
                    row: rect.0,
                    col: rect.1,
                    value_display: if on {
                        "合并单元格".into()
                    } else {
                        "取消合并".into()
                    },
                    formula: None,
                    style_changed: false,
                    merge_rect: Some(rect),
                    reason,
                });
            }
        }

        discarded
    }

    /// Keep live collaboration coordinates, locks, active editing state, remote cursors,
    /// and any queued rejoin outboxes consistent when a `CellShift` commits live.
    pub(super) fn on_live_cell_shift_committed(
        &mut self,
        site: u32,
        phase: CellShiftPhase,
        patch: &CellShiftPatch,
        first_ts: u64,
        seq: u64,
        old_seq: u64,
        pre_clock: u64,
    ) {
        let my_site = self.net.as_ref().map(|n| n.site).unwrap_or(0);
        let hosting = self.net.as_ref().is_some_and(|n| n.hosting);
        let target_sheet = patch.target_sheet();
        let forward = !matches!(phase, CellShiftPhase::Undo);
        let effective_insert = patch.insert == forward;

        self.shift_intro.record_live_shift(
            !hosting && site == my_site,
            first_ts,
            patch.cells.len(),
            seq,
            old_seq,
            pre_clock,
        );
        if let Some(net) = self.net.as_ref() {
            net.set_shift_seq(seq);
        }

        let in_shifted_band = |si: usize, r: u32, c: u32| -> bool {
            si == target_sheet
                && shift_cell_shift_coord(r, c, patch.rect, patch.horizontal, effective_insert)
                    != Some((r, c))
        };
        let touched_by_patch = |si: usize, r: u32, c: u32| -> bool {
            in_shifted_band(si, r, c)
                || patch
                    .cells
                    .iter()
                    .any(|cell| cell.sheet == si && cell.row == r && cell.col == c)
        };

        // Release any local lock entries in the shifted band / touched cells (host does not
        // shift locks across partial CellShift and already fenced other peers' active locks).
        let stale_locks: Vec<(usize, u32, u32, u32)> = self
            .locks
            .iter()
            .filter_map(|((ls, lr, lc), holder)| {
                touched_by_patch(ls, lr, lc).then_some((ls, lr, lc, holder.site))
            })
            .collect();
        for (ls, lr, lc, lsite) in stale_locks {
            self.locks.release(ls, lr, lc, lsite);
        }

        let mut cancelled_edit = false;
        if let Some((ls, lr, lc)) = self.my_lock {
            if touched_by_patch(ls, lr, lc) {
                self.my_lock = None;
                if let Some(net) = self.net.as_ref() {
                    net.release_lock(ls, lr, lc);
                }
                if self.edit_sheet.unwrap_or(self.sheet_idx) == ls
                    && self.edit_sel == Some((lr, lc))
                {
                    if self.editing.take().is_some() {
                        cancelled_edit = true;
                    }
                    self.pending_edit = None;
                    self.edit_sel = None;
                    self.edit_sheet = None;
                }
            }
        }
        if let Some((ls, lr, lc)) = self.requested_lock {
            if touched_by_patch(ls, lr, lc) {
                self.requested_lock = None;
                self.pending_edit = None;
                if let Some(net) = self.net.as_ref() {
                    net.release_lock(ls, lr, lc);
                }
                if self.edit_sheet.unwrap_or(self.sheet_idx) == ls
                    && self.edit_sel == Some((lr, lc))
                {
                    if self.editing.take().is_some() {
                        cancelled_edit = true;
                    }
                    self.edit_sel = None;
                    self.edit_sheet = None;
                }
            }
        }
        let edit_sheet = self.edit_sheet.unwrap_or(self.sheet_idx);
        let active_edit_cell = self
            .edit_sel
            .or_else(|| self.editing.is_some().then_some(self.sel));
        if let Some((er, ec)) = active_edit_cell {
            if touched_by_patch(edit_sheet, er, ec) {
                if self.editing.take().is_some() {
                    cancelled_edit = true;
                }
                self.pending_edit = None;
                self.edit_sel = None;
                self.edit_sheet = None;
            }
        }

        // Shift or remove remote cursors on the target sheet.
        let old_remote = std::mem::take(&mut self.remote);
        for (rsite, (rsh, rname, rcol, rr, rc, er, ec)) in old_remote {
            if rsh == target_sheet {
                if let Some((nr, nc)) =
                    shift_cell_shift_coord(rr, rc, patch.rect, patch.horizontal, effective_insert)
                {
                    let (ner, nec) = shift_cell_shift_coord(
                        er,
                        ec,
                        patch.rect,
                        patch.horizontal,
                        effective_insert,
                    )
                    .unwrap_or((nr, nc));
                    self.remote
                        .insert(rsite, (rsh, rname, rcol, nr, nc, ner, nec));
                }
            } else {
                self.remote
                    .insert(rsite, (rsh, rname, rcol, rr, rc, er, ec));
            }
        }

        // Discard any queued rejoin outboxes from older shift epochs.
        let discarded = self.discard_stale_rejoin_outboxes();
        if discarded > 0 {
            self.shift_intro.pending_cancelled_edits = self
                .shift_intro
                .pending_cancelled_edits
                .saturating_add(discarded);
            self.shift_intro.needs_delta_resync_for_stale = true;
        }
        if cancelled_edit {
            self.status.push_str("；当前编辑单元格受位移影响，已取消编辑");
        }
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
        let intent = TxIntent::from_cell_shift(si, &op);
        if let Err(err) = WorkbookTx::preflight(&self.wb, &intent) {
            self.status = err.message().into();
            return;
        }
        let sh = self.wb.sheet(si);

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
        let focus = (si, self.sel.0, self.sel.1);
        let (_entry_id, effects) = match self.execute_local_tx(intent, Some(label.to_string()), focus) {
            Ok(ok) => ok,
            Err(TxError::NoOp) => return,
            Err(err) => {
                self.status = err.message().into();
                return;
            }
        };
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
        if effects.detached_sources {
            self.status
                .push_str("；部分数据源的绑定区域已失效，请重新选择区域并导入");
        }
    }

    fn request_room_shift(&mut self, sheet: usize, op: sheetcore::structural::CellShift) {
        let intent = TxIntent::from_cell_shift(sheet, &op);
        if let Err(err) = WorkbookTx::preflight(&self.wb, &intent) {
            self.status = err.message().into();
            return;
        }
        let Some(patch) = TxPatch::prepare(&self.wb, &intent).and_then(TxPatch::into_cell_shift) else {
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
            let effective_op = sheetcore::structural::CellShift::new(
                patch.rect,
                if patch.horizontal {
                    sheetcore::cells::Shift::Horizontal
                } else {
                    sheetcore::cells::Shift::Vertical
                },
                patch.insert == redo,
            );
            if let Err(err) = WorkbookTx::preflight(
                &self.wb,
                &TxIntent::from_cell_shift(patch.target_sheet(), &effective_op),
            ) {
                self.status = err.message().into();
                return false;
            }
            let tx_patch = TxPatch::from_cell_shift((*patch).clone());
            if !tx_patch.matches_replica(&self.wb, redo)
                || !tx_patch.apply_committed(&mut self.wb, redo)
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
        if matches!(phase, CellShiftPhase::Forward) {
            let intent = TxIntent::from_cell_shift(patch.target_sheet(), &patch.operation());
            if let Err(err) = WorkbookTx::preflight(&self.wb, &intent) {
                return Err(err.message().into());
            }
        }
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
        let effective_op = sheetcore::structural::CellShift::new(
            patch.rect,
            if patch.horizontal {
                sheetcore::cells::Shift::Horizontal
            } else {
                sheetcore::cells::Shift::Vertical
            },
            patch.insert == forward,
        );
        if let Err(err) =
            WorkbookTx::preflight(&self.wb, &TxIntent::from_cell_shift(sheet, &effective_op))
        {
            return Err(err.message().into());
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
        let effects = TxEffects {
            recalc: RecalcScope::Full,
            invalidate_deps: true,
            invalidate_render_caches: true,
            coord_changed: true,
            structural: false,
            detached_sources: false,
            revision_deltas: Vec::new(),
            touched_sheets: Vec::new(),
            new_focus: None,
        };
        self.apply_tx_effects("", &effects);
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

    pub(crate) fn request_room_defined_names(
        &mut self,
        intent: DefinedNameIntent,
    ) -> Result<bool, TxError> {
        let tx_intent = TxIntent::DefinedNames(intent.clone());
        WorkbookTx::preflight(&self.wb, &tx_intent)?;
        let Some(patch) = DefinedNamesPatch::prepare(&self.wb, &intent) else {
            return Ok(false);
        };
        if let Err(reason) = self.request_room_tx(RoomTxPatch::DefinedNames(patch)) {
            self.status = reason;
            return Ok(false);
        }
        self.status = "正在等待宿主确认名称更改".into();
        Ok(true)
    }

    pub(crate) fn request_room_axis_permute(
        &mut self,
        sheet: usize,
        axis: Axis,
        from: u32,
        n: u32,
        drop: u32,
    ) -> Result<bool, String> {
        let Some(intent) = TxIntent::from_axis_move(sheet, axis, from, n, drop) else {
            return Ok(false);
        };
        if let Err(err) = WorkbookTx::preflight(&self.wb, &intent) {
            return Err(err.message().into());
        }
        let Some(patch) = AxisPermutePatch::from_drop(&self.wb, sheet, axis, from, n, drop) else {
            return Ok(false);
        };
        self.request_room_tx(RoomTxPatch::AxisPermute(patch))?;
        self.status = if axis == Axis::Row {
            "正在等待宿主确认移动行".into()
        } else {
            "正在等待宿主确认移动列".into()
        };
        Ok(true)
    }

    fn request_room_tx(&mut self, patch: RoomTxPatch) -> Result<(), String> {
        let Some(net) = &self.net else {
            return Err("当前未处于协作房间".into());
        };
        if !net.supports_room_tx() {
            return Err("协作宿主版本较旧，请升级后再执行该协作操作".into());
        }
        self.check_room_tx(net.site, CellShiftPhase::Forward, &patch)?;
        let Ok(id) = getrandom::u64() else {
            return Err("无法创建本次协作操作，请重试".into());
        };
        net.request_room_tx(id, CellShiftPhase::Forward, Arc::new(patch))
    }

    pub(super) fn request_room_tx_replay(
        &mut self,
        id: u64,
        patch: Arc<RoomTxPatch>,
        redo: bool,
    ) -> bool {
        if !self
            .undo
            .room_shift_history_matches(&self.wb, redo, self.net.is_none())
        {
            self.status = "关联工作表或内容已更改，已保留历史记录".into();
            return false;
        }
        let phase = if redo {
            CellShiftPhase::Redo
        } else {
            CellShiftPhase::Undo
        };
        let Some(net) = &self.net else {
            let label = patch.label();
            let is_coord = matches!(*patch, RoomTxPatch::AxisPermute(_) | RoomTxPatch::CellShift(_));
            if !patch.matches_replica(&self.wb, redo) || !patch.apply_committed(&mut self.wb, redo)
            {
                self.status = "关联内容已更改，已保留历史记录".into();
                return false;
            }
            if is_coord {
                self.finish_shift_edit();
            } else {
                let effects = TxEffects {
                    recalc: RecalcScope::Full,
                    invalidate_deps: true,
                    invalidate_render_caches: true,
                    ..TxEffects::default()
                };
                self.apply_tx_effects(label, &effects);
            }
            self.finish_room_tx_history(id, phase, Arc::new(patch.to_tx_patch()));
            self.status = if redo {
                format!("已重做{label}")
            } else {
                format!("已撤销{label}")
            };
            return true;
        };
        let site = net.site;
        let result = self
            .check_room_tx(site, phase, &patch)
            .and_then(|()| self.net.as_ref().unwrap().request_room_tx(id, phase, patch));
        if let Err(reason) = result {
            self.status = reason;
            return false;
        }
        self.status = "正在等待宿主确认协作事务".into();
        true
    }

    pub(super) fn validate_room_tx_request(
        &self,
        site: u32,
        phase: CellShiftPhase,
        patch: Arc<RoomTxPatch>,
    ) -> Result<Arc<RoomTxPatch>, String> {
        let canonical = if matches!(phase, CellShiftPhase::Forward) {
            Arc::new(
                patch
                    .canonical_forward(&self.wb)
                    .ok_or("工作簿状态已变更，请重新执行操作")?,
            )
        } else {
            patch
        };
        self.check_room_tx(site, phase, &canonical)?;
        Ok(canonical)
    }

    fn check_room_tx(
        &self,
        site: u32,
        phase: CellShiftPhase,
        patch: &RoomTxPatch,
    ) -> Result<(), String> {
        if self.deps_pending {
            return Err("工作簿正在建立依赖关系，请稍后重试".into());
        }
        let forward = !matches!(phase, CellShiftPhase::Undo);
        match patch {
            RoomTxPatch::DefinedNames(dn) => {
                if !dn.matches(&self.wb, forward) {
                    return Err("定义名称已由其他成员修改，请重新操作".into());
                }
                Ok(())
            }
            RoomTxPatch::AxisPermute(ap) => {
                if !ap.can_install(&self.wb) {
                    return Err("目标工作表已变更，无法移动行或列".into());
                }
                if let Some(tx) = ap.effective_transform(forward) {
                    if let Err(err) = tx.preflight(&self.wb) {
                        return Err(err.message().into());
                    }
                } else {
                    return Err("移动范围无效".into());
                }
                if self.wb.sheet(ap.sheet).protection.is_some() {
                    return Err("工作表已受保护，无法移动行或列".into());
                }
                let (axis, eff_from, eff_n, eff_at) = ap.effective_move(forward);
                if let Some(((r, c), holder)) = self.locks.conflicts_axis_permute(
                    ap.sheet,
                    axis,
                    eff_from,
                    eff_n,
                    eff_at,
                    site,
                    self.lock_now(),
                ) {
                    return Err(format!(
                        "{} 正在编辑 {}，无法移动行或列",
                        holder.name,
                        sheetcore::cell_name(r, c)
                    ));
                }
                Ok(())
            }
            RoomTxPatch::CellShift(cs) => self.check_room_cell_shift(site, phase, cs),
        }
    }

    pub(super) fn commit_room_tx(
        &mut self,
        site: u32,
        id: u64,
        phase: CellShiftPhase,
        patch: Arc<RoomTxPatch>,
        seq: u64,
        old_seq: u64,
        pre_clock: u64,
    ) -> bool {
        let forward = !matches!(phase, CellShiftPhase::Undo);
        if !patch.can_install(&self.wb) {
            self.resync_room_cell_shift();
            return false;
        }
        let local = self.net.as_ref().is_some_and(|n| n.site == site);
        let label = patch.label();
        match &*patch {
            RoomTxPatch::DefinedNames(dn) => {
                if !dn.apply_committed(&mut self.wb, forward) {
                    self.resync_room_cell_shift();
                    return false;
                }
                let effects = TxEffects {
                    recalc: RecalcScope::Full,
                    invalidate_deps: true,
                    invalidate_render_caches: true,
                    ..TxEffects::default()
                };
                self.apply_tx_effects(label, &effects);
                if local {
                    if let Some(net) = &self.net {
                        net.finish_structure();
                    }
                    self.finish_room_tx_history(
                        id,
                        phase,
                        Arc::new(TxPatch::DefinedNames(dn.clone())),
                    );
                    self.status = match phase {
                        CellShiftPhase::Forward => "名称已保存".into(),
                        CellShiftPhase::Undo => format!("已撤销{label}"),
                        CellShiftPhase::Redo => format!("已重做{label}"),
                    };
                } else {
                    self.record_remote_note_revision(site, "远端名称更改", 0, label.into());
                    let who = self.peer_display_name(site);
                    self.status = format!("{who}：{label}");
                }
                true
            }
            RoomTxPatch::AxisPermute(ap) => {
                if !ap.apply_committed(&mut self.wb, forward) {
                    self.resync_room_cell_shift();
                    return false;
                }
                self.on_live_axis_permute_committed(ap, phase, seq, old_seq, pre_clock);
                self.finish_shift_edit();
                if local {
                    if let Some(net) = &self.net {
                        net.finish_structure();
                    }
                    self.finish_room_tx_history(
                        id,
                        phase,
                        Arc::new(TxPatch::AxisPermute(ap.clone())),
                    );
                    self.status = match phase {
                        CellShiftPhase::Forward => format!("已{label}"),
                        CellShiftPhase::Undo => format!("已撤销{label}"),
                        CellShiftPhase::Redo => format!("已重做{label}"),
                    };
                } else {
                    self.undo.clear();
                    self.record_remote_note_revision(site, "远端移动行列", ap.sheet, label.into());
                    let who = self.peer_display_name(site);
                    self.status = format!("{who}：{label}");
                }
                true
            }
            RoomTxPatch::CellShift(cs) => {
                self.commit_room_cell_shift(site, id, phase, Arc::new(cs.clone()), 0)
            }
        }
    }

    fn on_live_axis_permute_committed(
        &mut self,
        patch: &AxisPermutePatch,
        phase: CellShiftPhase,
        seq: u64,
        old_seq: u64,
        pre_clock: u64,
    ) {
        let forward = !matches!(phase, CellShiftPhase::Undo);
        let (axis, eff_from, eff_n, eff_at) = patch.effective_move(forward);
        let sheet = patch.sheet;
        let map = |r: u32, c: u32| {
            sheetcore::sync::permute_axis_coord(r, c, axis, eff_from, eff_n, eff_at)
        };

        self.shift_intro
            .record_live_shift(false, 0, 0, seq, old_seq, pre_clock);
        if let Some(net) = self.net.as_ref() {
            net.set_shift_seq(seq);
        }

        self.locks.permute_axis(sheet, axis, eff_from, eff_n, eff_at);
        if let Some(peer) = self.peer.as_mut() {
            let names: Vec<_> = self.wb.sheets.iter().map(|s| s.name.clone()).collect();
            peer.permute_axis_with_formulas(&names, sheet, axis, eff_from, eff_n, eff_at);
        }
        if let Some((ls, lr, lc)) = self.my_lock {
            if ls == sheet {
                match map(lr, lc) {
                    Some((nr, nc)) => self.my_lock = Some((ls, nr, nc)),
                    None => {
                        self.my_lock = None;
                        self.pending_edit = None;
                        self.editing = None;
                    }
                }
            }
        }
        if let Some((ls, lr, lc)) = self.requested_lock {
            if ls == sheet {
                self.requested_lock = map(lr, lc).map(|(nr, nc)| (ls, nr, nc));
            }
        }
        if self.edit_sheet == Some(sheet) {
            if let Some((er, ec)) = self.edit_sel {
                self.edit_sel = map(er, ec);
            }
        }
        let old_remote = std::mem::take(&mut self.remote);
        for (rsite, (rsh, rname, rcol, rr, rc, er, ec)) in old_remote {
            if rsh == sheet {
                if let Some((nr, nc)) = map(rr, rc) {
                    let (ner, nec) = map(er, ec).unwrap_or((nr, nc));
                    self.remote
                        .insert(rsite, (rsh, rname, rcol, nr, nc, ner, nec));
                }
            } else {
                self.remote
                    .insert(rsite, (rsh, rname, rcol, rr, rc, er, ec));
            }
        }

        let discarded = self.discard_stale_rejoin_outboxes();
        if discarded > 0 {
            self.shift_intro.pending_cancelled_edits = self
                .shift_intro
                .pending_cancelled_edits
                .saturating_add(discarded);
            self.shift_intro.needs_delta_resync_for_stale = true;
        }
    }

    pub(super) fn finish_room_tx_history(
        &mut self,
        id: u64,
        phase: CellShiftPhase,
        patch: Arc<TxPatch>,
    ) {
        if matches!(phase, CellShiftPhase::Forward) {
            if patch.can_install(&self.wb) {
                self.undo.push_room_tx_patch(id, patch, &self.wb);
            }
            return;
        }
        let redo = matches!(phase, CellShiftPhase::Redo);
        let entry = if redo {
            self.undo.top_redo_id()
        } else {
            self.undo.top_undo_id()
        };
        if !self.undo.confirm_room_tx_patch(id, redo) {
            return;
        }
        if let Some(entry) = entry {
            if redo {
                self.audit_on_redo(entry);
            } else {
                self.audit_on_undo(entry);
            }
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
    for &(r, c) in sh
        .rich
        .keys()
        .chain(sh.hyperlinks.keys())
        .chain(sh.ignored_errors.iter())
    {
        include(r, c);
    }
    for sp in &sh.sparklines {
        include(sp.r, sp.c);
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
