use super::*;
use std::sync::MutexGuard;

pub(super) fn structural_anchor(op: &sheetcore::structural::Structural) -> (u32, u32) {
    use sheetcore::structural::Structural::*;
    match *op {
        InsertRows { at, .. } | DeleteRows { at, .. } => (at, 0),
        InsertCols { at, .. } | DeleteCols { at, .. } => (0, at),
    }
}

pub(super) fn reject_pending_shift(
    events: &EvtTx,
    pending: &ShiftPending,
    id: u64,
    phase: CellShiftPhase,
    reason: String,
) {
    let mut entry = lock_unpoison(pending);
    if entry
        .as_ref()
        .is_some_and(|(expected, mode, _)| *expected == id && *mode == phase)
    {
        entry.take();
        let _ = events.send(NetEvent::StructuralRejected(reason));
    }
}

pub(super) fn reject_pending_room_tx(
    events: &EvtTx,
    pending: &RoomTxPending,
    id: u64,
    phase: CellShiftPhase,
    reason: String,
) {
    let mut entry = lock_unpoison(pending);
    if entry
        .as_ref()
        .is_some_and(|(expected, mode, _)| *expected == id && *mode == phase)
    {
        entry.take();
        let _ = events.send(NetEvent::StructuralRejected(reason));
    }
}

pub(super) fn finish_shift_snapshot(
    events: &EvtTx,
    pending: &ShiftPending,
    room_tx_pending: &RoomTxPending,
    got_receipt: bool,
) {
    if !got_receipt {
        let had_shift = lock_unpoison(pending).take().is_some();
        let had_room_tx = lock_unpoison(room_tx_pending).take().is_some();
        if had_shift || had_room_tx {
            let _ = events.send(NetEvent::StructuralRejected(if had_room_tx {
                "重连后宿主未确认此次协作事务，已恢复房间状态，可重新执行".into()
            } else {
                "重连后宿主未确认此次局部插删，已恢复房间状态，可重新执行".into()
            }));
        }
    }
    let _ = events.send(NetEvent::SnapshotDone);
}

type Stamp = (CellShiftPhase, u64, u64);
pub(super) type ShiftPending = Arc<Mutex<Option<(u64, CellShiftPhase, Arc<CellShiftPatch>)>>>;
pub(super) type RoomTxPending = Arc<Mutex<Option<(u64, CellShiftPhase, Arc<RoomTxPatch>)>>>;

enum CommittedPayload {
    CellShift(Arc<CellShiftPatch>),
    RoomTx(Arc<RoomTxPatch>),
}

struct CommittedShift {
    id: u64,
    stamp: Stamp,
    request_digest: [u8; 32],
    payload: CommittedPayload,
}

#[derive(Default)]
pub(super) struct ShiftJournal {
    entries: HashMap<(u32, u64), ([u8; 32], Stamp)>,
    seq: u64,
    coord_ack_seq: u64,
    used_room_tx: bool,
    used_full_overlay_tx: bool,
    latest: HashMap<u32, CommittedShift>,
    acks: HashMap<u32, (u64, u64)>,
}

pub(super) struct CellShiftHost {
    pub(super) order: Arc<Mutex<()>>,
    pub(super) roster: Arc<Mutex<RoomRoster>>,
    pub(super) events: EvtTx,
    pub(super) cmd: Sender<Cmd>,
    pub(super) wm: Arc<Mutex<HostWatermark>>,
    pub(super) clock: Arc<AtomicU64>,
    pub(super) structs: Arc<Mutex<Vec<(usize, sheetcore::structural::Structural)>>>,
    pub(super) locks: Arc<Mutex<sheetcore::sync::LockTable>>,
    pub(super) baseline: SharedBaseline,
    pub(super) base_hash: Arc<Mutex<String>>,
    pub(super) cursors: Arc<Mutex<HashMap<u32, Cursor>>>,
    pub(super) sheet_names: Arc<Mutex<Vec<String>>>,
    pub(super) t0: std::time::Instant,
    pub(super) state: SharedGrid<Op>,
    pub(super) delta: SharedGrid<Op>,
    pub(super) formula_keys: Arc<Mutex<BTreeSet<(usize, u32, u32)>>>,
    pub(super) styles: SharedStyles,
    pub(super) delta_styles: SharedStyles,
    pub(super) threads: HostThreads,
    pub(super) merges: Arc<Mutex<Vec<(usize, u32, u32, u32, u32, bool)>>>,
    pub(super) acked_ts: Arc<Mutex<HashMap<u32, u64>>>,
    pub(super) transactions: Mutex<ShiftJournal>,
    pub(super) seed: Mutex<Option<(u64, Arc<CellShiftOverlay>)>>,
    pub(super) has_app: AtomicBool,
}

