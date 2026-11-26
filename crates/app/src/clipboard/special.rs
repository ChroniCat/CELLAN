//! Paste Special options, source-coordinate translation and undo.

use crate::{SheetApp, office_clipboard};
use eframe::egui;
use sheetcore::cell_name;

impl SheetApp {
    /// 选择性粘贴对话框
    pub(crate) fn paste_special_dialog(&mut self, ctx: &egui::Context) {
        self.draw_paste_special_wps(ctx);
    }

    /// 执行选择性粘贴：内部剪贴板优先，其次系统剪贴板（富格式 XMLSS/HTML 优先于纯文本）
    pub(crate) fn paste_special_apply(&mut self, ctx: &egui::Context) {
        let text = self.system_paste_text();
        self.paste_special_apply_impl(ctx, text, None, true);
    }


    /// text 是系统剪贴板文本；None/空 表示没有可粘贴的外部文本。
    pub(crate) fn paste_special_apply_text(&mut self, ctx: &egui::Context, ext_text: Option<String>) {
        self.paste_special_apply_impl(ctx, ext_text, None, false);
    }

    #[cfg(test)]
    pub(crate) fn paste_special_apply_rich(
        &mut self,
        ctx: &egui::Context,
        ext_text: Option<String>,
        ext_rich: Option<office_clipboard::RichClipboard>,
    ) {
        self.paste_special_apply_impl(ctx, ext_text, ext_rich, false);
    }

