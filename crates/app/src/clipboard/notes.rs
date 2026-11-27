//! Capture, transfer and undo of notes and threaded comments during paste.

use super::Clip;
use crate::SheetApp;

#[derive(Clone, Copy)]
pub(super) enum NoteRows<'a> {
    Explicit(&'a [u32]),
    Sparse(&'a super::sparse::PasteRows),
}

impl NoteRows<'_> {
    fn contains(self, row: u32) -> bool {
        match self {
            Self::Explicit(rows) => rows.binary_search(&row).is_ok(),
            Self::Sparse(rows) => rows.contains(row),
        }
    }
    fn at(self, offset: u32) -> u32 {
        match self {
            Self::Explicit(rows) => rows[offset as usize],
            Self::Sparse(rows) => rows.at(offset),
        }
    }
}

impl SheetApp {
    pub(super) fn capture_clip_notes_mapped(
        &mut self,
        si: usize,
        rect: (u32, u32, u32, u32),
        source_rows: Option<&[u32]>,
        source_cols: Option<&[u32]>,
    ) {
        self.clip_notes.clear();
        self.clip_threads.clear();
        self.clip_notes_captured = true;
        let sh = self.wb.sheet(si);
        if sh.notes.is_empty() && sh.threads.is_empty() {
            return;
        }
        let (r1, c1, r2, c2) = rect;
        let row_map: Option<std::collections::HashMap<u32, u32>> = source_rows.map(|rows| {
            rows.iter()
                .enumerate()
                .map(|(idx, &r)| (r, idx as u32))
                .collect()
        });
        let col_map: Option<std::collections::HashMap<u32, u32>> = source_cols.map(|cols| {
            cols.iter()
                .enumerate()
                .map(|(idx, &c)| (c, idx as u32))
                .collect()
        });
        let map_coord = |r: u32, c: u32| -> Option<(u32, u32)> {
            let ri = match &row_map {
                Some(m) => *m.get(&r)?,
                None => {
                    if r < r1 || r > r2 {
                        return None;
                    }
                    r - r1
                }
            };
            let ci = match &col_map {
                Some(m) => *m.get(&c)?,
                None => {
                    if c < c1 || c > c2 {
                        return None;
                    }
                    c - c1
                }
            };
            Some((ri, ci))
        };
        for (&k, n) in &sh.notes {
            if let Some((ri, ci)) = map_coord(k.row, k.col) {
                self.clip_notes.push((ri, ci, n.clone()));
            }
        }
        for (&k, tc) in &sh.threads {
            if let Some((ri, ci)) = map_coord(k.row, k.col) {
                self.clip_threads.push((ri, ci, tc.clone()));
            }
        }
        self.clip_notes.sort_unstable_by_key(|(r, c, _)| (*r, *c));
        self.clip_threads.sort_unstable_by_key(|(r, c, _)| (*r, *c));
    }