pub(crate) fn cell_shift_ops(
    site: u32,
    first_ts: u64,
    patch: &CellShiftPatch,
    phase: CellShiftPhase,
) -> Vec<Op> {
    patch
        .cells
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let input = if phase == CellShiftPhase::Undo {
                &c.before
            } else {
                &c.after
            };
            Op {
                site,
                sheet: c.sheet,
                row: c.row,
                col: c.col,
                ts: first_ts + i as u64,
                value: input.value.clone(),
                formula: input.formula.as_deref().map(Arc::from),
            }
        })
        .collect()
}

fn fingerprint(patch: &CellShiftPatch) -> Result<[u8; 32], String> {
    let bytes = serde_json::to_vec(patch).map_err(|e| e.to_string())?;
    if bytes.len() > MAX_PLAIN_LINE - 1024 {
        return Err("局部插删操作超过协作单条消息上限".into());
    }
    Ok(sha256_bytes(&bytes))
}

fn fingerprint_room_tx(patch: &RoomTxPatch) -> Result<[u8; 32], String> {
    let bytes = serde_json::to_vec(patch).map_err(|e| e.to_string())?;
    if bytes.len() > MAX_PLAIN_LINE - 1024 {
        return Err("协作事务操作超过单条消息上限".into());
    }
    Ok(sha256_bytes(&bytes))
}

impl CellShiftHost {
    pub(super) fn ack(&self, site: u32, conn: u64, seq: u64) {
        let mut journal = lock_unpoison(&self.transactions);
        if seq != 0 && seq == journal.coord_ack_seq {
            journal.acks.insert(site, (conn, seq));
        }
    }
    pub(super) fn needs_ack(&self, site: u32, conn: u64) -> bool {
        let journal = lock_unpoison(&self.transactions);
        journal.coord_ack_seq != 0 && journal.acks.get(&site) != Some(&(conn, journal.coord_ack_seq))
    }

    fn check_room_tx(
        &self,
        site: u32,
        conn: Option<u64>,
        revision: usize,
        requires_full_overlay: bool,
    ) -> Result<(), String> {
        let roster = lock_unpoison(&self.roster);
        if conn.is_some_and(|id| roster.peers.get(&site).is_none_or(|peer| peer.0 != id)) {
            return Err("协作连接已更换，请重试".into());
        }
        if roster
            .peers
            .iter()
            .any(|(site, (id, _, _))| roster.room_tx_members.get(site) != Some(id))
        {
            return Err("房间中有旧版本客户端，请所有成员升级后再执行该协作操作".into());
        }
        if requires_full_overlay
            && roster
                .peers
                .iter()
                .any(|(site, (id, _, _))| roster.full_overlay_members.get(site) != Some(id))
        {
            return Err(
                "房间中有旧版本客户端不支持完整元数据快照，请所有成员升级后再移动行或列".into(),
            );
        }
        if self.needs_ack(site, conn.unwrap_or(0)) {
            return Err("正在同步新的工作表位置，请稍后重试".into());
        }
        if revision != lock_unpoison(&self.structs).len() {
            return Err("工作表已变更，请重新执行操作".into());
        }
        Ok(())
    }

