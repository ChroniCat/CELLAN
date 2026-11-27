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

pub(super) fn finish_shift_snapshot(events: &EvtTx, pending: &ShiftPending, got_receipt: bool) {
    if !got_receipt && lock_unpoison(pending).take().is_some() {
        let _ = events.send(NetEvent::StructuralRejected(
            "重连后宿主未确认此次局部插删，已恢复房间状态，可重新执行".into(),
        ));
    }
    let _ = events.send(NetEvent::SnapshotDone);
}

type Stamp = (CellShiftPhase, u64, u64);
pub(super) type ShiftPending = Arc<Mutex<Option<(u64, CellShiftPhase, Arc<CellShiftPatch>)>>>;

struct CommittedShift {
    id: u64,
    stamp: Stamp,
    request_digest: [u8; 32],
    patch: Arc<CellShiftPatch>,
}

#[derive(Default)]
pub(super) struct ShiftJournal {
    entries: HashMap<(u32, u64), ([u8; 32], Stamp)>,
    seq: u64,
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
    pub(super) t0: std::time::Instant,
    pub(super) state: SharedGrid<Op>,
    pub(super) delta: SharedGrid<Op>,
    pub(super) formula_keys: Arc<Mutex<BTreeSet<(usize, u32, u32)>>>,
    pub(super) styles: SharedStyles,
    pub(super) delta_styles: SharedStyles,
    pub(super) threads: HostThreads,
    pub(super) merges: Arc<Mutex<Vec<(usize, u32, u32, u32, u32, bool)>>>,
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

impl CellShiftHost {
    pub(super) fn ack(&self, site: u32, conn: u64, seq: u64) {
        let mut journal = lock_unpoison(&self.transactions);
        if seq == journal.seq {
            journal.acks.insert(site, (conn, seq));
        }
    }
    pub(super) fn needs_ack(&self, site: u32, conn: u64) -> bool {
        let journal = lock_unpoison(&self.transactions);
        journal.seq != 0 && journal.acks.get(&site) != Some(&(conn, journal.seq))
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
        let seq = wm.bump();
        let mut journal = lock_unpoison(&self.transactions);
        journal
            .entries
            .insert((site, id), (canonical_digest, (phase, first_ts, seq)));
        journal.seq = seq;
        journal.latest.insert(
            site,
            CommittedShift {
                id,
                stamp: (phase, first_ts, seq),
                request_digest: digest,
                patch: canonical.clone(),
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
        emit_room_watermark(&self.cmd, &self.roster, seq, revision);
        Ok(())
    }

    pub(super) fn capture_intro<'a>(
        &'a self,
        mut order: MutexGuard<'a, ()>,
        site: u32,
        capable: bool,
    ) -> Result<(MutexGuard<'a, ()>, ShiftIntro), String> {
        for _ in 0..4 {
            let (seq, receipt) = {
                let j = lock_unpoison(&self.transactions);
                (
                    j.seq,
                    j.latest
                        .get(&site)
                        .map(|receipt| (receipt.id, receipt.stamp, receipt.patch.clone())),
                )
            };
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
            if !capable {
                return Err("该房间已使用局部插删，请升级客户端后加入".into());
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
                        receipt: receipt.map(|(id, (phase, first_ts, seq), patch)| {
                            Msg::CellShiftReceipt {
                                site,
                                id,
                                phase,
                                first_ts,
                                seq,
                                patch,
                            }
                        }),
                        seq,
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
        if lock_unpoison(&self.shift_pending).is_none() {
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
}
