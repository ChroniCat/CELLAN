//! Import of external Office clipboard formats and conversion to cells and styles.

use crate::{NoticeBanner, NoticeLevel, SheetApp, office_clipboard};
use sheetcore::{Cell, Value};

impl SheetApp {
    /// 富格式块和 TSV 文本是否来自同一次复制（用于识别过期 HTML）。
    pub(super) fn office_block_tsv(block: &office_clipboard::OfficeBlock) -> String {
        let width = block
            .rows
            .iter()
            .map(|row| row.iter().map(|c| c.colspan.max(1) as usize).sum())
            .max()
            .unwrap_or(0);
        let mut tsv = String::new();
        for (i, row) in block.rows.iter().enumerate() {
            if i > 0 {
                tsv.push('\n');
            }
            let mut ci = 0usize;
            for cell in row {
                if ci > 0 {
                    tsv.push('\t');
                }
                tsv.push_str(&cell.text.replace('\r', ""));
                ci += 1;
                for _ in 1..cell.colspan.max(1) {
                    tsv.push('\t');
                    ci += 1;
                }
            }
            while ci < width {
                tsv.push('\t');
                ci += 1;
            }
        }
        tsv
    }

    /// 把从 Excel 复制来的块贴进来：值 + 格式 + 合并
    pub(crate) fn paste_office_block(&mut self, block: &office_clipboard::OfficeBlock) -> bool {
        if block.is_empty() {
            return false;
        }
        let (r0, c0) = self.sel;
        // 统一按 HTML 表格占位规则计算每格的真实 (rel_r, rel_c)，确保 rowspan 覆盖的下方列自动右移，不左偏覆盖
        let mut occupied: std::collections::HashSet<(u32, u32)> = std::collections::HashSet::new();
        let mut placements: Vec<(usize, usize, u32, u32)> =
            Vec::with_capacity(block.rows.iter().map(Vec::len).sum());
        let mut max_rows: u64 = 0;
        let mut max_cols: u64 = 0;
        let mut incoming = Vec::new();
        for (ri, row) in block.rows.iter().enumerate() {
            let rel_r = ri as u32;
            let mut rel_c = 0u32;
            for (ci, cell) in row.iter().enumerate() {
                while occupied.contains(&(rel_r, rel_c)) {
                    rel_c = rel_c.saturating_add(1);
                }
                let rs = cell.rowspan.max(1);
                let cs = cell.colspan.max(1);
                let end_r = u64::from(rel_r).saturating_add(u64::from(rs));
                let end_c = u64::from(rel_c).saturating_add(u64::from(cs));
                if u64::from(r0).saturating_add(end_r) > 1_048_576
                    || u64::from(c0).saturating_add(end_c) > 16_384
                {
                    self.status = "粘贴区域超过工作表边界或单次 50 万格上限".into();
                    return true;
                }
                max_rows = max_rows.max(end_r);
                max_cols = max_cols.max(end_c);
                placements.push((ri, ci, rel_r, rel_c));
                if rs > 1 || cs > 1 {
                    let abs_r = r0 + rel_r;
                    let abs_c = c0 + rel_c;
                    incoming.push((abs_r, abs_c, abs_r + rs - 1, abs_c + cs - 1));
                    // 仅需把后续行（dr >= 1）的占位写入 occupied；同行右侧由 rel_c += cs 直接跳过
                    for dr in 1..rs {
                        for dc in 0..cs {
                            occupied.insert((rel_r + dr, rel_c + dc));
                        }
                    }
                }
                rel_c = rel_c.saturating_add(cs);
            }
        }
        let rows = max_rows;
        let cols = max_cols;
        if rows == 0 || cols == 0 {
            return false;
        }
        let area = rows.saturating_mul(cols);
        if (self.net.is_some() && area > sheetcore::undo::MAX_DENSE_RECT_KEYS as u64)
            || u64::from(r0) + rows > 1_048_576
            || u64::from(c0) + cols > 16_384
        {
            self.status = "粘贴区域超过工作表边界或单次 50 万格上限".into();
            return true;
        }
        let (rows, cols) = (rows as u32, cols as u32);
        let si = self.sheet_idx;
        let target_rect = (r0, c0, r0 + rows - 1, c0 + cols - 1);
        let use_cow = self.net.is_none() && area > sheetcore::undo::COW_BATCH_THRESHOLD as u64;
        let keys = if use_cow {
            if !self.check_sparse_rect_preflight(si, target_rect, &[], "从 Excel 粘贴") {
                return true;
            }
            Vec::new()
        } else {
            let keys = sheetcore::undo::rect_keys(r0, c0, r0 + rows - 1, c0 + cols - 1);
            if !self.write_preflight(&keys, "从 Excel 粘贴") {
                return true;
            }
            keys
        };
        let merge_edit = match self.paste_merge_edit(target_rect, &incoming, None) {
            Ok(edit) => edit,
            Err(message) => { self.status = message.into(); return true; }
        };
        let before_cow = use_cow.then(|| self.wb.sheet(si).clone());
        if let Some(edit) = &merge_edit {
            let Some(changes) = edit.apply(&mut self.wb.sheets, true) else {
                self.status = "粘贴区域中的合并单元格相互冲突".into();
                return true;
            };
            if let Some(net) = &self.net {
                for (sheet, rect, on) in changes { net.broadcast_merge(sheet, rect, on); }
            }
        }
        let txn = if use_cow { Vec::new() } else { self.txn_begin(&keys) };
        let mut touched = if use_cow {
            Vec::new()
        } else {
            Vec::with_capacity(block.rows.iter().map(Vec::len).sum())
        };
        #[derive(Hash, PartialEq, Eq)]
        struct OfficeStyleKey<'a> {
            base_id: u16,
            bold: bool,
            italic: bool,
            underline: bool,
            strike: bool,
            fill: Option<[u8; 3]>,
            color: Option<[u8; 3]>,
            halign: u8,
            valign: u8,
            wrap: bool,
            border: Option<[u8; 3]>,
            borders: [Option<[u8; 3]>; 4],
            font: &'a str,
            size_bits: u32,
            num_format: &'a str,
        }
        let mut style_cache: std::collections::HashMap<OfficeStyleKey<'_>, Option<u16>> =
            std::collections::HashMap::new();
        let parse_office_num = |s: &str| -> Result<f64, std::num::ParseFloatError> {
            if s.as_bytes().contains(&b',') {
                s.replace(',', "").parse::<f64>()
            } else {
                s.parse::<f64>()
            }
        };
        for &(ri, ci, rel_r, rel_c) in &placements {
            let cell = &block.rows[ri][ci];
            let r = r0 + rel_r;
            let c = c0 + rel_c;
            if !use_cow {
                touched.push((si, r, c));
            }
                // 值：带公式的按缓存值类型走，之后统一重算
                let text = cell.text.trim();
                let value = if cell.formula.is_some() {
                    match cell.kind {
                        0 => parse_office_num(text)
                            .map(Value::Number)
                            .unwrap_or(Value::Empty),
                        2 => Value::Bool(text == "1" || text.eq_ignore_ascii_case("true")),
                        4 => sheetcore::formula::ErrKind::from_text(text)
                            .map(Value::Error)
                            .unwrap_or(Value::Text(text.into())),
                        _ => {
                            if text.is_empty() {
                                Value::Empty
                            } else {
                                Value::Text(text.into())
                            }
                        }
                    }
                } else if let Some(raw) = cell.raw.as_deref() {
                    // HTML 的 x:num/x:str 是原始值；text 只是显示文本。日期按序列号
                    // 存回，配合 mso-number-format 保持类型和显示都正确。
                    if raw.is_empty() {
                        Value::Empty
                    } else if cell.kind == 2 {
                        Value::Bool(raw == "1" || raw.eq_ignore_ascii_case("true"))
                    } else if cell.kind == 0 {
                        match parse_office_num(raw) {
                            Ok(n) => Value::Number(n),
                            Err(_) => Value::Text(raw.into()),
                        }
                    } else {
                        Value::Text(raw.into())
                    }
                } else if text.is_empty() {
                    Value::Empty
                } else if cell.kind == 4 {
                    // 从 Excel 复制的错误格：还原成错误值，别当文本也别当 0
                    sheetcore::formula::ErrKind::from_text(text)
                        .map(Value::Error)
                        .unwrap_or_else(|| Value::Text(text.into()))
                } else if cell.kind == 3 {
                    // 从 Excel/WPS 复制的日期：XMLSS 给的是 ISO 文本，不是序列号。
                    // 按**目标簿的日期体系**重算序列号（1900/1904 差 1462 天），
                    // 顺手补一个日期格式，否则粘进来是 45293 这样的裸数字。
                    match sheetcore::clipboard_xmlss::iso_to_serial(self.wb.date_system, text) {
                        Some((serial, _)) => {
                            if cell.num_format.trim().is_empty() {
                                let sid = self.wb.styles.intern(sheetcore::style::CellStyle {
                                    format: "yyyy-mm-dd".into(),
                                    ..Default::default()
                                });
                                self.wb.sheet_mut(si).set_style(r, c, sid);
                            }
                            Value::Number(serial)
                        }
                        None => Value::Text(text.into()),
                    }
                } else if cell.kind == 0 {
                    // 只有 XMLSS 标成 Number 的才按数字落格；Text 一律保持文本，
                    // 否则 "00123" 这类前导零文本会被 f64 吃掉零（Excel 自己粘不丢）。
                    match parse_office_num(text) {
                        Ok(n) => Value::Number(n),
                        Err(_) => Value::Text(text.into()),
                    }
                } else {
                    Value::Text(text.into())
                };
                self.wb.sheet_mut(si).set(
                    r,
                    c,
                    Cell { value, formula: cell.formula.as_deref().map(|f| f.into()) },
                );

                // 格式：按 (基准样式号, Office 样式属性) 缓存去重，避免 10 万格粘贴逐格克隆与重复 intern
                let base_id = self.wb.sheet(si).style_of(r, c);
                let cache_key = OfficeStyleKey {
                    base_id,
                    bold: cell.bold,
                    italic: cell.italic,
                    underline: cell.underline,
                    strike: cell.strike,
                    fill: cell.fill,
                    color: cell.color,
                    halign: cell.halign,
                    valign: cell.valign,
                    wrap: cell.wrap,
                    border: cell.border,
                    borders: cell.borders,
                    font: cell.font.as_str(),
                    size_bits: if cell.size > 0.0 { cell.size.to_bits() } else { 0 },
                    num_format: cell.num_format.as_str(),
                };
                let resolved_id = match style_cache.get(&cache_key).copied() {
                    Some(cached) => cached,
                    None => {
                        let mut st = self.wb.styles.get(base_id).clone();
                        let mut style_changed = false;
                        if cell.bold != st.bold {
                            st.bold = cell.bold;
                            style_changed = true;
                        }
                        if cell.italic != st.italic {
                            st.italic = cell.italic;
                            style_changed = true;
                        }
                        if cell.underline != st.underline {
                            st.underline = cell.underline;
                            style_changed = true;
                        }
                        if cell.strike != st.strikethrough {
                            st.strikethrough = cell.strike;
                            style_changed = true;
                        }
                        if let Some(fill) = cell.fill {
                            st.fill = Some(fill);
                            style_changed = true;
                        }
                        if let Some(color) = cell.color {
                            st.color = color;
                            style_changed = true;
                        }
                        if cell.halign != 0 && cell.halign != st.halign {
                            st.halign = cell.halign;
                            style_changed = true;
                        }
                        let mapped_va = match cell.valign { 2 => 1, 1 => 2, _ => 0 };
                        if mapped_va != 0 && mapped_va != st.valign {
                            st.valign = mapped_va;
                            style_changed = true;
                        }
                        if cell.wrap != st.wrap {
                            st.wrap = cell.wrap;
                            style_changed = true;
                        }
                        if cell.borders.iter().any(|b| b.is_some()) {
                            // cell.borders = [Top, Bottom, Left, Right] -> st.border = [Left, Right, Top, Bottom]
                            for (src_i, dst_i) in [(0, 2), (1, 3), (2, 0), (3, 1)] {
                                if let Some(bc) = cell.borders[src_i] {
                                    st.border[dst_i].style = 1;
                                    st.border[dst_i].color = bc;
                                }
                            }
                            style_changed = true;
                        } else if let Some(bc) = cell.border {
                            for side in st.border.iter_mut() {
                                if side.style == 0 {
                                    side.style = 1;
                                    side.color = bc;
                                }
                            }
                            style_changed = true;
                        }
                        if !cell.font.is_empty() {
                            let f = sheetcore::style::normalize_font(&cell.font);
                            if f != st.font {
                                st.font = f;
                                style_changed = true;
                            }
                        }
                        if cell.size > 0.0 && (cell.size - st.size).abs() > 0.01 {
                            st.size = cell.size;
                            style_changed = true;
                        }
                        if !cell.num_format.is_empty() && cell.num_format != st.format {
                            st.format = cell.num_format.clone();
                            style_changed = true;
                        }
                        let id_opt = style_changed.then(|| self.wb.styles.intern(st));
                        style_cache.insert(cache_key, id_opt);
                        id_opt
                    }
                };
                if let Some(id) = resolved_id {
                    self.wb.sheet_mut(si).set_style(r, c, id);
                }
        }