    fn check_room_tx_locks(
        &self,
        site: u32,
        phase: CellShiftPhase,
        patch: &RoomTxPatch,
    ) -> Result<(), String> {
        match patch {
            RoomTxPatch::DefinedNames(_) => Ok(()),
            RoomTxPatch::AxisPermute(ap) => {
                let (axis, eff_from, eff_n, eff_at) = ap.effective_move(phase != CellShiftPhase::Undo);
                let locks = lock_unpoison(&self.locks);
                let now = self.t0.elapsed().as_millis() as u64;
                if let Some(((r, c), holder)) = locks.conflicts_axis_permute(
                    ap.sheet,
                    axis,
                    eff_from,
                    eff_n,
                    eff_at,
                    site,
                    now,
                ) {
                    return Err(format!(
                        "{} 正在编辑 {}，无法移动行或列",
                        holder.name,
                        sheetcore::cell_name(r, c)
                    ));
                }
                Ok(())
            }
            RoomTxPatch::CellShift(cs) => self.check_cells(site, phase, cs),
        }
    }

    fn check_room(&self, site: u32, conn: Option<u64>, revision: usize) -> Result<(), String> {
        let roster = lock_unpoison(&self.roster);
        if conn.is_some_and(|id| roster.peers.get(&site).is_none_or(|peer| peer.0 != id)) {
            return Err("协作连接已更换，请重试".into());
        }
        if roster
            .peers
            .iter()
            .any(|(site, (id, _, _))| roster.shift_members.get(site) != Some(id))
        {
            return Err("房间中有旧版本客户端，请所有成员升级后再局部插删".into());
        }
        if self.needs_ack(site, conn.unwrap_or(0)) {
            return Err("正在同步新的工作表位置，请稍后重试".into());
        }
        if revision != lock_unpoison(&self.structs).len() {
            return Err("工作表已变更，请重新执行局部插删".into());
        }
        Ok(())
    }

    fn check_cells(
        &self,
        site: u32,
        phase: CellShiftPhase,
        patch: &CellShiftPatch,
    ) -> Result<(), String> {
        let locks = lock_unpoison(&self.locks);
        let now = self.t0.elapsed().as_millis() as u64;
        let cells = lock_unpoison(&self.state);
        for c in &patch.cells {
            if locks
                .holder_other(c.sheet, c.row, c.col, site, now)
                .is_some()
            {
                return Err("关联单元格正由其他成员编辑".into());
            }
            if phase != CellShiftPhase::Forward
                && cells
                    .get(&(c.sheet, c.row, c.col))
                    .is_none_or(|op| op.site != site)
            {
                return Err("关联单元格已由其他成员更改，已保留历史记录".into());
            }
        }
        let (r1, c1, r2, c2) = patch.rect;
        for ((si, r, c), _) in locks.iter() {
            if si == patch.target_sheet()
                && locks.holder_other(si, r, c, site, now).is_some()
                && if patch.horizontal {
                    (r1..=r2).contains(&r) && c >= c1
                } else {
                    (c1..=c2).contains(&c) && r >= r1
                }
            {
                return Err("移动区域正由其他成员编辑".into());
            }
        }
        Ok(())
    }

