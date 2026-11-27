//! Sparse and deferred range paste, including background XMLSS results.

use crate::{sheet_edit_target::SheetEditTarget, SheetApp};
use sheetcore::{Cell, Value};
use std::sync::mpsc;

type ParsedPaste = Result<sheetcore::clipboard_xmlss::SparseBlock, String>;

pub(crate) struct BackgroundPaste {
    target: SheetEditTarget,
    gate: crate::bg_gate::CoordGate,
    values_only: bool,
    result: mpsc::Receiver<ParsedPaste>,
}

/// Maps compact clipboard rows to contiguous visible runs. Storage is proportional to
/// the number of hidden intervals, including for a mostly empty million-row clipboard.
pub(super) struct PasteRows {
    runs: Vec<(u32, u32, u32)>, // first clipboard offset, first/last physical rows
    filtered: bool,
}

impl PasteRows {
    fn new(sh: &sheetcore::Sheet, start: u32, height: u32, skip_filtered: bool) -> Option<Self> {
        let end = start.checked_add(height.checked_sub(1)?)?;
        if end >= sheetcore::sync::MAX_ROWS {
            return None;
        }
        if !skip_filtered || height == 1 || !sh.hidden_rows.intersects_range(start, end) {
            return Some(Self {
                runs: vec![(0, start, end)],
                filtered: false,
            });
        }
        let hidden = sheetcore::layout::HiddenIndices::from_intervals(
            sh.hidden_rows
                .intervals()
                .iter()
                .chain(sh.hidden_rows_user.intervals())
                .copied(),
        );
        let mut runs = Vec::new();
        let mut used = 0;
        for (first, last) in hidden.complement_in(start, sheetcore::sync::MAX_ROWS - 1) {
            let take = (last - first + 1).min(height - used);
            runs.push((used, first, first + take - 1));
            used += take;
            if used == height {
                return Some(Self {
                    runs,
                    filtered: true,
                });
            }
        }
        None
    }

    pub(super) fn at(&self, offset: u32) -> u32 {
        let i = self.runs.partition_point(|&(base, _, _)| base <= offset) - 1;
        let (base, first, _) = self.runs[i];
        first + (offset - base)
    }
    pub(super) fn contains(&self, row: u32) -> bool {
        let i = self.runs.partition_point(|&(_, first, _)| first <= row);
        i > 0 && self.runs[i - 1].2 >= row
    }
    fn rects(&self, left: u32, right: u32) -> impl Iterator<Item = (u32, u32, u32, u32)> + '_ {
        self.runs
            .iter()
            .map(move |&(_, first, last)| (first, left, last, right))
    }
    fn clear(&self, sh: &mut sheetcore::Sheet, left: u32, right: u32, values_only: bool) {
        for &(_, top, bottom) in &self.runs {
            sh.clear_cells_rect(top, left, bottom, right);
        }
        if !values_only {
            let runs: Vec<_> = self.runs.iter().map(|&(_,first,last)| (first,last)).collect();
            sh.clear_styles_in_row_runs(left,right,&runs);
        }
    }

    fn preflight(
        &self,
        app: &mut SheetApp,
        si: usize,
        left: u32,
        right: u32,
        keys: &[(u32, u32)],
        label: &str,
    ) -> bool {
        if self.runs.len() == 1 {
            let (_, first, last) = self.runs[0];
            return app.check_sparse_rect_preflight(si, (first, left, last, right), keys, label);
        }
        app.write_preflight_on(si, keys, label)
            && self
                .rects(left, right)
                .all(|rect| app.check_sparse_rect_preflight(si, rect, &[], label))
    }
}

impl SheetApp {
    pub(crate) fn begin_background_paste(&mut self, values_only: bool) -> mpsc::Sender<ParsedPaste> {
        let (tx, result) = mpsc::channel();
        let gate = crate::bg_gate::CoordGate::sheet(self.doc_uid, &self.wb, self.sheet_idx)
            .unwrap_or_else(|| crate::bg_gate::CoordGate::workbook(self.doc_uid, &self.wb));
        self.bg_paste = Some(BackgroundPaste {
            target: SheetEditTarget::current(self, self.sel),
            gate,
            values_only,
            result,
        });
        tx
    }