        // Constants are precedents too. Replacing a formula also removes its old edges.
        self.doc_gen = self.doc_gen.wrapping_add(1);
        self.tasks.last_edit = Some(std::time::Instant::now());
        if let Some(before_sheet) = before_cow {
            self.undo
                .push_cow_batch("从 Excel 粘贴", si, &before_sheet, self.wb.sheet(si), (si, r0, c0));
            self.wb.calc_manual |= self.manual_calc;
            self.recalc_all_and_deps_adaptive();
            std::thread::spawn(move || drop(before_sheet));
        } else {
            self.fast_recalc_cells(&touched);
            let deltas = sheetcore::undo::finish(txn, self.wb.sheet(si));
            if self.net.is_some() {
                let style_keys: Vec<_> = deltas.iter().filter(|d| d.before_style != d.after_style)
                    .map(|d| (d.sheet, d.row, d.col)).collect();
                self.broadcast_cell_styles(&style_keys);
            }
            self.record_deltas_and_merges("从 Excel 粘贴", deltas, merge_edit);
            self.broadcast_cells(&touched);
        }
        self.wb.dirty = true;
        self.sel_end = (r0 + rows - 1, c0 + cols - 1);
        self.status = format!("已粘贴 Excel 区域 {rows} 行 × {cols} 列（含格式与合并）");
        if std::env::var_os("SC_CLIP_LOG").is_some() {
            eprintln!("[paste] office 块 {rows}x{cols}");
        }
        true
    }

    /// 把外部 Excel/WPS 富剪贴板（XMLSS / HTML）转为单元格与样式矩阵，供选择性粘贴和 WPS 粘贴变体共用。
    pub(crate) fn materialize_rich_clipboard_matrix(
        &mut self,
        r0: u32,
        c0: u32,
        rich: office_clipboard::RichClipboard,
    ) -> Option<Vec<Vec<(sheetcore::model::Cell, u16)>>> {
        let date_sys = self.wb.date_system;
        match rich {
            office_clipboard::RichClipboard::Sparse(sb) => {
                let h = sb.rows as usize;
                let w = sb.cols as usize;
                if h == 0 || w == 0 || (h as u64) * (w as u64) > sheetcore::undo::MAX_DENSE_RECT_KEYS as u64 {
                    self.status = "外部剪贴板区域过大或为空".into();
                    return None;
                }
                let mut grid = vec![vec![(sheetcore::model::Cell::default(), 0u16); w]; h];
                let mut sty_map: Vec<u16> = Vec::with_capacity(sb.styles.len());
                for s in &sb.styles {
                    let mut st = sheetcore::style::CellStyle::default();
                    st.bold = s.bold;
                    st.italic = s.italic;
                    st.underline = s.underline;
                    st.strikethrough = s.strike;
                    st.fill = s.fill;
                    if let Some(c) = s.color { st.color = c; }
                    st.halign = s.halign;
                    st.valign = s.valign;
                    st.wrap = s.wrap;
                    if !s.font.is_empty() { st.font = sheetcore::style::normalize_font(&s.font); }
                    if s.size > 0.0 { st.size = s.size; }
                    if !s.num_format.is_empty() { st.format = s.num_format.clone(); }
                    if s.borders.iter().any(|b| b.is_some()) {
                        for (si_b, di_b) in [(0, 2), (1, 3), (2, 0), (3, 1)] {
                            if let Some(bc) = s.borders[si_b] {
                                st.border[di_b].style = 1;
                                st.border[di_b].color = bc;
                            }
                        }
                    } else if let Some(bc) = s.border {
                        for side in &mut st.border { side.style = 1; side.color = bc; }
                    }
                    sty_map.push(self.wb.styles.intern(st));
                }
                for sc in &sb.cells {
                    let (rr, cc) = (sc.row as usize, sc.col as usize);
                    if rr >= h || cc >= w { continue; }
                    let val = match sc.kind {
                        sheetcore::clipboard_xmlss::Kind::Number => sc.text.trim().replace(',', "").parse::<f64>()
                            .map(sheetcore::model::Value::Number)
                            .unwrap_or_else(|_| if sc.text.is_empty() { sheetcore::model::Value::Empty } else { sheetcore::model::Value::Text(sc.text.clone().into()) }),
                        sheetcore::clipboard_xmlss::Kind::Bool => sheetcore::model::Value::Bool(sc.text == "1" || sc.text.eq_ignore_ascii_case("true")),
                        sheetcore::clipboard_xmlss::Kind::Error => sheetcore::formula::ErrKind::from_text(sc.text.trim())
                            .map(sheetcore::model::Value::Error)
                            .unwrap_or_else(|| sheetcore::model::Value::Text(sc.text.clone().into())),
                        sheetcore::clipboard_xmlss::Kind::DateTime => sheetcore::clipboard_xmlss::iso_to_serial(date_sys, &sc.text)
                            .map(|(ser, _)| sheetcore::model::Value::Number(ser))
                            .unwrap_or_else(|| sheetcore::model::Value::Text(sc.text.clone().into())),
                        sheetcore::clipboard_xmlss::Kind::Text => if sc.text.is_empty() { sheetcore::model::Value::Empty } else { sheetcore::model::Value::Text(sc.text.clone().into()) },
                    };
                    let formula = sc.r1c1_formula.as_deref().map(|f| {
                        sheetcore::clipboard_xmlss::r1c1_to_a1(f, r0 + sc.row, c0 + sc.col).into()
                    });
                    let sid = sty_map.get(sc.style_idx as usize).copied().unwrap_or(0);
                    grid[rr][cc] = (sheetcore::model::Cell { value: val, formula }, sid);
                }
                Some(grid)
            }
            office_clipboard::RichClipboard::Block(blk) => {
                let mut grid = Vec::with_capacity(blk.rows.len());
                for row in blk.rows {
                    let mut out_row = Vec::with_capacity(row.len());
                    for cell in row {
                        let mut st = sheetcore::style::CellStyle::default();
                        st.bold = cell.bold;
                        st.italic = cell.italic;
                        st.underline = cell.underline;
                        st.strikethrough = cell.strike;
                        st.fill = cell.fill;
                        if let Some(c) = cell.color { st.color = c; }
                        st.halign = cell.halign;
                        st.valign = cell.valign;
                        st.wrap = cell.wrap;
                        if !cell.font.is_empty() { st.font = sheetcore::style::normalize_font(&cell.font); }
                        if cell.size > 0.0 { st.size = cell.size; }
                        if !cell.num_format.is_empty() { st.format = cell.num_format.clone(); }
                        if cell.borders.iter().any(|b| b.is_some()) {
                            for (si_b, di_b) in [(0, 2), (1, 3), (2, 0), (3, 1)] {
                                if let Some(bc) = cell.borders[si_b] {
                                    st.border[di_b].style = 1;
                                    st.border[di_b].color = bc;
                                }
                            }
                        } else if let Some(bc) = cell.border {
                            for side in &mut st.border { side.style = 1; side.color = bc; }
                        }
                        let sid = self.wb.styles.intern(st);
                        let raw_text = cell.raw.as_deref().unwrap_or(cell.text.trim());
                        let val = if raw_text.is_empty() {
                            sheetcore::model::Value::Empty
                        } else if cell.kind == 0 {
                            raw_text.replace(',', "").parse::<f64>()
                                .map(sheetcore::model::Value::Number)
                                .unwrap_or_else(|_| sheetcore::model::Value::Text(raw_text.into()))
                        } else if cell.kind == 2 {
                            sheetcore::model::Value::Bool(raw_text == "1" || raw_text.eq_ignore_ascii_case("true"))
                        } else if cell.kind == 3 {
                            sheetcore::clipboard_xmlss::iso_to_serial(date_sys, raw_text)
                                .map(|(ser, _)| sheetcore::model::Value::Number(ser))
                                .unwrap_or_else(|| sheetcore::model::Value::Text(raw_text.into()))
                        } else if cell.kind == 4 {
                            sheetcore::formula::ErrKind::from_text(raw_text)
                                .map(sheetcore::model::Value::Error)
                                .unwrap_or_else(|| sheetcore::model::Value::Text(raw_text.into()))
                        } else {
                            sheetcore::model::Value::Text(raw_text.into())
                        };
                        out_row.push((sheetcore::model::Cell { value: val, formula: cell.formula.map(Into::into) }, sid));
                    }
                    grid.push(out_row);
                }
                Some(grid)
            }
            office_clipboard::RichClipboard::HtmlTruncated(limit) => {
                self.report_html_clipboard_limit(limit);
                None
            }
        }
    }

    pub(super) fn report_html_clipboard_limit(&mut self, limit: office_clipboard::HtmlLimitKind) {
        let msg = limit.user_message();
        self.status = msg.clone();
        self.set_notice(NoticeBanner {
            level: NoticeLevel::Warn,
            title: "剪贴板 HTML 数据超限，已中止粘贴".into(),
            detail: msg,
            actions: Vec::new(),
            tag: Some("clipboard_html_limit"),
            target_doc_uid: Some(self.doc_uid),
        });
    }

    /// 自检：读系统剪贴板里的 HTML 表格并粘贴
    pub(crate) fn paste_probe(&mut self) {
        if std::env::var("SC_PASTE_HTML").is_err() || self.paste_done || self.tasks.open_rx.is_some() {
            return;
        }
        self.paste_done = true;
        // SC_PASTE_AT="行,列"（0 基）指定粘贴左上角，默认 A1
        let at = std::env::var("SC_PASTE_AT")
            .ok()
            .and_then(|v| {
                let mut it = v.split(',').map(|x| x.trim().parse::<u32>().ok());
                Some((it.next()??, it.next()??))
            })
            .unwrap_or((0, 0));
        self.sel = at;
        self.sel_end = at;
        match office_clipboard::read_table() {
            Some(block) => {
                eprintln!("[paste] 读到表格 {} 行 × {} 列", block.rows.len(), block.cols());
                let ok = self.paste_office_block(&block);
                let sh = self.wb.sheet(self.sheet_idx);
                let a1 = sh.get(0, 0).map(|c| c.value.display()).unwrap_or_default();
                let st = sh.style_of(0, 0);
                let cs = self.wb.styles.get(st);
                eprintln!(
                    "[paste] 成功={ok} A1={a1:?} 粗体={} 填充={:?} 合并数={}",
                    cs.bold,
                    cs.fill,
                    sh.merges.len()
                );
                for (r, c) in [(0u32, 0u32), (0, 1), (1, 0), (1, 1)] {
                    if let Some(x) = sh.get(r, c) {
                        let sid = sh.style_of(r, c);
                        let st = self.wb.styles.get(sid);
                        eprintln!(
                            "[paste] ({r},{c}) 值={:?} 公式={:?} 字体={:?} 字号={} 格式={:?}",
                            x.value.display(),
                            x.formula,
                            st.font,
                            st.size,
                            st.format
                        );
                    }
                }
            }
            None => eprintln!("[paste] 剪贴板里没有 HTML 表格"),
        }
        // SC_PASTE_DUMP="r1,c1,r2,c2"：把粘贴结果按格打印，供三方对拍脚本比对
        if let Ok(spec) = std::env::var("SC_PASTE_DUMP") {
            let n: Vec<u32> = spec.split(',').filter_map(|x| x.trim().parse().ok()).collect();
            if n.len() == 4 {
                let sh = self.wb.sheet(self.sheet_idx);
                for r in n[0]..=n[2] {
                    for c in n[1]..=n[3] {
                        let (val, formula, fmt, bold, fill) = match sh.get(r, c) {
                            Some(cell) => {
                                let sid = sh.style_of(r, c);
                                let st = self.wb.styles.get(sid);
                                (
                                    cell.value.display(),
                                    cell.formula.as_deref().unwrap_or("").to_string(),
                                    st.format.clone(),
                                    st.bold,
                                    st.fill.map(|f| format!("{f:?}")).unwrap_or_default(),
                                )
                            }
                            None => (String::new(), String::new(), String::new(), false, String::new()),
                        };
                        eprintln!(
                            "[dump] ({r},{c}) v={val:?} f={formula:?} fmt={fmt:?} bold={bold} fill={fill:?}"
                        );
                    }
                }
                eprintln!("[dump] merges={:?}", sh.merges);
            }
        }
        if std::env::var("SC_PASTE_CHECK").is_ok() {
            // 改一下被引用的格子，看粘进来的公式是不是活的
            let key = (self.sheet_idx, 0u32, 0u32);
            if let Some(c) = self.wb.sheet(self.sheet_idx).get(0, 0).map(|c| c.into_owned()) {
                let mut c2 = c;
                c2.value = Value::Number(5.0);
                self.wb.sheet_mut(self.sheet_idx).set(0, 0, c2);
                self.update_dependency(key);
                sheetcore::formula::recalc_dirty(&mut self.wb, &self.deps, &[key]);
                let b1 = self
                    .wb
                    .sheet(self.sheet_idx)
                    .get(0, 1)
                    .map(|x| x.value.display())
                    .unwrap_or_default();
                eprintln!("[paste] A1=5 后 B1={b1:?}（应 7）");
            }
        }
    }
}