    pub(super) fn validate_and_commit(
        &self,
        site: u32,
        id: u64,
        phase: CellShiftPhase,
        revision: usize,
        patch: Arc<CellShiftPatch>,
        conn: Option<u64>,
    ) -> Result<(), String> {
        if id == 0 || !patch.wire_valid() {
            return Err("无效的单元格移动操作".into());
        }
        let digest = fingerprint(&patch)?;
        let (tx, rx) = channel();
        let observed = {
            let _order = lock_unpoison(&self.order);
            // Reliable delivery already queued the live commit; reconnection uses its receipt.
            if lock_unpoison(&self.transactions)
                .latest
                .get(&site)
                .is_some_and(|receipt| {
                    receipt.id == id && receipt.stamp.0 == phase && receipt.request_digest == digest
                })
            {
                return Ok(());
            }
            self.check_room(site, conn, revision)?;
            self.check_cells(site, phase, &patch)?;
            let journal = lock_unpoison(&self.transactions);
            match (phase, journal.entries.get(&(site, id))) {
                (CellShiftPhase::Forward, None) => {}
                (CellShiftPhase::Undo, Some((original, (previous, ..))))
                    if *original == digest && *previous != CellShiftPhase::Undo => {}
                (CellShiftPhase::Redo, Some((original, (CellShiftPhase::Undo, ..))))
                    if *original == digest => {}
                _ => return Err("该协作历史步骤已处理或与宿主记录不一致".into()),
            }
            let seq = lock_unpoison(&self.wm).seq;
            self.events
                .send(NetEvent::CheckCellShift {
                    site,
                    id,
                    phase,
                    patch: patch.clone(),
                    reply: tx,
                })
                .map_err(|_| "宿主已关闭".to_string())?;
            seq
        };
        let canonical = rx
            .recv_timeout(Duration::from_secs(12))
            .map_err(|_| "宿主未能及时核对工作簿，请重试".to_string())??;
        let canonical_digest = fingerprint(&canonical)?;
        if !canonical.wire_valid()
            || (phase != CellShiftPhase::Forward && canonical_digest != digest)
        {
            return Err("宿主历史与操作不匹配".into());
        }
        let _order = lock_unpoison(&self.order);
        self.check_room(site, conn, revision)?;
        self.check_cells(site, phase, &canonical)?;
        let mut wm = lock_unpoison(&self.wm);
        if wm.seq != observed {
            return Err("核对期间工作簿已有新编辑，请重新执行".into());
        }
        let first_ts = self.clock.load(Ordering::Acquire).max(1) + 1;
        if first_ts + canonical.cells.len() as u64 > sheetcore::sync::MAX_LOGICAL_TS {
            return Err("协作时钟超出范围".into());
        }
        let ops = cell_shift_ops(site, first_ts, &canonical, phase);
        wm.record_ops(ops.iter());
        update_host_shared_ops(
            &self.state,
            &self.delta,
            &self.formula_keys,
            &self.clock,
            ops.into_iter().map(Arc::new),
        );
        self.clock.fetch_max(first_ts, Ordering::AcqRel);
        let forward = phase != CellShiftPhase::Undo;
        let mut by_sheet = BTreeMap::new();
        let mut cleared = Vec::new();
        for c in &canonical.cells {
            if c.before_style != c.after_style {
                let st = if forward {
                    &c.after_style
                } else {
                    &c.before_style
                };
                if st.is_none() {
                    cleared.push((c.sheet, c.row, c.col));
                }
                by_sheet.entry(c.sheet).or_insert_with(Vec::new).push((
                    c.row,
                    c.col,
                    st.clone()
                        .unwrap_or_else(sheetcore::style::CellStyle::default_dark),
                ));
            }
        }
        for (sheet, items) in by_sheet {
            update_host_styles(&self.styles, &self.delta_styles, sheet, &items);
            wm.record_styles(sheet, &items);
        }
        // The delta retains the clear marker for snapshot projections. The live table
        // omits cleared styles to preserve row/column inheritance on authoritative repair.
        if !cleared.is_empty() {
            let mut styles = lock_unpoison(&self.styles);
            let values = Arc::make_mut(&mut styles);
            for key in cleared {
                values.remove_cow(&key);
            }
        }
        let sheet = canonical.target_sheet();
        let mut comments = lock_unpoison(&self.threads);
        for change in &canonical.threads {
            let key = (sheet, change.row, change.col);
            comments.remove(&key);
            if let Some(tc) = if forward {
                &change.after
            } else {
                &change.before
            } {
                comments.insert(key, HostComment::Thread(tc.clone()));
            }
        }
        for change in &canonical.notes {
            let key = (sheet, change.row, change.col);
            if !matches!(comments.get(&key), Some(HostComment::Thread(_))) {
                if let Some(note) = if forward {
                    &change.after
                } else {
                    &change.before
                } {
                    comments.insert(key, HostComment::Note(note.clone()));
                } else {
                    comments.remove(&key);
                }
            }
        }
        drop(comments);
        let mut merges = lock_unpoison(&self.merges);
        for m in &canonical.merges {
            let (old, new) = if forward {
                (m.before, m.after)
            } else {
                (m.after, m.before)
            };
            for (rect, on) in old
                .into_iter()
                .map(|m| (m, false))
                .chain(new.into_iter().map(|m| (m, true)))
            {
                merges.push((sheet, rect.0, rect.1, rect.2, rect.3, on));
                wm.record_merge(sheet, rect, on);
            }
        }
        drop(merges);
        let last_ts = (!canonical.cells.is_empty())
            .then(|| first_ts.saturating_add(canonical.cells.len() as u64 - 1));
        if let Some(ack_ts) = last_ts {
            let mut acks = lock_unpoison(&self.acked_ts);
            let entry = acks.entry(site).or_insert(0);
            if ack_ts > *entry {
                *entry = ack_ts;
            }
        }
        let seq = wm.bump();
        let mut journal = lock_unpoison(&self.transactions);
        journal
            .entries
            .insert((site, id), (canonical_digest, (phase, first_ts, seq)));
        journal.seq = seq;
        journal.coord_ack_seq = seq;
        journal.latest.insert(
            site,
            CommittedShift {
                id,
                stamp: (phase, first_ts, seq),
                request_digest: digest,
                payload: CommittedPayload::CellShift(canonical.clone()),
            },
        );
        drop(journal);
        drop(wm);
        let msg = Msg::CellShiftCommitted {
            site,
            id,
            phase,
            first_ts,
            seq,
            patch: canonical.clone(),
        };
        queue_room_change(&self.cmd, &self.roster, None, RoomChange::Message(msg));
        let _ = self.events.send(NetEvent::CellShiftCommitted {
            site,
            id,
            phase,
            first_ts,
            seq,
            patch: canonical,
            snapshot: false,
        });
        if let Some(ack_ts) = last_ts {
            emit_room_watermark_ack(&self.cmd, &self.roster, seq, revision, site, ack_ts);
        } else {
            emit_room_watermark(&self.cmd, &self.roster, seq, revision);
        }
        Ok(())
    }