    pub(crate) fn poll_background_paste(&mut self) {
        let Some(paste) = &self.bg_paste else { return };
        let result = match paste.result.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return,
            Err(mpsc::TryRecvError::Disconnected) => {
                self.bg_paste = None;
                self.status = "后台粘贴任务已中断".into();
                return;
            }
        };
        // A room commit and a parser completion can arrive between two UI polls.
        // Consume queued coordinate changes before validating the saved anchor.
        let paste = self.bg_paste.take().unwrap();
        if self.net.is_some() { self.poll_net(); }
        let valid = paste.gate.matches_sheet(self.doc_uid, &self.wb) == Some(self.sheet_idx)
            && self.sheet_idx == paste.target.sheet
            && paste.target.valid(self)
            && !self.in_snapshot
            && self.net.as_ref().is_none_or(|net| net.is_live())
            && !self.net.as_ref().is_some_and(crate::net::Net::pending_structure);
        let anchor = paste.target.at;
        let values_only = paste.values_only;
        if !valid {
            self.status = "目标工作表或行列结构已改变，已取消后台粘贴".into();
            return;
        }
        match result {
            Ok(sparse) => {
                let saved = self.sel;
                self.sel = anchor;
                let prior_clock = self.peer.as_ref().map(|p| p.clock);
                if !self.paste_sparse_xmlss_ext(&sparse, values_only) {
                    self.sel = saved;
                } else if let (Some(first), Some(peer)) = (prior_clock, self.peer.as_ref()) {
                    self.track_deferred_paste(first, peer.clock);
                }
            }
            Err(e) => self.status = format!("粘贴解析失败：{e}"),
        }
    }

    /// 内部延迟大选区（含超 50 万格稀疏区域与筛选可见行）稀疏粘贴路径：
    /// 零空格稠密物化、完整记录稀疏 `CellDelta` 并将 `self.last_paste` 置 `None`。
    pub(crate) fn paste_deferred_sparse(&mut self, values_only: bool) {
        use std::collections::{BTreeMap, BTreeSet, HashMap};
        let (r, c) = self.sel;
        let si = self.sheet_idx;
        let (clip_sheet, clip_rect, clip_cut, clip_merges, clip_filtered, range) = {
            let Some(clip) = self.clip.as_ref() else { return };
            let Some(range) = clip.deferred.clone() else { return };
            (
                clip.sheet,
                clip.rect,
                clip.cut,
                clip.merges.clone(),
                clip.source_rows.is_some(),
                range,
            )
        };
        if clip_cut && self.clip.as_ref().is_none_or(|clip| !self.cut_source_matches(clip)) {
            self.status = "剪切源已更改，请重新剪切".into();
            return;
        }
        let (h, w) = range.dimensions();
        if h == 0 || w == 0 {
            return;
        }
        if r.saturating_add(h - 1) > 1_048_575 || c.saturating_add(w - 1) > 16_383 {
            self.status = "粘贴区域超出工作表行列边界".into();
            return;
        }
        let Some(dest_rows) = PasteRows::new(self.wb.sheet(si), r, h, !clip_cut) else {
            self.status = "粘贴区域超出工作表行列边界".into();
            return;
        };
        let target_rect = (r, c, dest_rows.at(h - 1), c + w - 1);
        let movement = self.clip.as_ref().filter(|clip| clip.cut)
            .map(|clip| super::cut::CutRange::new(clip, &self.wb, si, (r,c)));
        if !values_only && self.net.as_ref().is_some_and(|net| !net.supports_thread_changes()) {
            let carries_threads = self.clip.as_ref().is_some_and(|clip| !self.resolve_clip_notes_for_paste(clip).1.is_empty());
            if !self.thread_change_allowed(carries_threads) || !dest_rows.rects(c, target_rect.3).all(|rect| self.thread_rects_allowed(si, &[rect]))
                || (clip_cut && !self.thread_rects_allowed(clip_sheet, &[clip_rect])) { return; }
        }
        let uncapped_cow = self.net.is_none() && !clip_cut;
        let sparse_entries = match range.sparse_entries_with_limit(
            (!uncapped_cow).then_some(sheetcore::undo::MAX_DENSE_RECT_KEYS),
        ) {
            Ok(v) => v,
            Err(msg) => {
                self.status = msg.into();
                return;
            }
        };

        // Fast CoW batch path for non-collab deferred sparse pastes (no 500k cap, no 500k BTreeSet/CellDelta).
        if uncapped_cow
            && (sparse_entries.len() > sheetcore::undo::COW_BATCH_THRESHOLD
                || u64::from(h) * u64::from(w) > sheetcore::undo::COW_BATCH_THRESHOLD as u64)
        {
            if !dest_rows.preflight(self, si, c, target_rect.3, &[], "粘贴") {
                return;
            }
            let (sr1, sc1, _, _) = clip_rect;
            let dr = r as i64 - sr1 as i64;
            let dc = c as i64 - sc1 as i64;
            let pasted_merges: Vec<(u32, u32, u32, u32)> = if values_only || clip_merges.is_empty() {
                Vec::new()
            } else {
                clip_merges
                    .iter()
                    .map(|&(a, b, c2, d)| {
                        (
                            (a as i64 + dr).max(0) as u32,
                            (b as i64 + dc).max(0) as u32,
                            (c2 as i64 + dr).max(0) as u32,
                            (d as i64 + dc).max(0) as u32,
                        )
                    })
                    .collect()
            };
            let merge_edit = if values_only || dest_rows.filtered {
                None
            } else {
                match self.paste_merge_edit(target_rect, &pasted_merges, None) {
                    Ok(edit) => edit,
                    Err(message) => {
                        self.status = message.into();
                        return;
                    }
                }
            };
            let before_sheet = self.wb.sheet(si).clone();
            if let Some(edit) = &merge_edit {
                if edit.apply(&mut self.wb.sheets, true).is_none() {
                    self.status = "粘贴区域中的合并单元格相互冲突".into();
                    return;
                }
            }
            dest_rows.clear(self.wb.sheet_mut(si), c, target_rect.3, values_only);
            let target_sheet_name = self.wb.sheet(si).name.clone();
            let mut style_map: HashMap<u16, u16> = HashMap::new();
            let mut fill_cache = sheetcore::refs::FillFormulas::default();
            for (rel_r, rel_c, src_r, cell_opt, src_sid) in sparse_entries {
                let rr = dest_rows.at(rel_r);
                let cc = c + rel_c;
                if let Some(mut cell) = cell_opt {
                    if !values_only {
                        let row_shift = rr as i64 - src_r as i64;
                        if let Some(f) = cell.formula.as_deref() {
                            let nf: std::sync::Arc<str> = if !f.contains('!') {
                                fill_cache.translate(f, row_shift, dc).into()
                            } else {
                                sheetcore::refs::translate(f, row_shift, dc, &target_sheet_name).into()
                            };
                            cell.formula = Some(nf);
                        }
                    } else {
                        cell.formula = None;
                    }
                    if !cell.is_blank() {
                        self.wb.sheet_mut(si).set(rr, cc, cell);
                    }
                }
                if !values_only && src_sid != 0 {
                    let mapped_sid = *style_map.entry(src_sid).or_insert_with(|| {
                        self.wb
                            .styles
                            .intern(range.book().styles.get(src_sid).clone())
                    });
                    self.wb.sheet_mut(si).set_style(rr, cc, mapped_sid);
                }
            }
            if !values_only {
                let (notes, threads) = self.resolve_clip_notes_for_paste(self.clip.as_ref().unwrap());
                self.apply_pasted_notes(si, (r,c), Some(super::notes::NoteRows::Sparse(&dest_rows)),
                    h, w, h, w, &notes, &threads, None);
            }
            let label = if values_only { "粘贴数值" } else { "粘贴" };
            self.undo
                .push_cow_batch(label, si, &before_sheet, self.wb.sheet(si), (si, r, c));
            self.last_paste = None;
            self.wb.dirty = true;
            self.doc_gen = self.doc_gen.wrapping_add(1);
            self.tasks.last_edit = Some(std::time::Instant::now());
            self.wb.calc_manual |= self.manual_calc;
            self.recalc_all_and_deps_adaptive();
            self.status = format!("已粘贴 {h} 行 × {w} 列");
            std::thread::spawn(move || drop(before_sheet));
            return;
        }
        let cut_keys = if clip_cut {
            match range.sparse_cut_source_keys() {
                Ok(keys) => Some((clip_sheet, keys)),
                Err(msg) => {
                    self.status = msg.into();
                    return;
                }
            }
        } else {
            None
        };

        let max_keys = sheetcore::undo::MAX_DENSE_RECT_KEYS;
        let mut target_coord_set: BTreeSet<(u32, u32)> = BTreeSet::new();
        for &(rel_r, rel_c, _, _, _) in &sparse_entries {
            target_coord_set.insert((dest_rows.at(rel_r), c + rel_c));
        }
        {
            let target_sh = self.wb.sheet(si);
            for (k, _) in target_sh.iter_rect(target_rect) {
                if !dest_rows.contains(k.row) { continue; }
                target_coord_set.insert((k.row, k.col));
                if target_coord_set.len() > max_keys {
                    self.status = "粘贴内容过大（单次实际变更最多支持 50 万个单元格）".into();
                    return;
                }
            }
            if !values_only {
                for (k, _) in target_sh.styles.iter_rect(target_rect) {
                    if !dest_rows.contains(k.row) { continue; }
                    target_coord_set.insert((k.row, k.col));
                    if target_coord_set.len() > max_keys {
                        self.status = "粘贴内容过大（单次实际变更最多支持 50 万个单元格）".into();
                        return;
                    }
                }
            }
            for &k in &target_sh.spilled {
                if k.row >= target_rect.0
                    && k.row <= target_rect.2
                    && k.col >= target_rect.1
                    && k.col <= target_rect.3
                {
                    if !dest_rows.contains(k.row) { continue; }
                    target_coord_set.insert((k.row, k.col));
                }
            }
        }
        let target_keys: Vec<(u32, u32)> = target_coord_set.iter().copied().collect();
        if !dest_rows.preflight(self, si, c, target_rect.3, &target_keys, "粘贴") {
            return;
        }
        if let Some((src_si, ref src_keys)) = cut_keys {
            if !clip_filtered {
                if !self.check_sparse_rect_preflight(src_si, clip_rect, src_keys, "剪切") {
                    return;
                }
            } else if !self.write_preflight_on(src_si, src_keys, "剪切") {
                return;
            }
        }

        let (sr1, sc1, _, _) = clip_rect;
        let dr = r as i64 - sr1 as i64;
        let dc = c as i64 - sc1 as i64;
        let pasted_merges: Vec<(u32, u32, u32, u32)> = if values_only || clip_merges.is_empty() {
            Vec::new()
        } else {
            clip_merges
                .iter()
                .map(|&(a, b, c2, d)| {
                    (
                        (a as i64 + dr).max(0) as u32,
                        (b as i64 + dc).max(0) as u32,
                        (c2 as i64 + dr).max(0) as u32,
                        (d as i64 + dc).max(0) as u32,
                    )
                })
                .collect()
        };
        let merge_edit = if values_only || dest_rows.filtered {
            None
        } else {
            let cut_clip_ref = self.clip.as_ref().filter(|_| cut_keys.is_some());
            match self.paste_merge_edit(target_rect, &pasted_merges, cut_clip_ref) {
                Ok(edit) => edit,
                Err(message) => {
                    self.status = message.into();
                    return;
                }
            }
        };
        let rewrites = movement.as_ref().map_or_else(Vec::new, |m|
            m.external_formulas(&self.wb, si, (target_rect.2,target_rect.3)));
        let mut move_refs = movement.as_ref().map_or_else(Default::default, |m| m.workbook_references(&self.wb));
        if self.net.is_some() && !move_refs.is_empty() {
            self.status = "当前协作协议不能同步名称或图表引用，请断开协作后再剪切粘贴".into();
            return;
        }
        let mut ref_keys: BTreeMap<usize, Vec<(u32,u32)>> = BTreeMap::new();
        for &(sheet, row, col, _) in &rewrites { ref_keys.entry(sheet).or_default().push((row,col)); }
        if ref_keys.iter().any(|(&sheet, keys)| !self.write_preflight_on(sheet, keys, "剪切")) { return; }
        let merge_changes = if let Some(edit) = &merge_edit {
            let Some(changes) = edit.apply(&mut self.wb.sheets, true) else {
                self.status = "粘贴区域中的合并单元格相互冲突".into();
                return;
            };
            changes
        } else {
            Vec::new()
        };

        let mut transactions: BTreeMap<usize, Vec<(u32, u32)>> = BTreeMap::new();
        transactions.insert(si, target_keys);
        if let Some((src_si, ref src_keys)) = cut_keys {
            let entry = transactions.entry(src_si).or_default();
            entry.extend_from_slice(src_keys);
            entry.sort_unstable();
            entry.dedup();
        }
        for (sheet, keys) in ref_keys {
            transactions.entry(sheet).or_default().extend(keys);
        }
        let before: Vec<_> = transactions
            .iter()
            .map(|(&sheet, keys)| (sheet, sheetcore::undo::capture(self.wb.sheet(sheet), sheet, keys)))
            .collect();

        if let Some((src_si, ref src_keys)) = cut_keys {
            for &(rr, cc) in src_keys {
                self.wb.sheet_mut(src_si).set(rr, cc, Cell::default());
                self.wb.sheet_mut(src_si).set_style(rr, cc, 0);
            }
        }
        // 先清空目标矩形中原本存在而本次可能被空单元格覆盖的稀疏旧值与显式样式
        for &(rr, cc) in &target_coord_set {
            self.wb.sheet_mut(si).set(rr, cc, Cell::default());
            if !values_only {
                let inherited = self
                    .wb
                    .sheet(si)
                    .row_styles
                    .get(&rr)
                    .or_else(|| self.wb.sheet(si).col_styles.get(&cc))
                    .copied()
                    .unwrap_or(0);
                self.wb.sheet_mut(si).set_style(rr, cc, inherited);
            }
        }

        let target_sheet_name = self.wb.sheet(si).name.clone();
        let mut style_map: HashMap<u16, u16> = HashMap::new();
        for (rel_r, rel_c, src_r, cell_opt, src_sid) in sparse_entries {
            let rr = dest_rows.at(rel_r);
            let cc = c + rel_c;
            if let Some(mut cell) = cell_opt {
                if !values_only {
                    let row_shift = rr as i64 - src_r as i64;
                    if let Some(f) = &cell.formula {
                        let nf = match &movement {
                            Some(m) => m.moved_formula(f),
                            None => sheetcore::refs::translate(f, row_shift, dc, &target_sheet_name),
                        };
                        cell.formula = Some(nf.into());
                    }
                } else {
                    cell.formula = None;
                }
                if !cell.is_blank() {
                    self.wb.sheet_mut(si).set(rr, cc, cell);
                }
            }
            if !values_only && src_sid != 0 {
                let mapped_sid = *style_map.entry(src_sid).or_insert_with(|| {
                    self.wb
                        .styles
                        .intern(range.book().styles.get(src_sid).clone())
                });
                self.wb.sheet_mut(si).set_style(rr, cc, mapped_sid);
            }
        }

        for (sheet, row, col, formula) in rewrites {
            if let Some(mut cell) = self.wb.sheet(sheet).get(row,col).map(|v| v.into_owned()) {
                cell.formula = Some(formula.into());
                self.wb.sheet_mut(sheet).set(row,col,cell);
            }
        }
        let names_changed = move_refs.replay(&mut self.wb, true);
        let deferred_note_edit = if !values_only {
            let (cn, ct, cut_rows) = if let Some(clip) = self.clip.as_ref() {
                let (cn, ct) = self.resolve_clip_notes_for_paste(clip);
                (cn, ct, clip.source_rows.clone())
            } else {
                (Vec::new(), Vec::new(), None)
            };
            self.apply_pasted_notes(
                si,
                (r, c),
                Some(super::notes::NoteRows::Sparse(&dest_rows)),
                h,
                w,
                h,
                w,
                &cn,
                &ct,
                clip_cut.then_some((clip_sheet, clip_rect, cut_rows.as_deref())),
            )
        } else {
            sheetcore::undo::NoteUndoEdit::default()
        };
        let label = if cut_keys.is_some() {
            "剪切粘贴"
        } else if values_only {
            "粘贴数值"
        } else {
            "粘贴"
        };
        let mut deltas = Vec::new();
        let mut touched = Vec::new();
        for (sheet, captured) in before {
            let changes = sheetcore::undo::finish(captured, self.wb.sheet(sheet));
            if !changes.is_empty() {
                for d in &changes {
                    touched.push((d.sheet, d.row, d.col));
                }
                self.record_revision(label, &changes);
                deltas.extend(changes);
            }
        }
        if self.net.is_some() || self.room_lost {
            let styles: Vec<_> = deltas
                .iter()
                .filter(|d| d.before_style != d.after_style)
                .map(|d| (d.sheet, d.row, d.col))
                .collect();
            self.broadcast_cell_styles(&styles);
            for (sheet, rect, on) in merge_changes {
                self.queue_or_broadcast_merge(sheet, rect, on);
            }
        }
        if !deferred_note_edit.is_empty() {
            self.broadcast_note_undo_edit(&deferred_note_edit);
        }
        self.undo.push_with_move_references(
            sheetcore::undo::Action {
                label: label.into(),
                deltas,
                focus: (si, r, c),
                merges: merge_edit,
                snap_before: None,
                snap_after: None,
                by_site: 0,
                pictures: None,
                shapes: None,
                refresh_sizes: None,
            },
            (!deferred_note_edit.is_empty()).then_some(deferred_note_edit), move_refs,
        );
        if cut_keys.is_some() {
            self.clear_internal_clip();
        }
        if names_changed {
            self.recalc_all_and_deps_adaptive();
            self.broadcast_cells(&touched);
        } else {
            self.recalc_and_broadcast(&touched);
        }
        self.last_paste = None;
        self.wb.dirty = true;
        self.doc_gen = self.doc_gen.wrapping_add(1);
        self.tasks.last_edit = Some(std::time::Instant::now());
        self.status = format!("已粘贴 {h} 行 × {w} 列");
    }

    /// 外部超 50 万格稀疏 XML Spreadsheet 与外部 Excel 整列（`ExpandedRowCount="1048576"`）富格式粘贴路径。
    pub(crate) fn paste_sparse_xmlss(
        &mut self,
        block: &sheetcore::clipboard_xmlss::SparseBlock,
    ) -> bool {
        self.paste_sparse_xmlss_ext(block, false)
    }

    pub(crate) fn paste_sparse_xmlss_ext(
        &mut self,
        block: &sheetcore::clipboard_xmlss::SparseBlock,
        values_only: bool,
    ) -> bool {
        use sheetcore::clipboard_xmlss::Kind;
        use std::collections::{BTreeSet, HashMap};
        if block.rows == 0 || block.cols == 0 {
            return false;
        }
        let (r0, c0) = self.sel;
        if block.full_columns && r0 != 0 {
            self.status = "整列粘贴须从首行开始，且目标列不能超出工作表边界".into();
            return true;
        }
        if u64::from(r0) + u64::from(block.rows) > 1_048_576
            || u64::from(c0) + u64::from(block.cols) > 16_384
        {
            self.status = "粘贴区域超过工作表边界".into();
            return true;
        }
        let si = self.sheet_idx;
        let Some(dest_rows) = PasteRows::new(self.wb.sheet(si), r0, block.rows, true) else {
            self.status = "粘贴区域超过工作表边界".into();
            return true;
        };
        let target_rect = (r0, c0, dest_rows.at(block.rows-1), c0 + block.cols - 1);
        let max_keys = sheetcore::undo::MAX_DENSE_RECT_KEYS;
        let use_cow = self.net.is_none()
            && (block.cells.len() > sheetcore::undo::COW_BATCH_THRESHOLD
                || u64::from(block.rows) * u64::from(block.cols)
                    > sheetcore::undo::COW_BATCH_THRESHOLD as u64);
        let label = if values_only { "粘贴数值" } else { "从 Excel 粘贴" };

        let mut coord_set: BTreeSet<(u32, u32)> = BTreeSet::new();
        let sparse_keys: Vec<(u32, u32)> = if use_cow {
            if !dest_rows.preflight(self, si, c0, target_rect.3, &[], label) {
                return true;
            }
            Vec::new()
        } else {
            for sc in &block.cells {
                coord_set.insert((dest_rows.at(sc.row), c0 + sc.col));
            }
            {
                let target_sh = self.wb.sheet(si);
                for (k, _) in target_sh.iter_rect(target_rect) {
                    if !dest_rows.contains(k.row) { continue; }
                    coord_set.insert((k.row, k.col));
                    if coord_set.len() > max_keys {
                        self.status = "粘贴区域实际变更超过 50 万格，请分批粘贴".into();
                        return true;
                    }
                }
                if !values_only {
                    for (k, _) in target_sh.styles.iter_rect(target_rect) {
                        if !dest_rows.contains(k.row) { continue; }
                        coord_set.insert((k.row, k.col));
                        if coord_set.len() > max_keys {
                            self.status = "粘贴区域实际变更超过 50 万格，请分批粘贴".into();
                            return true;
                        }
                    }
                }
                for &k in &target_sh.spilled {
                    if k.row >= target_rect.0
                        && k.row <= target_rect.2
                        && k.col >= target_rect.1
                        && k.col <= target_rect.3
                    {
                        if !dest_rows.contains(k.row) { continue; }
                        coord_set.insert((k.row, k.col));
                    }
                }
            }
            let keys: Vec<(u32, u32)> = coord_set.iter().copied().collect();
            if !dest_rows.preflight(self, si, c0, target_rect.3, &keys, label) {
                return true;
            }
            keys
        };

        let merge_edit = if values_only || dest_rows.filtered {
            None
        } else {
            let mut incoming_merges = Vec::new();
            for sc in &block.cells {
                if sc.colspan > 1 || sc.rowspan > 1 {
                    let r = dest_rows.at(sc.row);
                    let c = c0 + sc.col;
                    incoming_merges.push((
                        r,
                        c,
                        r + sc.rowspan.max(1) - 1,
                        c + sc.colspan.max(1) - 1,
                    ));
                }
            }
            match self.paste_merge_edit(target_rect, &incoming_merges, None) {
                Ok(edit) => edit,
                Err(message) => {
                    self.status = message.into();
                    return true;
                }
            }
        };
        let before_cow = use_cow.then(|| self.wb.sheet(si).clone());
        if let Some(edit) = &merge_edit {
            let Some(changes) = edit.apply(&mut self.wb.sheets, true) else {
                self.status = "粘贴区域中的合并单元格相互冲突".into();
                return true;
            };
            if self.net.is_some() || self.room_lost {
                for (sheet, rect, on) in changes {
                    self.queue_or_broadcast_merge(sheet, rect, on);
                }
            }
        }

        let txn = if use_cow {
            Vec::new()
        } else {
            sheetcore::undo::capture(self.wb.sheet(si), si, &sparse_keys)
        };
        if use_cow {
            dest_rows.clear(self.wb.sheet_mut(si), c0, target_rect.3, values_only);
        } else {
            // 先稀疏清理目标矩形内原本存在的非空单元格（非仅数值粘贴时同步复位显式样式）
            for &(rr, cc) in &coord_set {
                self.wb.sheet_mut(si).set(rr, cc, Cell::default());
                if !values_only {
                    let inherited = self
                        .wb
                        .sheet(si)
                        .row_styles
                        .get(&rr)
                        .or_else(|| self.wb.sheet(si).col_styles.get(&cc))
                        .copied()
                        .unwrap_or(0);
                    self.wb.sheet_mut(si).set_style(rr, cc, inherited);
                }
            }
        }

        let mut style_cache: HashMap<(u16, u32), Option<u16>> = HashMap::new();
        let parse_office_num = |s: &str| -> Result<f64, std::num::ParseFloatError> {
            if s.as_bytes().contains(&b',') {
                s.replace(',', "").parse::<f64>()
            } else {
                s.parse::<f64>()
            }
        };

        // Write in `(col, row)` column-major order so writes land in contiguous 256-row `ColSlab` pages.
        let mut order: Vec<usize> = (0..block.cells.len()).collect();
        order.sort_unstable_by_key(|&idx| {
            let sc = &block.cells[idx];
            (sc.col, sc.row)
        });

        for idx in order {
            let sc = &block.cells[idx];
            let r = dest_rows.at(sc.row);
            let c = c0 + sc.col;
            let formula = if values_only {
                None
            } else {
                sc.r1c1_formula
                    .as_deref()
                    .map(|f| sheetcore::clipboard_xmlss::r1c1_to_a1(f, r, c))
            };
            let text = sc.text.trim();
            let value = if formula.is_some() {
                match sc.kind {
                    Kind::Number => parse_office_num(text)
                        .map(Value::Number)
                        .unwrap_or(Value::Empty),
                    Kind::Bool => Value::Bool(text == "1" || text.eq_ignore_ascii_case("true")),
                    Kind::Error => sheetcore::formula::ErrKind::from_text(text)
                        .map(Value::Error)
                        .unwrap_or_else(|| Value::Text(text.into())),
                    Kind::DateTime => sheetcore::clipboard_xmlss::iso_to_serial(self.wb.date_system, text)
                        .map(|(serial, _)| Value::Number(serial))
                        .unwrap_or_else(|| Value::Text(text.into())),
                    Kind::Text => {
                        if text.is_empty() {
                            Value::Empty
                        } else {
                            Value::Text(text.into())
                        }
                    }
                }
            } else if text.is_empty() {
                Value::Empty
            } else {
                match sc.kind {
                    Kind::Error => sheetcore::formula::ErrKind::from_text(text)
                        .map(Value::Error)
                        .unwrap_or_else(|| Value::Text(text.into())),
                    Kind::DateTime => {
                        match sheetcore::clipboard_xmlss::iso_to_serial(self.wb.date_system, text) {
                            Some((serial, _)) => {
                                if !values_only {
                                    let cell_sty = block
                                        .styles
                                        .get(sc.style_idx as usize)
                                        .cloned()
                                        .unwrap_or_default();
                                    if cell_sty.num_format.trim().is_empty() {
                                        let sid = self.wb.styles.intern(sheetcore::style::CellStyle {
                                            format: "yyyy-mm-dd".into(),
                                            ..Default::default()
                                        });
                                        self.wb.sheet_mut(si).set_style(r, c, sid);
                                    }
                                }
                                Value::Number(serial)
                            }
                            None => Value::Text(text.into()),
                        }
                    }
                    Kind::Bool => Value::Bool(text == "1" || text.eq_ignore_ascii_case("true")),
                    Kind::Number => match parse_office_num(text) {
                        Ok(n) => Value::Number(n),
                        Err(_) => Value::Text(text.into()),
                    },
                    Kind::Text => Value::Text(text.into()),
                }
            };
            if !matches!(value, Value::Empty) || formula.is_some() {
                self.wb.sheet_mut(si).set(
                    r,
                    c,
                    Cell {
                        value,
                        formula: formula.map(|f| f.into()),
                    },
                );
            }

            if !values_only && sc.style_idx != 0 {
                let base_id = self.wb.sheet(si).style_of(r, c);
                let resolved_id = *style_cache
                    .entry((base_id, sc.style_idx))
                    .or_insert_with(|| {
                        let cell = block.styles.get(sc.style_idx as usize)?;
                        let mut st = self.wb.styles.get(base_id).clone();
                        let mut changed = false;
                        if cell.bold != st.bold {
                            st.bold = cell.bold;
                            changed = true;
                        }
                        if cell.italic != st.italic {
                            st.italic = cell.italic;
                            changed = true;
                        }
                        if cell.underline != st.underline {
                            st.underline = cell.underline;
                            changed = true;
                        }
                        if cell.strike != st.strikethrough {
                            st.strikethrough = cell.strike;
                            changed = true;
                        }
                        if let Some(fill) = cell.fill {
                            st.fill = Some(fill);
                            changed = true;
                        }
                        if let Some(color) = cell.color {
                            st.color = color;
                            changed = true;
                        }
                        if cell.halign != 0 && cell.halign != st.halign {
                            st.halign = cell.halign;
                            changed = true;
                        }
                        let mapped_va = match cell.valign { 2 => 1, 1 => 2, _ => 0 };
                        if mapped_va != 0 && mapped_va != st.valign {
                            st.valign = mapped_va;
                            changed = true;
                        }
                        if cell.wrap != st.wrap {
                            st.wrap = cell.wrap;
                            changed = true;
                        }
                        if cell.borders.iter().any(|b| b.is_some()) {
                            for (src_i, dst_i) in [(0, 2), (1, 3), (2, 0), (3, 1)] {
                                if let Some(bc) = cell.borders[src_i] {
                                    st.border[dst_i].style = 1;
                                    st.border[dst_i].color = bc;
                                }
                            }
                            changed = true;
                        } else if let Some(bc) = cell.border {
                            for side in st.border.iter_mut() {
                                if side.style == 0 {
                                    side.style = 1;
                                    side.color = bc;
                                }
                            }
                            changed = true;
                        }
                        if !cell.font.is_empty() {
                            let f = sheetcore::style::normalize_font(&cell.font);
                            if f != st.font {
                                st.font = f;
                                changed = true;
                            }
                        }
                        if cell.size > 0.0 && (cell.size - st.size).abs() > 0.01 {
                            st.size = cell.size;
                            changed = true;
                        }
                        if !cell.num_format.is_empty() && cell.num_format != st.format {
                            st.format = cell.num_format.clone();
                            changed = true;
                        }
                        changed.then(|| self.wb.styles.intern(st))
                    });
                if let Some(id) = resolved_id {
                    self.wb.sheet_mut(si).set_style(r, c, id);
                }
            }
        }

        self.last_paste = None;
        self.doc_gen = self.doc_gen.wrapping_add(1);
        self.tasks.last_edit = Some(std::time::Instant::now());
        if let Some(before_sheet) = before_cow {
            self.undo
                .push_cow_batch(label, si, &before_sheet, self.wb.sheet(si), (si, r0, c0));
            self.wb.calc_manual |= self.manual_calc;
            self.recalc_all_and_deps_adaptive();
            std::thread::spawn(move || drop(before_sheet));
        } else {
            let deltas = sheetcore::undo::finish(txn, self.wb.sheet(si));
            let touched: Vec<(usize, u32, u32)> = deltas
                .iter()
                .filter(|d| d.before != d.after)
                .map(|d| (d.sheet, d.row, d.col))
                .collect();
            self.fast_recalc_cells(&touched);
            if self.net.is_some() || self.room_lost {
                let style_keys: Vec<_> = deltas
                    .iter()
                    .filter(|d| d.before_style != d.after_style)
                    .map(|d| (d.sheet, d.row, d.col))
                    .collect();
                self.broadcast_cell_styles(&style_keys);
            }
            self.record_deltas_and_merges(label, deltas, merge_edit);
            self.broadcast_cells(&touched);
        }
        self.wb.dirty = true;
        self.sel_end = (target_rect.2, target_rect.3);
        self.status = if values_only {
            format!("已粘贴 Excel 数值 {} 行 × {} 列", block.rows, block.cols)
        } else {
            format!(
                "已粘贴 Excel 区域 {} 行 × {} 列（含格式与合并）",
                block.rows, block.cols
            )
        };
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env::TestEnv;

    fn parsed_cell() -> ParsedPaste {
        let xml = r#"<Workbook xmlns:ss="urn:schemas-microsoft-com:office:spreadsheet"><Worksheet><Table><Row><Cell><Data ss:Type="Number">42</Data></Cell></Row></Table></Worksheet></Workbook>"#;
        Ok(sheetcore::clipboard_xmlss::parse_sparse_ext(xml, false).unwrap())
    }

    #[test]
    fn background_paste_cancels_when_another_sheet_takes_the_target_index() {
        let _env = TestEnv::new("bg-paste-deleted-sheet");
        let mut app = SheetApp::new_with_context(&eframe::egui::Context::default(), None);
        let mut replacement = sheetcore::Sheet::new("Keep");
        replacement.set(0, 0, Cell { value: Value::Number(7.0), formula: None });
        app.wb.sheets.push(replacement);
        let tx = app.begin_background_paste(false);
        app.delete_sheet(0);
        assert_eq!(app.wb.sheet(0).name, "Keep");
        let history = app.undo.depth();

        tx.send(parsed_cell()).unwrap();
        app.poll_background_paste();
        assert_eq!(app.wb.sheet(0).value(0, 0), Value::Number(7.0));
        assert_eq!(app.undo.depth(), history);
        assert!(app.status.contains("已取消后台粘贴"));
    }

    #[test]
    fn background_paste_cancels_after_cells_in_its_column_shift() {
        let _env = TestEnv::new("bg-paste-shifted-cells");
        let mut app = SheetApp::new_with_context(&eframe::egui::Context::default(), None);
        app.wb.sheet_mut(0).set(1, 0, Cell { value: Value::Number(7.0), formula: None });
        app.sel = (1, 0);
        let tx = app.begin_background_paste(false);
        app.sel = (0, 0);
        app.sel_end = app.sel;
        app.shift_cells(true, false);
        assert_eq!(app.wb.sheet(0).value(2, 0), Value::Number(7.0));
        let history = app.undo.depth();

        tx.send(parsed_cell()).unwrap();
        app.poll_background_paste();
        assert_eq!(app.wb.sheet(0).value(1, 0), Value::Empty);
        assert_eq!(app.undo.depth(), history);
        assert!(app.status.contains("已取消后台粘贴"));
    }

    #[test]
    fn background_paste_cancels_after_row_insertion_but_keeps_an_unchanged_anchor() {
        let _env = TestEnv::new("bg-paste-anchor");
        let mut app = SheetApp::new_with_context(&eframe::egui::Context::default(), None);
        app.sel = (1, 0);
        let stale = app.begin_background_paste(false);
        app.structural(sheetcore::structural::Structural::InsertRows { at: 0, n: 1 });
        let history = app.undo.depth();
        stale.send(parsed_cell()).unwrap();
        app.poll_background_paste();
        assert_eq!(app.wb.sheet(0).value(1, 0), Value::Empty);
        assert_eq!(app.undo.depth(), history);
        assert!(app.status.contains("已取消后台粘贴"));

        app.sel = (2, 0);
        let current = app.begin_background_paste(true);
        app.sel = (4, 0);
        current.send(parsed_cell()).unwrap();
        app.poll_background_paste();
        assert_eq!(app.wb.sheet(0).value(2, 0), Value::Number(42.0));
        assert_eq!(app.wb.sheet(0).value(4, 0), Value::Empty);
        assert_eq!(app.undo.depth().0, history.0 + 1);
    }

    #[test]
    fn phase5_bg_gate_background_paste_rejects_cross_sheet_coord_epoch_and_keeps_ordinary_edit() {
        use sheetcore::structural::{Structural, apply_structural_wb};

        let _env = TestEnv::new("phase5-bg-gate-paste");
        let mut app = SheetApp::new_with_context(&eframe::egui::Context::default(), None);
        app.wb.sheets.push(sheetcore::Sheet::new("Other"));
        app.sel = (1, 0);

        // Ordinary non-coordinate cell edit on another sheet must NOT cancel background paste.
        let ok_tx = app.begin_background_paste(false);
        app.wb.sheet_mut(1).set(0, 0, Cell { value: Value::Number(9.0), formula: None });
        app.doc_gen += 1;
        let history = app.undo.depth();
        ok_tx.send(parsed_cell()).unwrap();
        app.poll_background_paste();
        assert_eq!(app.wb.sheet(0).value(1, 0), Value::Number(42.0));
        assert_eq!(app.undo.depth().0, history.0 + 1);

        // Structural edit on ANOTHER sheet advances wb.coord_epoch() while leaving sheet 0's
        // coordinate_revision() unchanged: CoordGate must still cancel the stale paste cleanly.
        app.sel = (3, 0);
        let stale_tx = app.begin_background_paste(false);
        let rev_before = app.wb.sheet(0).coordinate_revision();
        apply_structural_wb(&mut app.wb, 1, &Structural::InsertRows { at: 0, n: 1 });
        assert_eq!(app.wb.sheet(0).coordinate_revision(), rev_before);
        let history_before = app.undo.depth();
        stale_tx.send(parsed_cell()).unwrap();
        app.poll_background_paste();
        assert_eq!(app.wb.sheet(0).value(3, 0), Value::Empty);
        assert_eq!(app.undo.depth(), history_before);
        assert!(app.status.contains("已取消后台粘贴"), "{}", app.status);
    }
}