    fn paste_special_apply_impl(
        &mut self,
        _ctx: &egui::Context,
        ext_text: Option<String>,
        ext_rich: Option<office_clipboard::RichClipboard>,
        allow_os_rich: bool,
    ) {
        if self.edit_blocked() {
            return;
        }
        let (r0, c0) = self.sel;
        let target_sheet_name = self.wb.sheet(self.sheet_idx).name.clone();

        // 内部剪贴板：matrix 是要粘贴的格子，coords 记录每格在源表里的绝对坐标。
        // 外部文本没有源坐标，公式只能按原文粘贴（不偏移）。
        // 关键：内部 Clip 不能遮盖后复制到系统剪贴板的新内容——与普通粘贴同一判定。
        let internal = self.internal_clip_for(ext_text.as_deref(), true);
        if self.ps_kind == 5 {
            let Some(clip) = internal else { self.status = "外部剪贴板不含源列宽信息".into(); return };
            let (first_row, first_col, last_row, last_col) = clip.rect;
            let source = if let Some(range) = &clip.deferred { range.book().sheet(0) }
                else if let Some(sh) = self.wb.sheets.get(clip.sheet) { sh }
                else { self.status = "找不到复制列宽的工作表".into(); return };
            let height = clip.source_rows.as_ref().map_or(last_row - first_row + 1, |rows| rows.len() as u32);
            let width = if self.ps_transpose { height } else { last_col - first_col + 1 };
            if width == 0 || c0.saturating_add(width) > sheetcore::sync::MAX_COLS {
                self.status = "选择性粘贴超出工作表行列边界".into(); return;
            }
            // Paste Widths changes the entire columns, independent of selected row
            // count. Check column-format permission and all leases on these columns.
            let widths: Vec<_> = (0..width).map(|j| source.cols.size(if self.ps_transpose { first_col } else { first_col+j })).collect();
            let si = self.sheet_idx;
            if !self.write_preflight_op(&[], "粘贴列宽", sheetcore::protection::ProtOp::FormatColumns) { return; }
            let rect = (0,c0,sheetcore::sync::MAX_ROWS-1,c0+width-1);
            let holder = self.net.as_ref().and_then(|n| n.host_lock_conflict_rect(si,rect))
                .map(|(_,name,r,c)| (name,r,c)).or_else(|| {
                    let site = self.net.as_ref().map_or(0, |n| n.site);
                    self.locks.conflicts_rect(si,rect.0,rect.1,rect.2,rect.3,site,self.lock_now())
                        .map(|((r,c),h)| (h.name.clone(),r,c))
                });
            if let Some((name,r,c)) = holder { self.status = format!("{name} 正在编辑 {}，不能粘贴列宽",cell_name(r,c)); return; }
            let was_shared_only = self.only_shared_cell_edits();
            let layout_ok_before = {
                let sh = self.wb.sheet(si);
                !sh.layout_dirty || self.sheet_layout_is_synced(sh)
            };
            let sh = self.wb.sheet_mut(si);
            let prior_revision = sh.cols.revision();
            let before_widths: Vec<(u32, Option<f32>, Option<f32>)> = widths
                .iter()
                .enumerate()
                .map(|(j, &w)| {
                    let col = c0 + j as u32;
                    let old = sh.cols.size(col);
                    (col, Some(old), Some(w))
                })
                .collect();
            let sync_widths: Vec<(u32, f32)> = widths
                .iter()
                .enumerate()
                .map(|(j, &w)| (c0 + j as u32, w))
                .collect();
            sh.cols.set_many((c0..).zip(widths));
            if prior_revision != sh.cols.revision() {
                sh.layout_dirty = true;
                self.undo.push_meta(
                    "粘贴列宽",
                    (si, r0, c0),
                    sheetcore::undo::MetaEdit {
                        sheet: si,
                        col_widths: before_widths,
                        ..Default::default()
                    },
                );
                if let Some(net) = self.net.as_ref() {
                    let synced = net.broadcast_sheet_meta(si, sheetcore::sync::SheetMetaKind::ColWidths(sync_widths));
                    self.mark_sheet_layout_synced(si, synced && layout_ok_before);
                }
                self.wb.dirty = true;
                self.doc_gen = self.doc_gen.wrapping_add(1);
                if was_shared_only && self.is_delegated_collab_client() {
                    self.shared_cell_generation = Some((self.doc_uid, self.doc_gen));
                }
                self.tasks.last_edit = Some(std::time::Instant::now());
            }
            self.status = format!("已粘贴 {width} 列宽");
            return;
        }
        if internal.is_some_and(|clip| office_clipboard::delayed::is_full_columns(clip.rect)) {
            if self.ps_op == 0 && !self.ps_transpose && !self.ps_skip_blanks && matches!(self.ps_kind, 0 | 1) {
                self.clip_paste(self.ps_kind == 1, ext_text);
            } else {
                self.status = "整列支持普通粘贴和粘贴数值，其他选项请先选中数据区域".into();
            }
            return;
        }
        let src_sheet_idx = internal.and_then(|clip| clip.live_sheet_index(&self.wb));
        let mut src_sheet_name: Option<String> = None;
        let (mut matrix, mut coords) = if let Some(clip) = internal {
            src_sheet_name = clip.deferred.as_ref().map(|range| range.book().sheet(0).name.clone())
                .or_else(|| self.wb.sheets.get(clip.sheet).map(|sh| sh.name.clone()));
            let mut coords: Vec<Vec<(u32, u32)>> = Vec::new();
            for (i, row) in clip.cells().iter().enumerate() {
                coords.push(
                    (0..row.len())
                        .map(|j| (clip.source_row(i), clip.source_col(j)))
                        .collect(),
                );
            }
            (clip.cells().to_vec(), Some(coords))
        } else if let Some(rich) = ext_rich.or_else(|| {
            // Excel / WPS 复制单元格时会同时写入 CF_UNICODETEXT 与 XML Spreadsheet / HTML Format，
            // 决不能因 ext_text 为 Some 就跳过富格式读取，否则“仅格式”等选择性粘贴会丢掉源样式。
            (allow_os_rich && !cfg!(test) && office_clipboard::has_rich_table_format())
                .then(|| office_clipboard::read_rich_table_ext(self.ps_kind == 1))
                .flatten()
        }) {
            let Some(grid) = self.materialize_rich_clipboard_matrix(r0, c0, rich) else {
                return;
            };
            (grid, None)
        } else {
            let text = ext_text.unwrap_or_default();
            if text.is_empty() {
                self.status = "剪贴板是空的".into();
                return;
            }
            // 与普通粘贴共用同一解析器：保留中间空行，= 开头按公式解析
            (Self::parse_clip_text(&text), None)
        };
        if matrix.is_empty() {
            self.status = "剪贴板是空的".into();
            return;
        }
        let init_h = matrix.len() as u64;
        let init_w = matrix.iter().map(|r| r.len()).max().unwrap_or(0) as u64;
        if init_h * init_w > sheetcore::undo::MAX_DENSE_RECT_KEYS as u64 {
            self.status = "选择性粘贴范围过大（单次最多支持 50 万个单元格）".into();
            return;
        }
        if self.ps_transpose {
            let h = matrix.len();
            let w = matrix.iter().map(|r| r.len()).max().unwrap_or(0);
            let mut t = vec![
                vec![
                    (
                        sheetcore::model::Cell {
                            value: sheetcore::model::Value::Empty,
                            formula: None,
                        },
                        0u16,
                    );
                    h
                ];
                w
            ];
            for (r, row) in matrix.iter().enumerate() {
                for (c, cell) in row.iter().enumerate() {
                    t[c][r] = cell.clone();
                }
            }
            matrix = t;
            if let Some(co) = coords.take() {
                let mut tc = vec![vec![(0u32, 0u32); h]; w];
                for (r, row) in co.iter().enumerate() {
                    for (c, rc) in row.iter().enumerate() {
                        tc[c][r] = *rc;
                    }
                }
                coords = Some(tc);
            }
        }
        let base_rows = matrix.len() as u32;
        let base_cols = matrix.iter().map(|r| r.len()).max().unwrap_or(0) as u32;
        if base_rows == 0 || base_cols == 0 {
            return;
        }
        // 选择性粘贴平铺扩展 + 筛选可见行语义（与普通 Ctrl+V 对齐：绝不暗中覆写筛选隐藏行）
        let si_cur = self.sheet_idx;
        let (sel_r1, sel_c1, sel_r2, sel_c2) = self.sel_rect();
        let sel_h = sel_r2 - sel_r1 + 1;
        let sel_w = sel_c2 - sel_c1 + 1;
        let sh_ref = self.wb.sheet(si_cur);
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
        if vis_sel_rows.as_ref().is_some_and(|v| v.is_empty()) {
            self.status = "所选区域内没有可粘贴的可见单元格".into();
            return;
        }
        let eff_h = vis_sel_rows.as_ref().map_or(sel_h, |v| v.len() as u32);
        let (r0, c0, tile_r, tile_c, pre_dst_rows) = if eff_h > 0
            && (eff_h > base_rows || sel_w > base_cols)
            && eff_h % base_rows == 0
            && sel_w % base_cols == 0
            && u64::from(eff_h) * u64::from(sel_w) <= sheetcore::undo::MAX_DENSE_RECT_KEYS as u64
        {
            let start_r = vis_sel_rows
                .as_ref()
                .and_then(|v| v.first().copied())
                .unwrap_or(sel_r1);
            (start_r, sel_c1, eff_h / base_rows, sel_w / base_cols, vis_sel_rows)
        } else if let Some(vrows) = vis_sel_rows
            && vrows.len() == base_rows as usize
            && sel_w == base_cols
        {
            (vrows[0], sel_c1, 1, 1, Some(vrows))
        } else if sel_h == base_rows && sel_w == base_cols {
            (sel_r1, sel_c1, 1, 1, None)
        } else {
            (r0, c0, 1, 1, None)
        };
        if tile_r > 1 || tile_c > 1 {
            let orig_m = std::mem::take(&mut matrix);
            let orig_co = coords.take();
            let mut tiled_m = Vec::with_capacity((base_rows * tile_r) as usize);
            let mut tiled_co = orig_co.as_ref().map(|_| Vec::with_capacity((base_rows * tile_r) as usize));
            for _tr in 0..tile_r {
                for (i, row) in orig_m.iter().enumerate() {
                    let mut out_r = Vec::with_capacity((base_cols * tile_c) as usize);
                    let mut out_c = orig_co.as_ref().map(|_| Vec::with_capacity((base_cols * tile_c) as usize));
                    for _tc in 0..tile_c {
                        for j in 0..(base_cols as usize) {
                            out_r.push(row.get(j).cloned().unwrap_or_else(|| (sheetcore::model::Cell::default(), 0)));
                            if let (Some(co_src), Some(oc)) = (&orig_co, &mut out_c) {
                                oc.push(co_src.get(i).and_then(|r| r.get(j)).copied().unwrap_or((0, 0)));
                            }
                        }
                    }
                    tiled_m.push(out_r);
                    if let (Some(tco), Some(oc)) = (&mut tiled_co, out_c) {
                        tco.push(oc);
                    }
                }
            }
            matrix = tiled_m;
            coords = tiled_co;
        }
        let rows = matrix.len() as u32;
        let cols = matrix.iter().map(|r| r.len()).max().unwrap_or(0) as u32;
        if rows == 0 || cols == 0 {
            return;
        }
        if (rows as u64) * (cols as u64) > sheetcore::undo::MAX_DENSE_RECT_KEYS as u64 {
            self.status = "选择性粘贴范围过大（单次最多支持 50 万个单元格）".into();
            return;
        }
        // 若多行选择性粘贴跨越筛选隐藏行，则只落到可见行上，不覆盖隐藏记录
        let dst_rows: Option<Vec<u32>> = if let Some(pre) = pre_dst_rows {
            Some(pre)
        } else if rows > 1
            && !self.wb.sheet(si_cur).hidden_rows.is_empty()
            && (r0..r0.saturating_add(rows))
                .any(|rr| self.wb.sheet(si_cur).hidden_rows.contains(&rr))
        {
            let sh = self.wb.sheet(si_cur);
            let mut out = Vec::with_capacity(rows as usize);
            let mut cur = r0;
            while out.len() < rows as usize {
                if cur > 1_048_575 {
                    self.status = "选择性粘贴超出工作表行列边界".into();
                    return;
                }
                if !sh.hidden_rows.contains(&cur) && !sh.hidden_rows_user.contains(&cur) {
                    out.push(cur);
                }
                cur = cur.saturating_add(1);
            }
            Some(out)
        } else {
            None
        };
        let target_last_row = dst_rows
            .as_ref()
            .and_then(|v| v.last().copied())
            .unwrap_or_else(|| r0.saturating_add(rows - 1));
        if target_last_row > 1_048_575 || c0.saturating_add(cols - 1) > 16_383 {
            self.status = "选择性粘贴超出工作表行列边界".into();
            return;
        }
        let keys: Vec<(u32, u32)> = if let Some(ref vr) = dst_rows {
            let mut ks = Vec::with_capacity((rows as usize) * (cols as usize));
            for &rr in vr {
                for j in 0..cols {
                    ks.push((rr, c0 + j));
                }
            }
            ks
        } else {
            sheetcore::undo::rect_keys(r0, c0, r0 + rows - 1, c0 + cols - 1)
        };
        if !self.write_preflight(&keys, "选择性粘贴") {
            return;
        }
        let before_dv = (self.ps_kind == 9).then(|| self.wb.sheet(si_cur).dv.clone());
        // 预先快照源批注、线程批注与源数据验证规则，防止源区与目标区重叠时读到刚被覆写的新值
        let src_notes_snap: Option<Vec<Vec<Option<sheetcore::notes::Note>>>> =
            if self.ps_kind == 8 {
                if let (Some(co), Some(ssi)) = (&coords, src_sheet_idx) {
                    let src_sh = self.wb.sheet(ssi);
                    Some(
                        co.iter()
                            .map(|row_co| {
                                row_co
                                    .iter()
                                    .map(|&(sr, sc)| {
                                        src_sh
                                            .notes
                                            .get(&sheetcore::model::CellRef::new(sr, sc))
                                            .cloned()
                                    })
                                    .collect()
                            })
                            .collect(),
                    )
                } else {
                    None
                }
            } else {
                None
            };
        let src_threads_snap: Option<Vec<Vec<Option<sheetcore::notes::ThreadedComment>>>> =
            if self.ps_kind == 8 {
                if let (Some(co), Some(ssi)) = (&coords, src_sheet_idx) {
                    let src_sh = self.wb.sheet(ssi);
                    Some(
                        co.iter()
                            .map(|row_co| {
                                row_co
                                    .iter()
                                    .map(|&(sr, sc)| {
                                        src_sh
                                            .threads
                                            .get(&sheetcore::model::CellRef::new(sr, sc))
                                            .cloned()
                                    })
                                    .collect()
                            })
                            .collect(),
                    )
                } else {
                    None
                }
            } else {
                None
            };
        if self.ps_kind == 8 && self.net.as_ref().is_some_and(|net| !net.supports_thread_changes()) {
            let sh = self.wb.sheet(si_cur);
            let touches_thread = matrix.iter().enumerate().any(|(i, row)| row.iter().enumerate().any(|(j, (cell, _))| {
                if self.ps_skip_blanks && cell.is_blank() { return false; }
                if src_threads_snap.as_ref().is_some_and(|g| g.get(i).and_then(|row| row.get(j)).is_some_and(Option::is_some)) { return true; }
                let r = dst_rows.as_ref().map_or(r0 + i as u32, |vr| vr[i]);
                let c = c0 + j as u32;
                let (ar, ac) = sh.merge_at(r, c).map(|m| (m.0, m.1)).unwrap_or((r, c));
                src_notes_snap.as_ref().is_some_and(|g| g.get(i).and_then(|row| row.get(j)).is_some_and(Option::is_some)) && sh.threads.contains_key(&sheetcore::model::CellRef::new(ar, ac))
            }));
            if !self.thread_change_allowed(touches_thread) { return; }
        }
        let src_dv_snap: Option<Vec<Vec<Option<sheetcore::dv::DataValidation>>>> =
            if self.ps_kind == 9 {
                if let (Some(co), Some(ssi)) = (&coords, src_sheet_idx) {
                    let src_sh = self.wb.sheet(ssi);
                    Some(
                        co.iter()
                            .map(|row_co| {
                                row_co
                                    .iter()
                                    .map(|&(sr, sc)| sheetcore::dv::find(&src_sh.dv, sr, sc).cloned())
                                    .collect()
                            })
                            .collect(),
                    )
                } else {
                    None
                }
            } else {
                None
            };
        let mut note_undo: Vec<((u32, u32), Option<sheetcore::notes::Note>, Option<sheetcore::notes::Note>)> = Vec::new();
        let mut thread_undo: Vec<((u32, u32), Option<sheetcore::notes::ThreadedComment>, Option<sheetcore::notes::ThreadedComment>)> = Vec::new();
        let txn = if matches!(self.ps_kind, 8 | 9) { Vec::new() } else { self.txn_begin(&keys) };
        let mut wire_styles: Vec<(u32, u32, sheetcore::style::CellStyle)> = Vec::new();
        for (i, row) in matrix.iter().enumerate() {
            let r = dst_rows
                .as_ref()
                .and_then(|vr| vr.get(i).copied())
                .unwrap_or_else(|| r0 + i as u32);
            for (j, (src, sid)) in row.iter().enumerate() {
                if self.ps_skip_blanks && src.is_blank() {
                    continue;
                }
                let c = c0 + j as u32;
                if self.ps_kind == 8 {
                    let src_note = src_notes_snap
                        .as_ref()
                        .and_then(|g| g.get(i))
                        .and_then(|row_n| row_n.get(j))
                        .and_then(|opt| opt.clone());
                    let src_thread = src_threads_snap
                        .as_ref()
                        .and_then(|g| g.get(i))
                        .and_then(|row_t| row_t.get(j))
                        .and_then(|opt| opt.clone());
                    if src_note.is_some() || src_thread.is_some() {
                        let sh = self.wb.sheet_mut(si_cur);
                        let (ar, ac) = sh.merge_at(r, c).map(|(mr1, mc1, _, _)| (mr1, mc1)).unwrap_or((r, c));
                        let cref = sheetcore::model::CellRef::new(ar, ac);
                        if let Some(mut n) = src_note {
                            let before_n = sh.notes.get(&cref).cloned();
                            n.r = ar;
                            n.c = ac;
                            sh.notes.insert(cref, n.clone());
                            sh.notes_dirty = true;
                            note_undo.push(((ar, ac), before_n, Some(n)));
                            if src_thread.is_none() && let Some(before) = sh.threads.remove(&cref) {
                                sh.threads_dirty = true;
                                thread_undo.push(((ar, ac), Some(before), None));
                            }
                        }
                        if let Some(mut tc) = src_thread {
                            let before_tc = sh.threads.get(&cref).cloned();
                            tc.r = ar;
                            tc.c = ac;
                            sh.threads.insert(cref, tc.clone());
                            sh.threads_dirty = true;
                            thread_undo.push(((ar, ac), before_tc, Some(tc)));
                        }
                    }
                    continue;
                }
                if self.ps_kind == 9 {
                    if let Some(dv) = src_dv_snap
                        .as_ref()
                        .and_then(|g| g.get(i))
                        .and_then(|row_d| row_d.get(j))
                        .and_then(|opt| opt.clone())
                    {
                        let sh = self.wb.sheet_mut(si_cur);
                        for existing in &mut sh.dv {
                            let mut kept = Vec::with_capacity(existing.ranges.len());
                            for &rng in &existing.ranges {
                                kept.extend(crate::validation::subtract_rect(rng, (r, c, r, c)));
                            }
                            existing.ranges = kept;
                        }
                        sh.dv.retain(|d| !d.ranges.is_empty());
                        if let Some(same) = sh.dv.iter_mut().find(|d| d.same_rule(&dv)) {
                            same.ranges.push((r, c, r, c));
                        } else {
                            let mut rule = dv;
                            rule.ranges = vec![(r, c, r, c)];
                            sh.dv.push(rule);
                        }
                        sh.dv_dirty = true;
                    }
                    continue;
                }
                let mut cell = self
                    .wb
                    .sheet(self.sheet_idx)
                    .get(r, c)
                    .map(|c| c.into_owned())
                    .unwrap_or(sheetcore::model::Cell {
                        value: sheetcore::model::Value::Empty,
                        formula: None,
                    });
                let dst_sid = self.wb.sheet(self.sheet_idx).style_of(r, c);
                let mut style = *sid;
                // 公式按“目标位置 - 源绝对坐标”平移；负偏移由 refs::translate 产出 #REF!
                let shifted_formula = |f: &str| -> String {
                    match (&coords, &src_sheet_name) {
                        (Some(co), Some(_)) => {
                            let (sr, sc) = co[i][j];
                            sheetcore::refs::translate(
                                f,
                                r as i64 - sr as i64,
                                c as i64 - sc as i64,
                                &target_sheet_name,
                            )
                        }
                        _ => f.to_string(),
                    }
                };
                match self.ps_kind {
                    1 => {
                        cell.value = src.value.clone();
                        cell.formula = None;
                    }
                    2 => {
                        if let Some(f) = &src.formula {
                            cell.formula = Some(shifted_formula(f).into());
                        } else {
                            cell.formula = None;
                            cell.value = src.value.clone();
                        }
                    }
                    3 => {
                        style = *sid;
                    }
                    4 => {
                        cell = src.clone();
                        if let Some(f) = &src.formula {
                            cell.formula = Some(shifted_formula(f).into());
                        } else {
                            cell.formula = None;
                        }
                        let mut st = self.wb.styles.get(*sid).clone();
                        st.clear_borders();
                        style = self.wb.styles.intern(st) as u16;
                    }
                    6 => {
                        cell.value = src.value.clone();
                        cell.formula = None;
                        let src_fmt = self.wb.styles.get(*sid).format.clone();
                        let mut st = self.wb.styles.get(dst_sid).clone();
                        st.format = src_fmt;
                        style = self.wb.styles.intern(st) as u16;
                    }
                    7 => {
                        if let Some(f) = &src.formula {
                            cell.formula = Some(shifted_formula(f).into());
                        } else {
                            cell.formula = None;
                            cell.value = src.value.clone();
                        }
                        let src_fmt = self.wb.styles.get(*sid).format.clone();
                        let mut st = self.wb.styles.get(dst_sid).clone();
                        st.format = src_fmt;
                        style = self.wb.styles.intern(st) as u16;
                    }
                    _ => {
                        cell = src.clone();
                        if let Some(f) = &src.formula {
                            cell.formula = Some(shifted_formula(f).into());
                        } else {
                            cell.formula = None;
                        }
                        style = *sid;
                    }
                }
                if self.ps_op != 0 && self.ps_kind != 3 {
                    // Excel 的“运算”语义：目标原值 op 剪贴板值；
                    // 目标已有公式时包裹原公式 `=(orig) op b` 保留动态公式；目标为空按 0 参与运算；文本目标保持不变。
                    let target_cell = self
                        .wb
                        .sheet(self.sheet_idx)
                        .get(r, c)
                        .map(|x| x.into_owned());
                    let target_before = target_cell
                        .as_ref()
                        .map(|x| x.value.clone())
                        .unwrap_or(sheetcore::model::Value::Empty);
                    if let sheetcore::model::Value::Number(b) = src.value {
                        if let Some(orig_f) = target_cell.as_ref().and_then(|x| x.formula.as_deref()) {
                            let op_ch = match self.ps_op {
                                1 => "+",
                                2 => "-",
                                3 => "*",
                                _ => "/",
                            };
                            let rhs = sheetcore::model::format_number(b);
                            let body = orig_f.trim_start_matches('=');
                            cell.formula = Some(format!("=({body}){op_ch}{rhs}").into());
                            cell.value = sheetcore::model::Value::Empty;
                        } else {
                            let a_opt = match target_before {
                                sheetcore::model::Value::Number(a) => Some(a),
                                sheetcore::model::Value::Empty => Some(0.0),
                                _ => None,
                            };
                            if let Some(a) = a_opt {
                                let v = match self.ps_op {
                                    1 => Some(a + b),
                                    2 => Some(a - b),
                                    3 => Some(a * b),
                                    _ => {
                                        if b == 0.0 {
                                            None
                                        } else {
                                            Some(a / b)
                                        }
                                    }
                                };
                                match v {
                                    Some(v) => {
                                        cell.value = sheetcore::model::Value::Number(v);
                                    }
                                    None => {
                                        cell.value = sheetcore::model::Value::Error(
                                            sheetcore::formula::ErrKind::Div0,
                                        );
                                    }
                                }
                                cell.formula = None;
                            } else if let Some(orig) = target_cell {
                                cell = orig;
                            }
                        }
                    } else if let Some(orig) = target_cell {
                        cell = orig;
                    }
                }
                let sheet = self.wb.sheet_mut(self.sheet_idx);
                sheet.set(r, c, cell);
                if matches!(self.ps_kind, 0 | 3 | 4 | 6 | 7) {
                    sheet.set_style(r, c, style);
                    wire_styles.push((r, c, self.wb.styles.get(style).clone()));
                }
            }
        }
        if self.ps_kind == 8 {
            if !note_undo.is_empty() || !thread_undo.is_empty() {
                self.broadcast_comment_diffs(si_cur, &note_undo, &thread_undo);
                self.undo.push_meta(
                    "粘贴批注",
                    (si_cur, r0, c0),
                    sheetcore::undo::MetaEdit {
                        sheet: si_cur,
                        notes: note_undo,
                        threads: thread_undo,
                        ..Default::default()
                    },
                );
            }
        } else if self.ps_kind == 9 {
            if let Some(before) = before_dv {
                let after = self.wb.sheet(si_cur).dv.clone();
                if before != after {
                    self.undo.push_meta(
                        "粘贴数据验证",
                        (si_cur, r0, c0),
                        sheetcore::undo::MetaEdit {
                            sheet: si_cur,
                            dv: Some((before, after)),
                            ..Default::default()
                        },
                    );
                }
            }
        } else {
            self.txn_end("选择性粘贴", txn);
        }
        // 与普通粘贴一致：值/公式和样式都同步给协作者（仅限实际写入的可见目标格）
        if !matches!(self.ps_kind, 8 | 9)
            && let (Some(peer), Some(net)) = (self.peer.as_mut(), self.net.as_ref())
        {
            for (i, row) in matrix.iter().enumerate() {
                let r = dst_rows
                    .as_ref()
                    .and_then(|vr| vr.get(i).copied())
                    .unwrap_or_else(|| r0 + i as u32);
                for j in 0..row.len() {
                    let c = c0 + j as u32;
                    let cell = self.wb.sheet(self.sheet_idx).get(r, c).map(|c| c.into_owned()).unwrap_or_default();
                    let op = peer.local_set(self.sheet_idx, r, c, &cell);
                    net.broadcast_op(op);
                }
            }
            if !wire_styles.is_empty() {
                net.broadcast_style(self.sheet_idx, wire_styles);
            }
        }
        self.sel_end = (target_last_row, c0 + cols - 1);
        if !matches!(self.ps_kind, 3 | 8 | 9) {
            let si = self.sheet_idx;
            let touched: Vec<(usize, u32, u32)> = keys
                .iter()
                .map(|&(r, c)| (si, r, c))
                .collect();
            self.fast_recalc_cells(&touched);
        }
        self.wb.dirty = true;
        self.doc_gen = self.doc_gen.wrapping_add(1);
        self.tasks.last_edit = Some(std::time::Instant::now());
        self.status = format!("已选择性粘贴 {rows} 行 × {cols} 列");
    }
}