    pub(super) fn validate_and_commit_room_tx(
        &self,
        site: u32,
        id: u64,
        phase: CellShiftPhase,
        revision: usize,
        patch: Arc<RoomTxPatch>,
        conn: Option<u64>,
    ) -> Result<(), String> {
        if id == 0 || !patch.wire_valid() {
            return Err("无效的协作事务操作".into());
        }
        if matches!(*patch, RoomTxPatch::CellShift(_)) {
            return Err("局部单元格插删请使用标准位移通道".into());
        }
        let requires_full_overlay = matches!(*patch, RoomTxPatch::AxisPermute(_));
        let digest = fingerprint_room_tx(&patch)?;
        let (tx, rx) = channel();
        let observed = {
            let _order = lock_unpoison(&self.order);
            if lock_unpoison(&self.transactions)
                .latest
                .get(&site)
                .is_some_and(|receipt| {
                    receipt.id == id
                        && receipt.stamp.0 == phase
                        && receipt.request_digest == digest
                        && matches!(receipt.payload, CommittedPayload::RoomTx(_))
                })
            {
                return Ok(());
            }
            self.check_room_tx(site, conn, revision, requires_full_overlay)?;
            self.check_room_tx_locks(site, phase, &patch)?;
            let journal = lock_unpoison(&self.transactions);
            match (phase, journal.entries.get(&(site, id))) {
                (CellShiftPhase::Forward, None) => {}
                (CellShiftPhase::Undo, Some((original, (previous, ..))))
                    if *original == digest && *previous != CellShiftPhase::Undo => {}
                (CellShiftPhase::Redo, Some((original, (CellShiftPhase::Undo, ..))))
                    if *original == digest => {}
                _ => return Err("该协作历史步骤已处理或与宿主记录不一致".into()),
            }
            let seq = lock_unpoison(&self.wm).seq;
            self.events
                .send(NetEvent::CheckRoomTx {
                    site,
                    phase,
                    patch: patch.clone(),
                    reply: tx,
                })
                .map_err(|_| "宿主已关闭".to_string())?;
            seq
        };
        let canonical = rx
            .recv_timeout(Duration::from_secs(12))
            .map_err(|_| "宿主未能及时核对工作簿，请重试".to_string())??;
        let canonical_digest = fingerprint_room_tx(&canonical)?;
        if !canonical.wire_valid()
            || (phase != CellShiftPhase::Forward && canonical_digest != digest)
        {
            return Err("宿主历史与操作不匹配".into());
        }
        let canonical_requires_full_overlay = matches!(*canonical, RoomTxPatch::AxisPermute(_));
        let _order = lock_unpoison(&self.order);
        self.check_room_tx(site, conn, revision, canonical_requires_full_overlay)?;
        self.check_room_tx_locks(site, phase, &canonical)?;
        let mut wm = lock_unpoison(&self.wm);
        if wm.seq != observed {
            return Err("核对期间工作簿已有新编辑，请重新执行".into());
        }
        let forward = phase != CellShiftPhase::Undo;
        let (seq, advances_coord_ack) = match &*canonical {
            RoomTxPatch::DefinedNames(_) => (wm.bump(), false),
            RoomTxPatch::AxisPermute(ap) => {
                let (axis, eff_from, eff_n, eff_at) = ap.effective_move(forward);
                let names = lock_unpoison(&self.sheet_names).clone();
                permute_state(
                    &self.baseline,
                    &self.formula_keys,
                    &self.state,
                    &self.delta,
                    &self.base_hash,
                    &self.styles,
                    &self.delta_styles,
                    &self.locks,
                    &self.cursors,
                    &self.threads,
                    &self.merges,
                    &names,
                    ap.sheet,
                    axis,
                    eff_from,
                    eff_n,
                    eff_at,
                );
                let seq = wm.permute_axis(ap.sheet, axis, eff_from, eff_n, eff_at);
                (seq, true)
            }
            RoomTxPatch::CellShift(_) => {
                return Err("局部单元格插删请使用标准位移通道".into());
            }
        };
        let first_ts = 0u64;
        let mut journal = lock_unpoison(&self.transactions);
        journal
            .entries
            .insert((site, id), (canonical_digest, (phase, first_ts, seq)));
        journal.seq = seq;
        journal.used_room_tx = true;
        if canonical_requires_full_overlay {
            journal.used_full_overlay_tx = true;
        }
        if advances_coord_ack {
            journal.coord_ack_seq = seq;
        }
        journal.latest.insert(
            site,
            CommittedShift {
                id,
                stamp: (phase, first_ts, seq),
                request_digest: digest,
                payload: CommittedPayload::RoomTx(canonical.clone()),
            },
        );
        drop(journal);
        drop(wm);
        let msg = Msg::RoomTxCommitted {
            site,
            id,
            phase,
            first_ts,
            seq,
            patch: canonical.clone(),
        };
        queue_room_change(&self.cmd, &self.roster, None, RoomChange::Message(msg));
        let _ = self.events.send(NetEvent::RoomTxCommitted {
            site,
            id,
            phase,
            seq,
            patch: canonical,
            snapshot: false,
        });
        emit_room_watermark(&self.cmd, &self.roster, seq, revision);
        Ok(())
    }