    pub(super) fn resolve_clip_notes_for_paste(
        &self,
        clip: &Clip,
    ) -> (
        Vec<(u32, u32, sheetcore::notes::Note)>,
        Vec<(u32, u32, sheetcore::notes::ThreadedComment)>,
    ) {
        if self.clip_notes_captured {
            return (self.clip_notes.clone(), self.clip_threads.clone());
        }
        let Some(sh) = self.wb.sheets.get(clip.sheet) else {
            return (Vec::new(), Vec::new());
        };
        if sh.notes.is_empty() && sh.threads.is_empty() {
            return (Vec::new(), Vec::new());
        }
        let (r1, c1, r2, c2) = clip.rect;
        let row_map: Option<std::collections::HashMap<u32, u32>> = clip.source_rows.as_ref().map(|rows| {
            rows.iter()
                .enumerate()
                .map(|(idx, &r)| (r, idx as u32))
                .collect()
        });
        let col_map: Option<std::collections::HashMap<u32, u32>> = clip.source_cols.as_ref().map(|cols| {
            cols.iter()
                .enumerate()
                .map(|(idx, &c)| (c, idx as u32))
                .collect()
        });
        let map_coord = |r: u32, c: u32| -> Option<(u32, u32)> {
            let ri = match &row_map {
                Some(m) => *m.get(&r)?,
                None => {
                    if r < r1 || r > r2 {
                        return None;
                    }
                    r - r1
                }
            };
            let ci = match &col_map {
                Some(m) => *m.get(&c)?,
                None => {
                    if c < c1 || c > c2 {
                        return None;
                    }
                    c - c1
                }
            };
            Some((ri, ci))
        };
        let mut notes = Vec::new();
        let mut threads = Vec::new();
        for (&k, n) in &sh.notes {
            if let Some((ri, ci)) = map_coord(k.row, k.col) {
                notes.push((ri, ci, n.clone()));
            }
        }
        for (&k, tc) in &sh.threads {
            if let Some((ri, ci)) = map_coord(k.row, k.col) {
                threads.push((ri, ci, tc.clone()));
            }
        }
        (notes, threads)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn apply_pasted_notes(
        &mut self,
        target_sheet: usize,
        target_origin: (u32, u32),
        target_rows: Option<NoteRows<'_>>,
        h: u32,
        w: u32,
        base_h: u32,
        base_w: u32,
        src_notes: &[(u32, u32, sheetcore::notes::Note)],
        src_threads: &[(u32, u32, sheetcore::notes::ThreadedComment)],
        cut_src: Option<(usize, (u32, u32, u32, u32), Option<&[u32]>)>,
    ) -> sheetcore::undo::NoteUndoEdit {
        use std::collections::{BTreeMap, HashSet};
        let mut out = sheetcore::undo::NoteUndoEdit::default();
        if h == 0 || w == 0 || base_h == 0 || base_w == 0 {
            return out;
        }
        let target_has_any = {
            let sh = self.wb.sheet(target_sheet);
            !sh.notes.is_empty() || !sh.threads.is_empty()
        };
        let cut_has_any = cut_src.is_some_and(|(src_si, ..)| {
            let sh = self.wb.sheet(src_si);
            !sh.notes.is_empty() || !sh.threads.is_empty()
        });
        if src_notes.is_empty() && src_threads.is_empty() && !target_has_any && !cut_has_any {
            return out;
        }

        let (r0, c0) = target_origin;
        let r_end = r0.saturating_add(h.saturating_sub(1));
        let c_end = c0.saturating_add(w.saturating_sub(1));
        let in_target = |rr: u32, cc: u32| -> bool {
            if cc < c0 || cc > c_end {
                return false;
            }
            match target_rows {
                Some(rows) => rows.contains(rr),
                None => rr >= r0 && rr <= r_end,
            }
        };
        let cut_row_set: Option<HashSet<u32>> =
            cut_src.and_then(|(_, _, rows)| rows.map(|v| v.iter().copied().collect()));
        let in_cut = |rr: u32, cc: u32, rect: (u32, u32, u32, u32)| -> bool {
            if cc < rect.1 || cc > rect.3 {
                return false;
            }
            match &cut_row_set {
                Some(set) => set.contains(&rr),
                None => rr >= rect.0 && rr <= rect.2,
            }
        };

        let mut before_notes: BTreeMap<(usize, u32, u32), Option<sheetcore::notes::Note>> = BTreeMap::new();
        let mut before_threads: BTreeMap<(usize, u32, u32), Option<sheetcore::notes::ThreadedComment>> =
            BTreeMap::new();

        // 1) 记录源剪切区受影响的现有批注与线程
        let mut cut_note_keys: Vec<(usize, sheetcore::model::CellRef)> = Vec::new();
        let mut cut_thread_keys: Vec<(usize, sheetcore::model::CellRef)> = Vec::new();
        if let Some((src_si, src_rect, _)) = cut_src {
            let sh = self.wb.sheet(src_si);
            for (&k, n) in &sh.notes {
                if in_cut(k.row, k.col, src_rect) {
                    before_notes.insert((src_si, k.row, k.col), Some(n.clone()));
                    cut_note_keys.push((src_si, k));
                }
            }
            for (&k, tc) in &sh.threads {
                if in_cut(k.row, k.col, src_rect) {
                    before_threads.insert((src_si, k.row, k.col), Some(tc.clone()));
                    cut_thread_keys.push((src_si, k));
                }
            }
        }

        // 2) 记录目标覆盖区受影响的现有批注与线程
        let mut dst_clear_note_keys: Vec<sheetcore::model::CellRef> = Vec::new();
        let mut dst_clear_thread_keys: Vec<sheetcore::model::CellRef> = Vec::new();
        {
            let sh = self.wb.sheet(target_sheet);
            for (&k, n) in &sh.notes {
                if in_target(k.row, k.col) {
                    before_notes.entry((target_sheet, k.row, k.col)).or_insert_with(|| Some(n.clone()));
                    dst_clear_note_keys.push(k);
                }
            }
            for (&k, tc) in &sh.threads {
                if in_target(k.row, k.col) {
                    before_threads.entry((target_sheet, k.row, k.col)).or_insert_with(|| Some(tc.clone()));
                    dst_clear_thread_keys.push(k);
                }
            }
        }

        // 3) 计算写入目标位置（支持平铺粘贴与合并锚点归位），并补全 before 快照
        let tile_r = (h / base_h).max(1);
        let tile_c = (w / base_w).max(1);
        let mut writes_notes: Vec<(u32, u32, sheetcore::notes::Note)> = Vec::new();
        let mut writes_threads: Vec<(u32, u32, sheetcore::notes::ThreadedComment)> = Vec::new();
        {
            let sh = self.wb.sheet(target_sheet);
            for tr in 0..tile_r {
                for tc in 0..tile_c {
                    for (ri, ci, n) in src_notes {
                        let out_r_idx = tr * base_h + *ri;
                        let out_c_idx = tc * base_w + *ci;
                        if out_r_idx >= h || out_c_idx >= w {
                            continue;
                        }
                        let raw_r = target_rows
                            .map(|rows| rows.at(out_r_idx))
                            .unwrap_or(r0 + out_r_idx);
                        let raw_c = c0 + out_c_idx;
                        let (ar, ac) = sh
                            .merge_at(raw_r, raw_c)
                            .map(|(mr1, mc1, _, _)| (mr1, mc1))
                            .unwrap_or((raw_r, raw_c));
                        before_notes.entry((target_sheet, ar, ac)).or_insert_with(|| {
                            sh.notes.get(&sheetcore::model::CellRef::new(ar, ac)).cloned()
                        });
                        let mut nn = n.clone();
                        nn.r = ar;
                        nn.c = ac;
                        writes_notes.push((ar, ac, nn));
                    }
                    for (ri, ci, thread) in src_threads {
                        let out_r_idx = tr * base_h + *ri;
                        let out_c_idx = tc * base_w + *ci;
                        if out_r_idx >= h || out_c_idx >= w {
                            continue;
                        }
                        let raw_r = target_rows
                            .map(|rows| rows.at(out_r_idx))
                            .unwrap_or(r0 + out_r_idx);
                        let raw_c = c0 + out_c_idx;
                        let (ar, ac) = sh
                            .merge_at(raw_r, raw_c)
                            .map(|(mr1, mc1, _, _)| (mr1, mc1))
                            .unwrap_or((raw_r, raw_c));
                        before_threads.entry((target_sheet, ar, ac)).or_insert_with(|| {
                            sh.threads.get(&sheetcore::model::CellRef::new(ar, ac)).cloned()
                        });
                        let mut nt = thread.clone();
                        nt.r = ar;
                        nt.c = ac;
                        writes_threads.push((ar, ac, nt));
                    }
                }
            }
        }

        // 4) 应用清理与写入
        for (src_si, k) in cut_note_keys {
            self.wb.sheet_mut(src_si).notes.remove(&k);
        }
        for (src_si, k) in cut_thread_keys {
            self.wb.sheet_mut(src_si).threads.remove(&k);
        }
        {
            let sh = self.wb.sheet_mut(target_sheet);
            for k in dst_clear_note_keys {
                sh.notes.remove(&k);
            }
            for k in dst_clear_thread_keys {
                sh.threads.remove(&k);
            }
            for (ar, ac, n) in writes_notes {
                sh.notes.insert(sheetcore::model::CellRef::new(ar, ac), n);
            }
            for (ar, ac, tc) in writes_threads {
                sh.threads.insert(sheetcore::model::CellRef::new(ar, ac), tc);
            }
        }

        // 5) 汇总真实 diff 与标脏
        let mut dirty_note_sheets = HashSet::new();
        let mut dirty_thread_sheets = HashSet::new();
        for ((si_k, rr, cc), b_opt) in before_notes {
            let a_opt = self
                .wb
                .sheet(si_k)
                .notes
                .get(&sheetcore::model::CellRef::new(rr, cc))
                .cloned();
            if b_opt != a_opt {
                out.notes.push((si_k, (rr, cc), b_opt, a_opt));
                dirty_note_sheets.insert(si_k);
            }
        }
        for ((si_k, rr, cc), b_opt) in before_threads {
            let a_opt = self
                .wb
                .sheet(si_k)
                .threads
                .get(&sheetcore::model::CellRef::new(rr, cc))
                .cloned();
            if b_opt != a_opt {
                out.threads.push((si_k, (rr, cc), b_opt, a_opt));
                dirty_thread_sheets.insert(si_k);
            }
        }
        for s in dirty_note_sheets {
            let sh = self.wb.sheet_mut(s);
            sh.notes_dirty = true;
            sh.bump_display_style_revision();
        }
        for s in dirty_thread_sheets {
            let sh = self.wb.sheet_mut(s);
            sh.threads_dirty = true;
            sh.bump_display_style_revision();
        }
        out
    }
}
