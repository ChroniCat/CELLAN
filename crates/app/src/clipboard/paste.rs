//! Ordinary paste, including source selection, merged cells and TSV input.

use super::{Clip, LastPasteState, current_os_clipboard_seq, read_system_clip};
use crate::{SheetApp, office_clipboard};
use sheetcore::Cell;

impl SheetApp {
    /// values_only=true 时只贴值（不贴公式和格式）
    pub(crate) fn clip_paste(&mut self, values_only: bool, text: Option<String>) {
        let owns_range = self.deferred_clip_current() == Some(true);
        if let Some(clip) = self.clip.as_ref().filter(|clip| owns_range && office_clipboard::delayed::is_full_columns(clip.rect)) {
            if let Some(range) = clip.deferred.clone() {
                self.paste_full_columns(range.book(), clip.rect.1, clip.rect.3, values_only);
                return;
            }
        }
        if self.edit_blocked() {
            return;
        }
        let seq_now = current_os_clipboard_seq();
        let seq_changed = self.clipboard_seq_changed_now(seq_now);
        let seq_unchanged = self.clip.is_some()
            && self.clip_stamp.is_some()
            && self.clip_seq.is_some_and(|saved| seq_now == Some(saved));
        // Fast path for external rich clipboard (Excel/WPS XMLSS or HTML): when clipboard sequence
        // changed since our copy (or no internal clip / deferred owner lost), read rich Office table
        // BEFORE decoding megabytes of unused CF_UNICODETEXT TSV!
        let definitely_external = seq_changed
            || self
                .clip
                .as_ref()
                .is_none_or(|c| c.deferred.is_some() && !owns_range);
        if definitely_external
            && (!values_only || office_clipboard::has_xml_spreadsheet())
            && text.is_none()
            && std::env::var_os("SC_CLIP_LOG").is_none()
            && office_clipboard::has_rich_table_format()
        {
            if !cfg!(test) && office_clipboard::has_xml_spreadsheet() {
                if let Some(raw) = office_clipboard::read_raw_xmlss() {
                    if raw.len() > 1_000_000 {
                        let mb = raw.len() as f64 / (1024.0 * 1024.0);
                        let tx = self.begin_background_paste(values_only);
                        self.status = format!("正在后台解析剪贴板数据（{mb:.1} MB）…");
                        std::thread::spawn(move || {
                            let res = (|| {
                                let text = match std::str::from_utf8(&raw) {
                                    Ok(s) => std::borrow::Cow::Borrowed(s),
                                    Err(_) => String::from_utf8_lossy(&raw),
                                };
                                let xml = text.trim_end_matches('\0');
                                sheetcore::clipboard_xmlss::parse_sparse_ext(xml, values_only)
                                    .filter(|s| s.rows > 0 && s.cols > 0)
                                    .ok_or_else(|| "无法解析剪贴板中的表格数据".to_string())
                            })();
                            let _ = tx.send(res);
                        });
                        return;
                    }
                }
            }
            match office_clipboard::read_rich_table_ext(values_only) {
                Some(office_clipboard::RichClipboard::Sparse(sparse)) => {
                    if self.paste_sparse_xmlss_ext(&sparse, values_only) {
                        return;
                    }
                }
                Some(office_clipboard::RichClipboard::Block(block)) if !values_only => {
                    if self.paste_office_block(&block) {
                        return;
                    }
                }
                Some(office_clipboard::RichClipboard::HtmlTruncated(limit)) if !values_only => {
                    self.report_html_clipboard_limit(limit);
                    return;
                }
                _ => {}
            }
        }
        // 所有入口共用同一份“最新系统剪贴板”：Ribbon/右键没传文本时主动读（若 Win32 序列号未变则直接走内部块，免锁免拷贝）。
        let text = text.filter(|text| !text.is_empty()).or_else(|| {
            if owns_range || seq_unchanged {
                return None;
            }
            let sys = read_system_clip();
            (!sys.text.is_empty()).then_some(sys.text)
        });
        let (r, c) = self.sel;
        let si = self.sheet_idx;
        // 三个入口（Ribbon/右键/Ctrl+V）共用这一份判断：系统剪贴板比内部 clip 新时，
        // 富格式走 Office XMLSS/HTML，纯文本才退回 TSV。
        let use_internal = self.internal_clip_for(text.as_deref(), false).is_some();
        if !use_internal && (!values_only || office_clipboard::has_xml_spreadsheet()) {
            match office_clipboard::read_rich_table_ext(values_only) {
                Some(office_clipboard::RichClipboard::Sparse(sparse)) => {
                    if self.paste_sparse_xmlss_ext(&sparse, values_only) {
                        return;
                    }
                }
                Some(office_clipboard::RichClipboard::Block(block)) if !values_only => {
                    if std::env::var("SC_CLIP_LOG").is_ok() {
                        eprintln!("[clip] text={:?} block={:?}", text, Self::office_block_tsv(&block));
                    }
                    if self.paste_office_block(&block) {
                        return;
                    }
                }
                Some(office_clipboard::RichClipboard::HtmlTruncated(limit)) if !values_only => {
                    self.report_html_clipboard_limit(limit);
                    return;
                }
                _ => {}
            }
        }
        if use_internal {
            let use_sparse_internal = self.clip.as_ref().is_some_and(|clip| {
                clip.deferred.as_ref().is_some_and(|range| {
                    let (h, w) = range.dimensions();
                    u64::from(h) * u64::from(w) > 4_096
                })
            });
            if use_sparse_internal {
                self.paste_deferred_sparse(values_only);
                return;
            }
        }
        let mut tsv_paste = false;
        let mut rows: Vec<Vec<(sheetcore::model::Cell, u16)>> = Vec::new();
        let mut pasted_merges: Vec<(u32, u32, u32, u32)> = Vec::new();
        let mut pasted_clip_notes: Vec<(u32, u32, sheetcore::notes::Note)> = Vec::new();
        let mut pasted_clip_threads: Vec<(u32, u32, sheetcore::notes::ThreadedComment)> = Vec::new();
        let mut cut_src: Option<(usize, Vec<(u32, u32)>)> = None;
        let mut cut_note_src: Option<(usize, (u32, u32, u32, u32), Option<Vec<u32>>)> = None;
        let mut cut_move = None;

        // Borrow the previous block only when it will actually be pasted. External plain
        // text must not copy or reformat the last internal selection.
        if let Some(clip) = self.clip.as_ref().filter(|_| use_internal) {
            if clip.cut && !self.cut_source_matches(clip) {
                self.status = "剪切源已更改，请重新剪切".into();
                return;
            }
            if !values_only {
                let (cn, ct) = self.resolve_clip_notes_for_paste(clip);
                pasted_clip_notes = cn;
                pasted_clip_threads = ct;
            }
            let (sr1, sc1, sr2, sc2) = clip.rect;
            let dr = r as i64 - sr1 as i64;
            let dc = c as i64 - sc1 as i64;
            cut_move = clip.cut.then(|| super::cut::CutRange::new(clip, &self.wb, si, (r,c)));
            let target_sheet_name = self.wb.sheet(si).name.clone();
            for (index, row) in clip.cells().iter().enumerate() {
                let row_shift = r as i64 + index as i64 - clip.source_row(index) as i64;
                let mut out = Vec::new();
                for (col_idx, (cell, st)) in row.iter().enumerate() {
                    let col_shift = c as i64 + col_idx as i64 - clip.source_col(col_idx) as i64;
                    let mut cell = cell.clone();
                    if !values_only {
                        if let Some(f) = &cell.formula {
                            if let Some(movement) = &cut_move {
                                cell.formula = Some(movement.moved_formula(f).into());
                            } else {
                                let nf = sheetcore::refs::translate(f, row_shift, col_shift, &target_sheet_name);
                                cell.formula = Some(nf.into());
                            }
                        }
                    } else {
                        cell.formula = None;
                    }
                    out.push((cell, if values_only { 0 } else { *st }));
                }
                rows.push(out);
            }
            if !clip.merges.is_empty() {
                pasted_merges = clip
                    .merges
                    .iter()
                    .map(|(a, b, c2, d)| {
                        (
                            (*a as i64 + dr).max(0) as u32,
                            (*b as i64 + dc).max(0) as u32,
                            (*c2 as i64 + dr).max(0) as u32,
                            (*d as i64 + dc).max(0) as u32,
                        )
                    })
                    .collect();
            }
            if clip.cut {
                let keys = (0..clip.cells().len()).flat_map(|index| {
                    let rr = clip.source_row(index);
                    (sc1..=sc2).map(move |cc| (rr, cc))
                }).collect();
                cut_src = Some((clip.sheet, keys));
                cut_note_src = Some((clip.sheet, clip.rect, clip.source_rows.clone()));
            }
        } else if let Some(t) = text {
            rows = Self::parse_tsv(&t);
            tsv_paste = true;
        } else {
            self.status = if office_clipboard::has_xml_spreadsheet() {
                "无法导入剪贴板中的表格范围，请在源程序中选中具体数据区域后再复制"
            } else {
                "剪贴板是空的"
            }.into();
            return;
        }
        if rows.is_empty() {
            return;
        }

        let clip_base_h = rows.len() as u32;
        let clip_base_w = rows.iter().map(|row| row.len()).max().unwrap_or(0) as u32;
        // 选区平铺粘贴（Excel 语义：当目标选区宽高均为源块宽高的整数倍且非剪切时，自动平铺源块并按格偏移公式；
        // 若选区内存在筛选隐藏行，仅按选区内的可见行计数并严格限定在选区范围内，绝不溢出覆写选区外的未选中行）
        let (r, c, tiled_dst_rows) = {
            let base_h = rows.len() as u32;
            let base_w = rows.iter().map(|row| row.len()).max().unwrap_or(0) as u32;
            let (sel_r1, sel_c1, sel_r2, sel_c2) = self.sel_rect();
            let sel_h = sel_r2 - sel_r1 + 1;
            let sel_w = sel_c2 - sel_c1 + 1;
            let sh_ref = self.wb.sheet(si);
            let sel_has_filter_hidden = !sh_ref.hidden_rows.is_empty()
                && sel_h <= sheetcore::undo::MAX_DENSE_RECT_KEYS as u32
                && (sel_r1..=sel_r2).any(|rr| sh_ref.hidden_rows.contains(&rr));
            let vis_sel_rows: Option<Vec<u32>> = if sel_has_filter_hidden {
                Some(
                    (sel_r1..=sel_r2)
                        .filter(|rr| {
                            !sh_ref.hidden_rows.contains(rr) && !sh_ref.hidden_rows_user.contains(rr)
                        })
                        .collect(),
                )
            } else {
                None
            };
            let eff_h = vis_sel_rows.as_ref().map_or(sel_h, |v| v.len() as u32);
            if !tsv_paste
                && cut_src.is_none()
                && base_h > 0
                && base_w > 0
                && eff_h > 0
                && (eff_h > base_h || sel_w > base_w)
                && eff_h % base_h == 0
                && sel_w % base_w == 0
                && u64::from(eff_h) * u64::from(sel_w) <= sheetcore::undo::MAX_DENSE_RECT_KEYS as u64
            {
                let tile_r = eff_h / base_h;
                let tile_c = sel_w / base_w;
                let target_sheet_name = self.wb.sheet(si).name.clone();
                let anchor_dr = sel_r1 as i64 - r as i64;
                let anchor_dc = sel_c1 as i64 - c as i64;
                let base_rows = std::mem::take(&mut rows);
                rows.reserve(eff_h as usize);
                for tr in 0..tile_r {
                    for (bi, base_row) in base_rows.iter().enumerate() {
                        let out_row_idx = (tr * base_h) as usize + bi;
                        let shift_r = if let Some(vrows) = vis_sel_rows.as_ref() {
                            vrows[out_row_idx] as i64 - (r as i64 + bi as i64)
                        } else {
                            anchor_dr + i64::from(tr * base_h)
                        };
                        let mut out_row = Vec::with_capacity(sel_w as usize);
                        for tc in 0..tile_c {
                            let shift_c = anchor_dc + i64::from(tc * base_w);
                            for col_idx in 0..(base_w as usize) {
                                let (mut cell, st) = base_row
                                    .get(col_idx)
                                    .cloned()
                                    .unwrap_or_else(|| (sheetcore::model::Cell::default(), 0));
                                if !values_only && (shift_r != 0 || shift_c != 0) {
                                    if let Some(f) = &cell.formula {
                                        let nf = sheetcore::refs::translate(f, shift_r, shift_c, &target_sheet_name);
                                        cell.formula = Some(nf.into());
                                    }
                                }
                                out_row.push((cell, st));
                            }
                        }
                        rows.push(out_row);
                    }
                }
                if !pasted_merges.is_empty() && vis_sel_rows.is_none() {
                    let base_merges = std::mem::take(&mut pasted_merges);
                    for tr in 0..tile_r {
                        for tc in 0..tile_c {
                            let dr_m = anchor_dr + i64::from(tr * base_h);
                            let dc_m = anchor_dc + i64::from(tc * base_w);
                            for &(a, b, c2, d) in &base_merges {
                                pasted_merges.push((
                                    (a as i64 + dr_m).max(0) as u32,
                                    (b as i64 + dc_m).max(0) as u32,
                                    (c2 as i64 + dr_m).max(0) as u32,
                                    (d as i64 + dc_m).max(0) as u32,
                                ));
                            }
                        }
                    }
                }
                let start_r = vis_sel_rows
                    .as_ref()
                    .and_then(|v| v.first().copied())
                    .unwrap_or(sel_r1);
                (start_r, sel_c1, vis_sel_rows)
            } else if let Some(vrows) = vis_sel_rows
                && cut_src.is_none()
                && !vrows.is_empty()
                && vrows.len() == base_h as usize
                && sel_w == base_w
            {
                let target_sheet_name = self.wb.sheet(si).name.clone();
                let extra_dc = sel_c1 as i64 - c as i64;
                if use_internal && !values_only {
                    for (i, row) in rows.iter_mut().enumerate() {
                        let extra_dr = vrows[i] as i64 - (r + i as u32) as i64;
                        if extra_dr != 0 || extra_dc != 0 {
                            for (cell, _) in row.iter_mut() {
                                if let Some(f) = &cell.formula {
                                    let nf = sheetcore::refs::translate(f, extra_dr, extra_dc, &target_sheet_name);
                                    cell.formula = Some(nf.into());
                                }
                            }
                        }
                    }
                }
                (vrows[0], sel_c1, Some(vrows))
            } else if cut_src.is_none()
                && (sel_r1 != r || sel_c1 != c)
                && sel_h == base_h
                && sel_w == base_w
            {
                let target_sheet_name = self.wb.sheet(si).name.clone();
                let extra_dr = sel_r1 as i64 - r as i64;
                let extra_dc = sel_c1 as i64 - c as i64;
                if use_internal && !values_only {
                    for row in rows.iter_mut() {
                        for (cell, _) in row.iter_mut() {
                            if let Some(f) = &cell.formula {
                                let nf = sheetcore::refs::translate(f, extra_dr, extra_dc, &target_sheet_name);
                                cell.formula = Some(nf.into());
                            }
                        }
                    }
                }
                if !pasted_merges.is_empty() {
                    for (a, b, c2, d) in &mut pasted_merges {
                        *a = (*a as i64 + extra_dr).max(0) as u32;
                        *b = (*b as i64 + extra_dc).max(0) as u32;
                        *c2 = (*c2 as i64 + extra_dr).max(0) as u32;
                        *d = (*d as i64 + extra_dc).max(0) as u32;
                    }
                }
                (sel_r1, sel_c1, None)
            } else {
                (r, c, None)
            }
        };

        // One transaction covers both sides of a cut, including overlapping cells.
        let h = rows.len() as u32;
        let w = rows.iter().map(|r| r.len()).max().unwrap_or(0) as u32;
        let area = (h as u64) * (w as u64);
        if (self.net.is_some() || cut_src.is_some())
            && area > sheetcore::undo::MAX_DENSE_RECT_KEYS as u64
        {
            self.status = "粘贴内容过大（单次最多支持 50 万个单元格）".into();
            return;
        }
        if h > 0 && w > 0 && (r.saturating_add(h - 1) > 1_048_575 || c.saturating_add(w - 1) > 16_383) {
            self.status = "粘贴区域超出工作表行列边界".into();
            return;
        }
        // 筛选态下粘贴多行：若目标行跨过被筛选隐藏的行，则只落到可见行上，绝不暗中覆写隐藏行
        let dst_rows: Option<Vec<u32>> = if let Some(pre) = tiled_dst_rows {
            Some(pre)
        } else if h > 1
            && cut_src.is_none()
            && !self.wb.sheet(si).hidden_rows.is_empty()
            && (r..r.saturating_add(h)).any(|rr| self.wb.sheet(si).hidden_rows.contains(&rr))
        {
            let sh = self.wb.sheet(si);
            let mut out = Vec::with_capacity(h as usize);
            let mut cur = r;
            while out.len() < h as usize {
                if cur > 1_048_575 {
                    self.status = "粘贴区域超出工作表行列边界".into();
                    return;
                }
                if !sh.hidden_rows.contains(&cur) && !sh.hidden_rows_user.contains(&cur) {
                    out.push(cur);
                }
                cur = cur.saturating_add(1);
            }
            // 若是内部复制带公式，同步按每个目标可见行的实际行偏移平移引用
            if use_internal && !values_only && let Some(clip) = self.clip.as_ref() {
                let target_sheet_name = self.wb.sheet(si).name.clone();
                let dc = c as i64 - clip.rect.1 as i64;
                for (i, row) in rows.iter_mut().enumerate() {
                    let actual_r = out[i];
                    let old_r = r + i as u32;
                    let extra_dr = actual_r as i64 - old_r as i64;
                    if extra_dr != 0 {
                        for (cell, _) in row.iter_mut() {
                            if let Some(f) = &cell.formula {
                                let nf = sheetcore::refs::translate(f, extra_dr, 0, &target_sheet_name);
                                cell.formula = Some(nf.into());
                            }
                        }
                    }
                    let _ = dc;
                }
            }
            Some(out)
        } else {
            None
        };
        let target_last_row = dst_rows
            .as_ref()
            .and_then(|v| v.last().copied())
            .unwrap_or_else(|| r.saturating_add(h.saturating_sub(1)));
        // Fast CoW batch path for large non-collab pastes (no 500k cap, no dense `Vec<CellDelta>`).
        if self.net.is_none()
            && cut_src.is_none()
            && dst_rows.is_none()
            && area > sheetcore::undo::COW_BATCH_THRESHOLD as u64
            && h > 0
            && w > 0
        {
            let target_rect = (r, c, r + h - 1, c + w - 1);
            if !self.check_sparse_rect_preflight(si, target_rect, &[], "粘贴") {
                return;
            }
            let merge_edit = if values_only || tsv_paste {
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
            let label = if values_only { "粘贴数值" } else { "粘贴" };
            let mut sample = Vec::with_capacity(sheetcore::revision::MAX_CELLS);
            let mut changed = 0usize;
            {
                let sh = self.wb.sheet_mut(si);
                for (i, row) in rows.iter().enumerate() {
                    let rr = r + i as u32;
                    for (j, (cell, st)) in row.iter().enumerate() {
                        let cc = c + j as u32;
                        let need_sample = sample.len() < sheetcore::revision::MAX_CELLS;
                        let (prev_cell, prev_st) = if need_sample {
                            (sh.get(rr, cc).map(|c| c.into_owned()).unwrap_or_default(), sh.style_of(rr, cc))
                        } else {
                            (Cell::default(), 0)
                        };
                        let cell_changed = sh.get(rr, cc).as_deref().map_or(!cell.is_blank(), |old| old != cell);
                        sh.set(rr, cc, cell.clone());
                        let new_st = if !values_only && !tsv_paste {
                            sh.set_style(rr, cc, *st);
                            *st
                        } else if need_sample {
                            prev_st
                        } else {
                            0
                        };
                        if cell_changed || (need_sample && prev_st != new_st) {
                            changed += 1;
                            if need_sample {
                                sample.push(sheetcore::undo::CellDelta {
                                    sheet: si,
                                    row: rr,
                                    col: cc,
                                    before: prev_cell,
                                    after: cell.clone(),
                                    before_style: prev_st,
                                    after_style: new_st,
                                });
                            }
                        }
                    }
                }
            }
            if !values_only && !tsv_paste {
                let _ = self.apply_pasted_notes(
                    si,
                    (r, c),
                    None,
                    h,
                    w,
                    clip_base_h,
                    clip_base_w,
                    &pasted_clip_notes,
                    &pasted_clip_threads,
                    None,
                );
            }
            if !sample.is_empty() {
                self.record_revision_sample(label, &sample, changed);
            }
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
        let mut touched = Vec::with_capacity(rows.iter().map(Vec::len).sum());
        for (i, row) in rows.iter().enumerate() {
            let rr = dst_rows.as_ref().map_or(r + i as u32, |v| v[i]);
            touched.extend((0..row.len()).map(|j| (si, rr, c + j as u32)));
        }
        if let Some((source, keys)) = &cut_src {
            touched.extend(keys.iter().map(|&(rr, cc)| (*source, rr, cc)));
            touched.sort_unstable();
            touched.dedup();
        }
        let cut_rewrites = cut_move.as_ref().map_or_else(Vec::new, |movement| {
            movement.external_formulas(&self.wb, si, (target_last_row, c + w - 1))
        });
        let mut move_refs = cut_move.as_ref().map_or_else(Default::default, |m| m.workbook_references(&self.wb));
        if self.net.is_some() && !move_refs.is_empty() {
            self.status = "当前协作协议不能同步名称或图表引用，请断开协作后再剪切粘贴".into();
            return;
        }
        if !cut_rewrites.is_empty() {
            touched.extend(cut_rewrites.iter().map(|&(s, rr, cc, _)| (s, rr, cc)));
            touched.sort_unstable();
            touched.dedup();
        }
        // 目标区或剪切源里任何一格被别人锁着 → 整批都不写
        let target_keys: Vec<(u32, u32)> = touched
            .iter()
            .filter(|(sheet, _, _)| *sheet == si)
            .map(|(_, rr, cc)| (*rr, *cc))
            .collect();
        if !values_only && !tsv_paste && self.net.as_ref().is_some_and(|net| !net.supports_thread_changes()) {
            let target_rows = dst_rows.as_deref();
            let contains = |sheet: usize, rect: (u32, u32, u32, u32), rows: Option<&[u32]>| {
                self.wb.sheet(sheet).threads.keys().any(|k|
                    (rect.0..=rect.2).contains(&k.row) && (rect.1..=rect.3).contains(&k.col)
                        && rows.is_none_or(|rows| rows.binary_search(&k.row).is_ok()))
            };
            let affects_threads = !pasted_clip_threads.is_empty()
                || contains(si, (r, c, target_last_row, c.saturating_add(w.saturating_sub(1))), target_rows)
                || cut_note_src.as_ref().is_some_and(|(sheet, rect, rows)| contains(*sheet, *rect, rows.as_deref()));
            if !self.thread_change_allowed(affects_threads) { return; }
        }
        if !self.write_preflight(&target_keys, "粘贴") {
            return;
        }
        if let Some((source, keys)) = &cut_src {
            if !self.write_preflight_on(*source, keys, "剪切") {
                return;
            }
        }
        let mut transactions = std::collections::BTreeMap::new();
        for &(sheet, rr, cc) in &touched {
            transactions.entry(sheet).or_insert_with(Vec::new).push((rr, cc));
        }
        for (&sheet, keys) in &transactions {
            if sheet != si && !self.write_preflight_on(sheet, keys, "剪切") { return; }
        }
        let merge_edit = if values_only || tsv_paste || dst_rows.is_some() {
            None
        } else {
            let source = self.clip.as_ref().filter(|_| cut_src.is_some());
            match self.paste_merge_edit((r, c, target_last_row, c + w - 1), &pasted_merges, source) {
                Ok(edit) => edit,
                Err(message) => { self.status = message.into(); return; }
            }
        };
        let merge_changes = if let Some(edit) = &merge_edit {
            let Some(changes) = edit.apply(&mut self.wb.sheets, true) else {
                self.status = "粘贴区域中的合并单元格相互冲突".into();
                return;
            };
            changes
        } else { Vec::new() };
        let before: Vec<_> = transactions.into_iter().map(|(sheet, keys)| {
            (sheet, sheetcore::undo::capture(self.wb.sheet(sheet), sheet, &keys))
        }).collect();
        let paste_before = if cut_src.is_some() {
            Vec::new()
        } else {
            before.iter().flat_map(|(_, captured)| captured.iter()).map(|d| {
                (d.row, d.col, (!d.before.is_blank()).then(|| d.before.clone()), d.before_style)
            }).collect()
        };
        // Clear before writing: the clipboard already holds a source snapshot.
        if let Some((source, keys)) = &cut_src {
            for &(rr, cc) in keys {
                self.wb.sheet_mut(*source).set(rr, cc, Cell::default());
                self.wb.sheet_mut(*source).set_style(rr, cc, 0);
            }
        }
        for (i, row) in rows.iter().enumerate() {
            let rr = dst_rows.as_ref().map_or(r + i as u32, |v| v[i]);
            for (j, (cell, st)) in row.iter().enumerate() {
                let cc = c + j as u32;
                self.wb.sheet_mut(si).set(rr, cc, cell.clone());
                if !values_only && !tsv_paste {
                    self.wb.sheet_mut(si).set_style(rr, cc, *st);
                }
            }
        }
        if !cut_rewrites.is_empty() {
            for (s, rr, cc, nf) in &cut_rewrites {
                if let Some(mut cell) = self.wb.sheet(*s).get(*rr, *cc).map(|c| c.into_owned()) {
                    cell.formula = Some(nf.as_str().into());
                    self.wb.sheet_mut(*s).set(*rr, *cc, cell);
                }
            }
        }
        let names_changed = move_refs.replay(&mut self.wb, true);
        let paste_note_edit = if !values_only && !tsv_paste {
            self.apply_pasted_notes(
                si,
                (r, c),
                dst_rows.as_deref().map(super::notes::NoteRows::Explicit),
                h,
                w,
                clip_base_h,
                clip_base_w,
                &pasted_clip_notes,
                &pasted_clip_threads,
                cut_note_src
                    .as_ref()
                    .map(|(src_si, rect, rows)| (*src_si, *rect, rows.as_deref())),
            )
        } else {
            sheetcore::undo::NoteUndoEdit::default()
        };
        let label = if cut_src.is_some() { "剪切粘贴" } else if values_only { "粘贴数值" } else { "粘贴" };
        let mut deltas = Vec::new();
        for (sheet, captured) in before {
            let changes = sheetcore::undo::finish(captured, self.wb.sheet(sheet));
            if !changes.is_empty() {
                self.record_revision(label, &changes);
                deltas.extend(changes);
            }
        }
        if self.net.is_some() || self.room_lost {
            let styles: Vec<_> = deltas.iter().filter(|d| d.before_style != d.after_style)
                .map(|d| (d.sheet, d.row, d.col)).collect();
            self.broadcast_cell_styles(&styles);
            for (sheet, rect, on) in merge_changes { self.queue_or_broadcast_merge(sheet, rect, on); }
        }
        if !paste_note_edit.is_empty() {
            self.broadcast_note_undo_edit(&paste_note_edit);
        }
        self.undo.push_with_move_references(
            sheetcore::undo::Action {
                label: label.into(), deltas, focus: (si, r, c),
                merges: merge_edit,
                snap_before: None, snap_after: None, by_site: 0,
                pictures: None,
                shapes: None,
                refresh_sizes: None,
            },
            (!paste_note_edit.is_empty()).then_some(paste_note_edit), move_refs,
        );
        if cut_src.is_some() {
            self.clear_internal_clip();
        }
        // 重算 + 广播
        if names_changed {
            self.recalc_all_and_deps_adaptive();
            self.broadcast_cells(&touched);
        } else {
            self.recalc_and_broadcast(&touched);
        }
        if cut_src.is_none() && h > 0 && w > 0 {
            self.last_paste = Some(LastPasteState {
                sheet_idx: si,
                rect: (r, c, r + h - 1, c + w - 1),
                cells: rows,
                mode: if values_only { 1 } else { 0 },
                before: paste_before,
                popup_open: false,
            });
        } else {
            self.last_paste = None;
        }
        self.wb.dirty = true;
        self.doc_gen = self.doc_gen.wrapping_add(1);
        self.tasks.last_edit = Some(std::time::Instant::now());
        self.status = format!("已粘贴 {h} 行 × {w} 列");
    }

    pub(super) fn paste_merge_edit(
        &self,
        target: (u32, u32, u32, u32),
        incoming: &[(u32, u32, u32, u32)],
        cut: Option<&Clip>,
    ) -> Result<Option<sheetcore::undo::MergeEdit>, &'static str> {
        let overlaps = |a: (u32,u32,u32,u32), b: (u32,u32,u32,u32)| a.0 <= b.2 && b.0 <= a.2 && a.1 <= b.3 && b.1 <= a.3;
        let contains = |a: (u32,u32,u32,u32), b: (u32,u32,u32,u32)| a.0 <= b.0 && a.1 <= b.1 && a.2 >= b.2 && a.3 >= b.3;
        let mut affected = vec![self.sheet_idx];
        if let Some(clip) = cut.filter(|c| c.sheet != self.sheet_idx) {
            affected.push(clip.sheet);
        }
        let mut edits = Vec::new();
        for sheet in affected {
            let mut removed = Vec::new();
            for &rect in &self.wb.sheet(sheet).merges {
                if let Some(clip) = cut.filter(|c| c.sheet == sheet && overlaps(c.rect, rect)) {
                    if !contains(clip.rect, rect) || clip.source_rows.is_some() {
                        return Err("剪切区域包含部分合并单元格，请选择完整区域");
                    }
                    removed.push(rect);
                } else if sheet == self.sheet_idx && overlaps(target, rect) {
                    if !contains(target, rect) {
                        return Err("粘贴区域包含部分合并单元格，请选择完整区域");
                    }
                    removed.push(rect);
                }
            }
            let added = if sheet == self.sheet_idx { incoming } else { &[] };
            if let Some(edit) = sheetcore::undo::MergeEdit::between(sheet, &removed, added) {
                edits.push(edit);
            }
        }
        Ok(sheetcore::undo::MergeEdit::join(edits))
    }


    fn parse_tsv(text: &str) -> Vec<Vec<(sheetcore::model::Cell, u16)>> {
        fn cell_of(field: &str, quoted: bool) -> (sheetcore::model::Cell, u16) {
            let leading_zero = field.len() > 1
                && field.starts_with('0')
                && field[1..].chars().all(|c| c.is_ascii_digit());
            let cell = if !quoted && field.starts_with('=') {
                sheetcore::model::Cell {
                    value: sheetcore::model::Value::Text(field.into()),
                    formula: Some(field.into()),
                }
            } else if quoted || leading_zero {
                sheetcore::model::Cell {
                    value: sheetcore::model::Value::Text(field.into()),
                    formula: None,
                }
            } else {
                sheetcore::model::Cell {
                    value: sheetcore::model::Value::parse_input(field),
                    formula: None,
                }
            };
            (cell, 0u16)
        }

        let mut rows = Vec::new();
        let mut row: Vec<(sheetcore::model::Cell, u16)> = Vec::new();
        let mut field = String::new();
        let mut quoted = false;
        let mut in_quotes = false;
        let mut chars = text.chars().peekable();
        while let Some(ch) = chars.next() {
            if in_quotes {
                if ch == '"' {
                    if chars.peek() == Some(&'"') {
                        field.push('"');
                        chars.next();
                    } else {
                        in_quotes = false;
                    }
                } else {
                    field.push(ch);
                }
                continue;
            }
            match ch {
                '"' => {
                    in_quotes = true;
                    quoted = true;
                }
                '\t' => {
                    row.push(cell_of(&field, quoted));
                    field.clear();
                    quoted = false;
                }
                '\r' => {}
                '\n' => {
                    row.push(cell_of(&field, quoted));
                    field.clear();
                    quoted = false;
                    rows.push(std::mem::take(&mut row));
                }
                _ => field.push(ch),
            }
        }
        if !field.is_empty() || quoted || !row.is_empty() {
            row.push(cell_of(&field, quoted));
            rows.push(row);
        }
        rows
    }

    /// 系统剪贴板文本 → 粘贴矩阵。与普通粘贴共用 parse_tsv，保留中间空行。
    pub(crate) fn parse_clip_text(text: &str) -> Vec<Vec<(sheetcore::model::Cell, u16)>> {
        Self::parse_tsv(text)
    }
}