    pub(super) fn capture_intro<'a>(
        &'a self,
        mut order: MutexGuard<'a, ()>,
        site: u32,
        capable: bool,
        room_tx_capable: bool,
        full_tx_overlay_capable: bool,
    ) -> Result<(MutexGuard<'a, ()>, ShiftIntro), String> {
        for _ in 0..4 {
            let (seq, coord_ack_seq, used_room_tx, used_full_overlay_tx, receipt) = {
                let j = lock_unpoison(&self.transactions);
                let rec = j.latest.get(&site).map(|receipt| {
                    let (phase, first_ts, rseq) = receipt.stamp;
                    match &receipt.payload {
                        CommittedPayload::CellShift(patch) => Msg::CellShiftReceipt {
                            site,
                            id: receipt.id,
                            phase,
                            first_ts,
                            seq: rseq,
                            patch: patch.clone(),
                        },
                        CommittedPayload::RoomTx(patch) => Msg::RoomTxReceipt {
                            site,
                            id: receipt.id,
                            phase,
                            first_ts,
                            seq: rseq,
                            patch: patch.clone(),
                        },
                    }
                });
                (
                    j.seq,
                    j.coord_ack_seq,
                    j.used_room_tx,
                    j.used_full_overlay_tx,
                    rec,
                )
            };
            if used_full_overlay_tx && !full_tx_overlay_capable {
                return Err("该房间已执行行列移动并依赖完整元数据快照，请升级客户端后加入".into());
            }
            if used_room_tx && !room_tx_capable {
                return Err("该房间已使用新版协作事务，请升级客户端后加入".into());
            }
            if seq == 0 {
                if !capable {
                    return Ok((order, ShiftIntro::default()));
                }
                // Name-manager edits are local workbook changes and need not advance the room clock.
                // A live host must snapshot its current metadata even before the first shift.
                if !self.has_app.load(Ordering::Acquire) {
                    let overlay = lock_unpoison(&self.seed).as_ref()
                        .filter(|(wm, _)| *wm == lock_unpoison(&self.wm).seq)
                        .map(|(_, overlay)| overlay.clone());
                    return Ok((order, ShiftIntro { overlay, ..Default::default() }));
                }
            }
            if coord_ack_seq != 0 && !capable {
                return Err("该房间已使用局部插删，请升级客户端后加入".into());
            }
            if !capable {
                return Ok((order, ShiftIntro::default()));
            }
            let wm = lock_unpoison(&self.wm).seq;
            let (tx, rx) = channel();
            self.events
                .send(NetEvent::CaptureCellShiftOverlay(tx))
                .map_err(|_| "宿主已关闭".to_string())?;
            drop(order);
            let overlay = rx
                .recv_timeout(Duration::from_secs(12))
                .map_err(|_| "无法取得宿主元数据快照，请重试".to_string())?;
            order = lock_unpoison(&self.order);
            if lock_unpoison(&self.wm).seq == wm {
                return Ok((
                    order,
                    ShiftIntro {
                        overlay: Some(Arc::new(overlay)),
                        receipt,
                        seq: coord_ack_seq,
                    },
                ));
            }
        }
        Err("房间持续更新，请稍后重试".into())
    }
}

#[derive(Default)]
pub(super) struct ShiftIntro {
    overlay: Option<Arc<CellShiftOverlay>>,
    receipt: Option<Msg>,
    seq: u64,
}
impl ShiftIntro {
    pub(super) fn send(&self, tx: &std::sync::mpsc::SyncSender<IntroFrame>) -> bool {
        let Some(overlay) = &self.overlay else {
            return true;
        };
        let Ok(bytes) = serde_json::to_vec(overlay) else {
            return false;
        };
        let chunks = bytes.chunks(512 * 1024);
        let parts = chunks.len();
        if parts > 4096 {
            return false;
        }
        for (n, buf) in chunks.enumerate() {
            let msg = Msg::CellShiftOverlayPart {
                seq: self.seq,
                index: n as u32,
                last: n + 1 == parts,
                data: hex::encode(buf),
            };
            if tx
                .send(IntroFrame::Line(serde_json::to_string(&msg).unwrap()))
                .is_err()
            {
                return false;
            }
        }
        self.receipt.as_ref().is_none_or(|r| {
            tx.send(IntroFrame::Line(serde_json::to_string(r).unwrap()))
                .is_ok()
        })
    }
}

impl Net {
    pub(crate) fn finish_unconfirmed_axis_structure(&self) {
        if lock_unpoison(&self.shift_pending).is_none()
            && lock_unpoison(&self.room_tx_pending).is_none()
        {
            self.pending_structure.store(false, Ordering::Release);
        }
    }
    pub(crate) fn enable_cell_shift_authority(&self) {
        if let Some(host) = &self.shift_host {
            host.has_app.store(true, Ordering::Release);
        }
    }
    pub(super) fn host_shift_pending_ack(&self) -> bool {
        self.shift_host
            .as_ref()
            .is_some_and(|host| host.needs_ack(self.site, 0))
    }
    pub(crate) fn ack_cell_shift(&self, seq: u64) {
        if let Some(host) = &self.shift_host {
            host.ack(self.site, 0, seq);
            return;
        }

        if !self.hosting && self.is_live() && seq != 0 {
            let _ = self.cmd.send(Cmd::GuestStructure(
                self.connection_generation.load(Ordering::Acquire),
                Msg::CellShiftAck {
                    site: self.site,
                    seq,
                },
            ));
        }
    }
    pub(crate) fn request_cell_shift(
        &self,
        id: u64,
        phase: CellShiftPhase,
        patch: Arc<CellShiftPatch>,
    ) -> Result<(), String> {
        if !self.is_live() || !self.cell_shift_cap.load(Ordering::Acquire) {
            return Err("协作连接不可用或宿主版本不支持局部插删".into());
        }
        if id == 0 || !patch.wire_valid() {
            return Err("无效的单元格移动操作".into());
        }
        fingerprint(&patch)?;
        if self.pending_structure.swap(true, Ordering::AcqRel) {
            return Err("正在等待宿主确认上一项结构操作".into());
        }
        *lock_unpoison(&self.shift_pending) = Some((id, phase, patch.clone()));
        let revision = self.structure_seen();
        if let Some(host) = self.shift_host.clone() {
            let site = self.site;
            let pending = self.shift_pending.clone();
            std::thread::spawn(move || {
                if let Err(reason) =
                    host.validate_and_commit(site, id, phase, revision, patch, None)
                {
                    reject_pending_shift(&host.events, &pending, id, phase, reason);
                }
            });
            return Ok(());
        }
        let msg = Msg::CellShiftRequest {
            site: self.site,
            id,
            phase,
            revision,
            patch,
        };
        if self
            .cmd
            .send(Cmd::GuestStructure(
                self.connection_generation.load(Ordering::Acquire),
                msg,
            ))
            .is_err()
        {
            self.finish_structure();
            return Err("协作发送队列已关闭".into());
        }
        Ok(())
    }

    pub(crate) fn ack_room_tx(&self, seq: u64) {
        if let Some(host) = &self.shift_host {
            host.ack(self.site, 0, seq);
            return;
        }
        if !self.hosting && self.is_live() && seq != 0 && self.room_tx_cap.load(Ordering::Acquire) {
            let _ = self.cmd.send(Cmd::GuestStructure(
                self.connection_generation.load(Ordering::Acquire),
                Msg::RoomTxAck {
                    site: self.site,
                    seq,
                },
            ));
        }
    }

    pub(crate) fn request_room_tx(
        &self,
        id: u64,
        phase: CellShiftPhase,
        patch: Arc<RoomTxPatch>,
    ) -> Result<(), String> {
        if !self.is_live() || !self.room_tx_cap.load(Ordering::Acquire) {
            return Err("协作连接不可用或宿主版本不支持此协作事务".into());
        }
        if matches!(*patch, RoomTxPatch::AxisPermute(_))
            && !self.full_tx_overlay_cap.load(Ordering::Acquire)
        {
            return Err("协作宿主版本较旧，不支持完整元数据快照，请升级宿主后再移动行或列".into());
        }
        if id == 0 || !patch.wire_valid() {
            return Err("无效的协作事务操作".into());
        }
        fingerprint_room_tx(&patch)?;
        if self.pending_structure.swap(true, Ordering::AcqRel) {
            return Err("正在等待宿主确认上一项结构操作".into());
        }
        *lock_unpoison(&self.room_tx_pending) = Some((id, phase, patch.clone()));
        let revision = self.structure_seen();
        if let Some(host) = self.shift_host.clone() {
            let site = self.site;
            let pending = self.room_tx_pending.clone();
            std::thread::spawn(move || {
                if let Err(reason) =
                    host.validate_and_commit_room_tx(site, id, phase, revision, patch, None)
                {
                    reject_pending_room_tx(&host.events, &pending, id, phase, reason);
                }
            });
            return Ok(());
        }
        let msg = Msg::RoomTxRequest {
            site: self.site,
            id,
            phase,
            revision,
            patch,
        };
        if self
            .cmd
            .send(Cmd::GuestStructure(
                self.connection_generation.load(Ordering::Acquire),
                msg,
            ))
            .is_err()
        {
            self.finish_structure();
            return Err("协作发送队列已关闭".into());
        }
        Ok(())
    }
}
