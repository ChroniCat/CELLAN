use super::*;
use sheetcore::{
    model::Sheet,
    sort_filter::{CmpOp, Criterion, Filter, FilterColumn},
};
use std::collections::{HashMap, HashSet};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

#[inline]
fn ascii_contains_ci(haystack: &[u8], needle_lower: &[u8]) -> bool {
    let n = needle_lower.len();
    if n == 0 {
        return true;
    }
    if haystack.len() < n {
        return false;
    }
    let first = needle_lower[0];
    let last_pos = haystack.len() - n;
    for i in 0..=last_pos {
        if haystack[i].to_ascii_lowercase() == first {
            let mut ok = true;
            for j in 1..n {
                if haystack[i + j].to_ascii_lowercase() != needle_lower[j] {
                    ok = false;
                    break;
                }
            }
            if ok {
                return true;
            }
        }
    }
    false
}

#[derive(Default)]
pub(super) struct Choices {
    pub(super) items: Vec<(String, bool)>,
    /// 与 `items` 一一对应的格式化展示文本（空表示直接使用 `items[i].0`）
    pub(super) labels: Vec<String>,
    /// 非 ASCII 项的小写紧凑拼接池；纯 ASCII 项记 `(u32::MAX, 0)` 直接零分配字节匹配，
    /// 百万唯一单号/数字列节省 ~32MB 独立 `String` 分配。
    folded_arena: String,
    folded_spans: Vec<(u32, u32)>,
    /// 搜索词非空时的可见下标表；`all_shown == true` 时第 `k` 项即下标 `k`（省 8MB 恒等表）。
    shown: Vec<usize>,
    all_shown: bool,
    /// 点击「(全选)」时的 O(1) 全局基线覆盖，配合稀疏 `toggled` 反转下标集合实现零字符串克隆。
    default_override: Option<bool>,
    toggled: HashSet<usize>,
    bulk_draft: Option<bool>,
    checked: usize,
    pub(super) checked_total: usize,
    query: String,
    pub(super) numeric: bool,
    pub(super) wake: Option<egui::Context>,
}

impl Choices {
    #[inline]
    pub(super) fn item_checked(&self, idx: usize) -> bool {
        let base = self.default_override.unwrap_or(self.items[idx].1);
        base ^ self.toggled.contains(&idx)
    }

    #[inline]
    fn shown_len(&self) -> usize {
        if self.all_shown {
            self.items.len()
        } else {
            self.shown.len()
        }
    }

    #[inline]
    fn shown_index(&self, pos: usize) -> usize {
        if self.all_shown {
            pos
        } else {
            self.shown[pos]
        }
    }

    #[inline]
    fn matches_folded(&self, idx: usize, q_lower: &str, q_ascii: bool) -> bool {
        let (off, len) = self.folded_spans[idx];
        let raw_hit = if off == u32::MAX {
            if q_ascii {
                ascii_contains_ci(self.items[idx].0.as_bytes(), q_lower.as_bytes())
            } else {
                false
            }
        } else {
            let s = &self.folded_arena[off as usize..(off + len) as usize];
            s.contains(q_lower)
        };
        if raw_hit {
            return true;
        }
        if let Some(lbl) = self.labels.get(idx) {
            if !lbl.is_empty() && lbl != &self.items[idx].0 {
                if q_ascii && lbl.is_ascii() {
                    return ascii_contains_ci(lbl.as_bytes(), q_lower.as_bytes());
                }
                return lbl.to_lowercase().contains(q_lower);
            }
        }
        false
    }

    /// 消费选项表生成最终放行集合（移动已有 `String` 所有权，零克隆）。
    pub(super) fn into_allowed_set(self) -> HashSet<String> {
        let default_override = self.default_override;
        let toggled = self.toggled;
        let mut out = HashSet::with_capacity(self.checked_total);
        for (i, (v, base)) in self.items.into_iter().enumerate() {
            let on = default_override.unwrap_or(base) ^ toggled.contains(&i);
            if on {
                out.insert(v);
            }
        }
        out
    }

    fn prepare(
        column: FilterColumn,
        allows: Option<&HashSet<String>>,
        changes: &HashMap<String, bool>,
        bulk_draft: Option<bool>,
        cancel: &AtomicBool,
    ) -> Option<Self> {
        let (values, numeric) = column.choices_while(|| !cancel.load(Ordering::Relaxed))?;
        let mut list = Self {
            numeric,
            items: Vec::with_capacity(values.len()),
            folded_spans: Vec::with_capacity(values.len()),
            all_shown: true,
            bulk_draft,
            ..Self::default()
        };
        for (i, value) in values.into_iter().enumerate() {
            if i % 256 == 0 && cancel.load(Ordering::Relaxed) {
                return None;
            }
            let checked = changes
                .get(&value)
                .copied()
                .or(bulk_draft)
                .unwrap_or_else(|| allows.is_none_or(|a| a.contains(&value)));
            list.checked += usize::from(checked);
            if value.is_ascii() {
                list.folded_spans.push((u32::MAX, 0));
            } else {
                let start = list.folded_arena.len() as u32;
                for ch in value.chars().flat_map(|c| c.to_lowercase()) {
                    list.folded_arena.push(ch);
                }
                let len = list.folded_arena.len() as u32 - start;
                list.folded_spans.push((start, len));
            }
            list.items.push((value, checked));
        }
        list.checked_total = list.checked;
        Some(list)
    }

    fn search(&mut self, text: &str) {
        if self.query == text {
            return;
        }
        if text.is_empty() {
            self.query.clear();
            self.shown.clear();
            self.all_shown = true;
            self.checked = self.checked_total;
            return;
        }
        let q = text.to_lowercase();
        let q_ascii = q.is_ascii();
        let prev_lower = self.query.to_lowercase();
        let narrow = !self.all_shown && !prev_lower.is_empty() && q.starts_with(&prev_lower);
        self.query.clear();
        self.query.push_str(text);
        if narrow {
            let mut checked = 0usize;
            let mut write = 0usize;
            for read in 0..self.shown.len() {
                let idx = self.shown[read];
                if self.matches_folded(idx, &q, q_ascii) {
                    self.shown[write] = idx;
                    write += 1;
                    checked += usize::from(self.item_checked(idx));
                }
            }
            self.shown.truncate(write);
            self.checked = checked;
            return;
        }
        self.all_shown = false;
        self.shown.clear();
        self.checked = 0;
        for i in 0..self.items.len() {
            if self.matches_folded(i, &q, q_ascii) {
                self.shown.push(i);
                self.checked += usize::from(self.item_checked(i));
            }
        }
    }

    pub(super) fn draw(
        &mut self,
        ui: &mut egui::Ui,
        height: f32,
        search: &str,
        changes: &mut Arc<HashMap<String, bool>>,
    ) {
        self.search(search);
        let visible_len = self.shown_len();
        egui::ScrollArea::vertical()
            .id_salt("filter_list")
            .max_height(height)
            .auto_shrink([false, false])
            .show_rows(ui, 20.0, 1 + visible_len, |ui, rows| {
                for row in rows {
                    if row == 0 {
                        let all = visible_len > 0 && self.checked == visible_len;
                        if checkbox(ui, "(全选)", all, self.checked > 0 && !all) {
                            let target = !all;
                            if self.all_shown {
                                // O(1) 全选/取消全选：零循环、零 String 克隆！
                                self.default_override = Some(target);
                                self.toggled.clear();
                                self.bulk_draft = Some(target);
                                Arc::make_mut(changes).clear();
                                self.checked = if target { visible_len } else { 0 };
                                self.checked_total = self.checked;
                            } else {
                                for pos in 0..visible_len {
                                    let i = self.shown[pos];
                                    if self.item_checked(i) != target {
                                        if !self.toggled.insert(i) {
                                            self.toggled.remove(&i);
                                        }
                                    }
                                    if visible_len <= 4_096 {
                                        Arc::make_mut(changes)
                                            .insert(self.items[i].0.clone(), target);
                                    }
                                }
                                self.checked_total -= self.checked;
                                self.checked = if target { visible_len } else { 0 };
                                self.checked_total += self.checked;
                            }
                        }
                    } else {
                        let i = self.shown_index(row - 1);
                        let on = self.item_checked(i);
                        let value = &self.items[i].0;
                        let disp = self
                            .labels
                            .get(i)
                            .filter(|s| !s.is_empty())
                            .map(|s| s.as_str())
                            .unwrap_or(value.as_str());
                        let label = if disp.is_empty() {
                            "（空白）"
                        } else {
                            disp
                        };
                        if checkbox(ui, label, on, false) {
                            let next = !on;
                            if !self.toggled.insert(i) {
                                self.toggled.remove(&i);
                            }
                            if self.default_override.is_none() {
                                self.toggled.remove(&i);
                                self.items[i].1 = next;
                            }
                            Arc::make_mut(changes).insert(self.items[i].0.clone(), next);
                            if next {
                                self.checked += 1;
                                self.checked_total += 1;
                            } else {
                                self.checked -= 1;
                                self.checked_total -= 1;
                            }
                        }
                    }
                }
            });
    }
}

fn checkbox(ui: &mut egui::Ui, label: &str, checked: bool, mixed: bool) -> bool {
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 20.0), egui::Sense::click());
    let pal = theme::palette();
    resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, checked, label));
    if mixed {
        let _ = ui.ctx().accesskit_node_builder(resp.id, |node| node.set_toggled(egui::accesskit::Toggled::Mixed));
    }
    if mixed {
        let _ = ui.ctx().accesskit_node_builder(resp.id, |node| node.set_toggled(egui::accesskit::Toggled::Mixed));
    }
    let p = ui.painter();
    if resp.hovered() || resp.is_pointer_button_down_on() {
        let bg = if resp.is_pointer_button_down_on() { pal.pressed } else { pal.hover };
        p.rect_filled(rect.shrink2(egui::vec2(2.0, 0.0)), 3.0, bg);
    }
    let boxr = egui::Rect::from_min_size(
        egui::pos2(rect.left() + 6.0, rect.center().y - 6.0),
        egui::vec2(12.0, 12.0),
    );
    let selected = checked || mixed;
    let ink = if pal.theme.is_dark() { egui::Color32::BLACK } else { egui::Color32::WHITE };
    p.rect_filled(boxr, 2.0, if selected { pal.accent } else { pal.card_bg });
    p.rect_stroke(
        boxr,
        2.0,
        egui::Stroke::new(1.0, if selected { pal.accent } else { pal.text_muted }),
        egui::StrokeKind::Inside,
    );
    if mixed {
        p.rect_filled(egui::Rect::from_center_size(boxr.center(), egui::vec2(7.0, 1.5)), 0.0, ink);
    } else if checked {
        let stroke = egui::Stroke::new(1.6, ink);
        let mid = egui::pos2(boxr.left() + 5.0, boxr.bottom() - 3.0);
        p.line_segment(
            [egui::pos2(boxr.left() + 2.5, boxr.center().y), mid],
            stroke,
        );
        p.line_segment(
            [mid, egui::pos2(boxr.right() - 2.0, boxr.top() + 3.0)],
            stroke,
        );
    }
    p.text(
        egui::pos2(boxr.right() + 6.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(12.0),
        pal.text,
    );
    ui_kit::focus_ring(ui, &resp, rect.shrink(1.0), 3.0);
    if resp.gained_focus() {
        resp.scroll_to_me(None);
    }
    resp.clicked()
}

impl FilterPopup {
    pub(super) fn open(sheet: &Sheet, doc_gen: u64, col: u32, anchor: egui::Pos2) -> Option<Self> {
        Self::open_with_ctx(sheet, doc_gen, col, anchor, None)
    }

    pub(super) fn open_with_styles(
        sheet: &Sheet,
        styles: &sheetcore::style::StyleTable,
        sys: sheetcore::model::DateSystem,
        doc_gen: u64,
        col: u32,
        anchor: egui::Pos2,
        wake: Option<egui::Context>,
    ) -> Option<Self> {
        let filter = sheet.filter.as_ref()?;
        let range = filter.range;
        let column = FilterColumn::new(sheet, range, col);
        let allows = filter.allows.get(&col).cloned().map(Arc::new);
        let mut popup = Self {
            col,
            list: Choices {
                wake: wake.clone(),
                ..Choices::default()
            },
            loading: None,
            source: (sheet.identity, sheet.cell_revision(), doc_gen, range),
            column_snapshot: column,
            initial_allows: allows.clone(),
            changed_choices: Arc::default(),
            cancel_load: Arc::default(),
            search: String::new(),
            anchor,
            crit_dlg: None,
            crit_and: true,
            crit_op2: None,
            crit_val2: String::new(),
            top_dlg: None,
            ops_open: false,
            ops_anchor: egui::Pos2::ZERO,
            focus_search: true,
            meta: None,
            date_selected: std::collections::HashSet::new(),
            date_mode: false,
            color_open: false,
            use_cascading: range.2.saturating_sub(range.0) <= 100_000,
        };
        if popup.use_cascading {
            let (pairs, numeric) = sheetcore::sort_filter::cascading_column_choices(
                sheet,
                filter,
                col,
                styles,
                sys,
            );
            let mut list = Choices {
                numeric,
                items: Vec::with_capacity(pairs.len()),
                labels: Vec::with_capacity(pairs.len()),
                folded_spans: Vec::with_capacity(pairs.len()),
                all_shown: true,
                wake,
                ..Choices::default()
            };
            for (raw, lbl) in pairs {
                let checked = allows.as_deref().is_none_or(|a| a.contains(&raw));
                list.checked += usize::from(checked);
                if raw.is_ascii() {
                    list.folded_spans.push((u32::MAX, 0));
                } else {
                    let start = list.folded_arena.len() as u32;
                    for ch in raw.chars().flat_map(|c| c.to_lowercase()) {
                        list.folded_arena.push(ch);
                    }
                    let len = list.folded_arena.len() as u32 - start;
                    list.folded_spans.push((start, len));
                }
                list.labels.push(lbl);
                list.items.push((raw, checked));
            }
            list.checked_total = list.checked;
            popup.list = list;
        } else {
            popup.prepare_choices();
        }
        Some(popup)
    }

    pub(super) fn open_with_ctx(
        sheet: &Sheet,
        doc_gen: u64,
        col: u32,
        anchor: egui::Pos2,
        wake: Option<egui::Context>,
    ) -> Option<Self> {
        let filter = sheet.filter.as_ref()?;
        let range = filter.range;
        let column = FilterColumn::new(sheet, range, col);
        let mut popup = Self {
            col,
            list: Choices {
                wake,
                ..Choices::default()
            },
            loading: None,
            source: (sheet.identity, sheet.cell_revision(), doc_gen, range),
            column_snapshot: column,
            initial_allows: filter.allows.get(&col).cloned().map(Arc::new),
            changed_choices: Arc::default(),
            cancel_load: Arc::default(),
            search: String::new(),
            anchor,
            crit_dlg: None,
            crit_and: true,
            crit_op2: None,
            crit_val2: String::new(),
            top_dlg: None,
            ops_open: false,
            ops_anchor: egui::Pos2::ZERO,
            focus_search: true,
            meta: None,
            date_selected: std::collections::HashSet::new(),
            date_mode: false,
            color_open: false,
            use_cascading: false,
        };
        popup.prepare_choices();
        Some(popup)
    }

    pub(super) fn refresh_source(&mut self, sheet: &Sheet, doc_gen: u64) -> bool {
        let Some(filter) = &sheet.filter else {
            return false;
        };
        let (identity, revision, generation, range) = self.source;
        if identity != sheet.identity || range != filter.range {
            return false;
        }
        if revision == sheet.cell_revision() && generation == doc_gen {
            return true;
        }
        self.source.1 = sheet.cell_revision();
        self.source.2 = doc_gen;
        let updated = FilterColumn::new(sheet, range, self.col);
        let allowed = filter.allows.get(&self.col);
        if self.column_snapshot.same_pages(&updated) && self.initial_allows.as_deref() == allowed {
            return true;
        }
        self.column_snapshot = updated;
        self.initial_allows = allowed.cloned().map(Arc::new);
        self.prepare_choices();
        true
    }

    fn prepare_choices(&mut self) {
        self.cancel_load.store(true, Ordering::Relaxed);
        self.cancel_load = Arc::default();
        let column = self.column_snapshot.clone();
        let allows = self.initial_allows.clone();
        let changes = self.changed_choices.clone();
        let bulk_draft = self.list.bulk_draft;
        let wake = self.list.wake.clone();
        let cancel = self.cancel_load.clone();
        let prepare = move || Choices::prepare(column, allows.as_deref(), &changes, bulk_draft, &cancel);
        let range = self.source.3;
        if range.2.saturating_sub(range.0) <= 2048 {
            if let Some(mut list) = prepare() {
                list.wake = wake;
                self.list = list;
            }
            self.loading = None;
        } else {
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                if let Some(mut list) = prepare() {
                    list.wake = wake.clone();
                    if tx.send(list).is_ok() {
                        if let Some(ctx) = wake {
                            ctx.request_repaint();
                        }
                    }
                }
            });
            self.loading = Some(rx);
        }
    }
}

impl Drop for FilterPopup {
    fn drop(&mut self) {
        self.cancel_load.store(true, Ordering::Relaxed);
    }
}

// ===== 零堆分配 + 多核加速筛选隐藏行求值器（替代百万行逐格 `Value::display()`） =====

#[inline]
fn canonical_f64_bits(n: f64) -> u64 {
    if n == 0.0 {
        0.0f64.to_bits()
    } else {
        n.to_bits()
    }
}

#[inline]
fn format_int_stack(mut n: i64, buf: &mut [u8; 24]) -> &str {
    if n == 0 {
        return "0";
    }
    let neg = n < 0;
    let mut abs = n.unsigned_abs();
    let mut pos = buf.len();
    while abs > 0 {
        pos -= 1;
        buf[pos] = b'0' + (abs % 10) as u8;
        abs /= 10;
    }
    if neg {
        pos -= 1;
        buf[pos] = b'-';
    }
    std::str::from_utf8(&buf[pos..]).unwrap_or("")
}

pub(crate) struct CompiledAllows<'a> {
    col: u32,
    set: &'a HashSet<String>,
    allows_empty: bool,
    allows_true: bool,
    allows_false: bool,
    num_bits: Option<HashSet<u64>>,
}

impl<'a> CompiledAllows<'a> {
    pub(crate) fn new(col: u32, set: &'a HashSet<String>) -> Self {
        let allows_empty = set.contains("");
        let allows_true = set.contains("TRUE");
        let allows_false = set.contains("FALSE");
        let num_bits = (set.len() <= 131_072).then(|| {
            let mut bits = HashSet::new();
            for s in set {
                if let Ok(n) = s.parse::<f64>() {
                    if sheetcore::Value::Number(n).display() == *s {
                        bits.insert(canonical_f64_bits(n));
                    }
                }
            }
            bits
        });
        Self {
            col,
            set,
            allows_empty,
            allows_true,
            allows_false,
            num_bits,
        }
    }

    #[inline]
    pub(crate) fn matches_cell(&self, cell: Option<&sheetcore::Cell>) -> bool {
        let Some(c) = cell else {
            return self.allows_empty;
        };
        match &c.value {
            sheetcore::Value::Empty => self.allows_empty,
            sheetcore::Value::Text(s) => self.set.contains(s.as_ref()),
            sheetcore::Value::Bool(b) => {
                if *b {
                    self.allows_true
                } else {
                    self.allows_false
                }
            }
            sheetcore::Value::Error(e) => self.set.contains(e.text()),
            sheetcore::Value::Number(n) => {
                if let Some(bits) = &self.num_bits {
                    return bits.contains(&canonical_f64_bits(*n));
                }
                if n.is_finite() && n.fract() == 0.0 && n.abs() < 1e15 {
                    let mut buf = [0u8; 24];
                    let s = format_int_stack(*n as i64, &mut buf);
                    return self.set.contains(s);
                }
                let s = sheetcore::Value::Number(*n).display();
                self.set.contains(s.as_str())
            }
        }
    }
}

pub(crate) struct CompiledCriterion<'a> {
    col: u32,
    raw: &'a Criterion,
    v1_num: Option<f64>,
    v2_num: Option<f64>,
    v1_lower: String,
    v2_lower: String,
    empty_passes: bool,
}

impl<'a> CompiledCriterion<'a> {
    pub(crate) fn new(col: u32, raw: &'a Criterion) -> Self {
        let v1_num = raw.val.trim().parse::<f64>().ok();
        let v2_num = raw
            .val2
            .as_deref()
            .and_then(|v| v.trim().parse::<f64>().ok());
        let v1_lower = raw.val.to_lowercase();
        let v2_lower = raw.val2.as_deref().unwrap_or(&raw.val).to_lowercase();
        let empty_passes = raw.matches("");
        Self {
            col,
            raw,
            v1_num,
            v2_num,
            v1_lower,
            v2_lower,
            empty_passes,
        }
    }

    #[inline]
    fn compare_num(&self, a: f64, b: f64) -> bool {
        match self.raw.op {
            CmpOp::Eq => a == b,
            CmpOp::Ne => a != b,
            CmpOp::Gt => a > b,
            CmpOp::Ge => a >= b,
            CmpOp::Lt => a < b,
            CmpOp::Le => a <= b,
            CmpOp::Between => {
                let hi = self.v2_num.unwrap_or(b);
                a >= b.min(hi) && a <= b.max(hi)
            }
            _ => true,
        }
    }

    #[inline]
    fn matches_str(&self, text: &str) -> bool {
        match self.raw.op {
            CmpOp::Contains => {
                if text.is_ascii() && self.v1_lower.is_ascii() {
                    ascii_contains_ci(text.as_bytes(), self.v1_lower.as_bytes())
                } else {
                    text.to_lowercase().contains(&self.v1_lower)
                }
            }
            CmpOp::NotContains => {
                if text.is_ascii() && self.v1_lower.is_ascii() {
                    !ascii_contains_ci(text.as_bytes(), self.v1_lower.as_bytes())
                } else {
                    !text.to_lowercase().contains(&self.v1_lower)
                }
            }
            CmpOp::Begins => {
                if text.is_ascii() && self.v1_lower.is_ascii() {
                    text.len() >= self.v1_lower.len()
                        && text.as_bytes()[..self.v1_lower.len()]
                            .eq_ignore_ascii_case(self.v1_lower.as_bytes())
                } else {
                    text.to_lowercase().starts_with(&self.v1_lower)
                }
            }
            CmpOp::Ends => {
                if text.is_ascii() && self.v1_lower.is_ascii() {
                    text.len() >= self.v1_lower.len()
                        && text.as_bytes()[text.len() - self.v1_lower.len()..]
                            .eq_ignore_ascii_case(self.v1_lower.as_bytes())
                } else {
                    text.to_lowercase().ends_with(&self.v1_lower)
                }
            }
            _ => {
                if let Some(b) = self.v1_num {
                    if let Ok(a) = text.trim().parse::<f64>() {
                        return self.compare_num(a, b);
                    }
                }
                let t = text.to_lowercase();
                let v = &self.v1_lower;
                match self.raw.op {
                    CmpOp::Eq => t == *v,
                    CmpOp::Ne => t != *v,
                    CmpOp::Gt => t > *v,
                    CmpOp::Ge => t >= *v,
                    CmpOp::Lt => t < *v,
                    CmpOp::Le => t <= *v,
                    CmpOp::Between => {
                        let hi = &self.v2_lower;
                        let (lo, hi) = if v <= hi { (v, hi) } else { (hi, v) };
                        &t >= lo && &t <= hi
                    }
                    _ => true,
                }
            }
        }
    }

    #[inline]
    pub(crate) fn matches_cell(&self, cell: Option<&sheetcore::Cell>) -> bool {
        let Some(c) = cell else {
            return self.empty_passes;
        };
        match &c.value {
            sheetcore::Value::Empty => self.empty_passes,
            sheetcore::Value::Number(a) => {
                if !self.raw.op.is_text() {
                    if let Some(b) = self.v1_num {
                        return self.compare_num(*a, b);
                    }
                }
                if a.is_finite() && a.fract() == 0.0 && a.abs() < 1e15 {
                    let mut buf = [0u8; 24];
                    return self.matches_str(format_int_stack(*a as i64, &mut buf));
                }
                let s = sheetcore::Value::Number(*a).display();
                self.matches_str(&s)
            }
            sheetcore::Value::Text(s) => self.matches_str(s),
            sheetcore::Value::Bool(b) => self.matches_str(if *b { "TRUE" } else { "FALSE" }),
            sheetcore::Value::Error(e) => self.matches_str(e.text()),
        }
    }
}

pub(crate) struct CompiledFilter<'a> {
    pub(crate) range: (u32, u32, u32, u32),
    allows: Vec<CompiledAllows<'a>>,
    criteria: Vec<CompiledCriterion<'a>>,
    blank_row_hidden: bool,
}

impl<'a> CompiledFilter<'a> {
    pub(crate) fn new(f: &'a Filter) -> Self {
        let (_, fc1, _, fc2) = f.range;
        let allows: Vec<_> = f
            .allows
            .iter()
            .filter(|&(&c, _)| c >= fc1 && c <= fc2)
            .map(|(&c, s)| CompiledAllows::new(c, s))
            .collect();
        let criteria: Vec<_> = f
            .criteria
            .iter()
            .filter(|&(&c, _)| c >= fc1 && c <= fc2)
            .map(|(&c, cr)| CompiledCriterion::new(c, cr))
            .collect();
        let blank_row_hidden = allows.iter().any(|a| !a.allows_empty)
            || criteria.iter().any(|c| !c.empty_passes);
        Self {
            range: f.range,
            allows,
            criteria,
            blank_row_hidden,
        }
    }

    #[inline]
    pub(crate) fn is_active(&self) -> bool {
        !self.allows.is_empty() || !self.criteria.is_empty()
    }

    #[inline]
    pub(crate) fn row_hidden(&self, sheet: &Sheet, r: u32) -> bool {
        if r <= self.range.0 || r > self.range.2 {
            return false;
        }
        for a in &self.allows {
            let cell = sheet.get(r, a.col);
            if !a.matches_cell(cell.as_deref()) {
                return true;
            }
        }
        for c in &self.criteria {
            let cell = sheet.get(r, c.col);
            if !c.matches_cell(cell.as_deref()) {
                return true;
            }
        }
        false
    }
}

/// 高性能筛选隐藏行求值：零 `Value::display()` 堆分配 + 空行带短路 + 大范围多核并行分块，直接输出合并闭区间 `HiddenIndices`（零百万级 `HashSet` 分配）。
pub(crate) fn compute_filter_hidden_fast(
    sheet: &Sheet,
    f: &Filter,
) -> sheetcore::layout::HiddenIndices {
    let compiled = CompiledFilter::new(f);
    if !compiled.is_active() || f.range.2 <= f.range.0 {
        return sheetcore::layout::HiddenIndices::default();
    }
    let start_r = f.range.0 + 1;
    let mut end_r = f.range.2;
    let used_max_r = sheet.used_bounds().map(|(_, _, r2, _)| r2).unwrap_or(f.range.0);
    if end_r > used_max_r.saturating_add(64) {
        end_r = used_max_r.saturating_add(1).min(end_r);
    }
    if start_r > end_r {
        return sheetcore::layout::HiddenIndices::default();
    }
    let scan_end = end_r.min(used_max_r);

    #[inline]
    fn push_run(runs: &mut Vec<(u32, u32)>, r: u32) {
        if let Some(last) = runs.last_mut()
            && r == last.1 + 1
        {
            last.1 = r;
        } else {
            runs.push((r, r));
        }
    }

    let mut runs: Vec<(u32, u32)> = if start_r > scan_end {
        Vec::new()
    } else if scan_end - start_r < 32_768 {
        let mut v = Vec::new();
        for r in start_r..=scan_end {
            if compiled.row_hidden(sheet, r) {
                push_run(&mut v, r);
            }
        }
        v
    } else {
        let workers = std::thread::available_parallelism()
            .map(|n| n.get().clamp(2, 8))
            .unwrap_or(4);
        let total = (scan_end - start_r + 1) as usize;
        let chunk = total.div_ceil(workers);
        let parts: Vec<Vec<(u32, u32)>> = std::thread::scope(|s| {
            let mut handles = Vec::with_capacity(workers);
            for w in 0..workers {
                let lo = start_r + (w * chunk) as u32;
                if lo > scan_end {
                    break;
                }
                let hi = (lo + chunk as u32 - 1).min(scan_end);
                let comp = &compiled;
                handles.push(s.spawn(move || {
                    let mut local = Vec::new();
                    for r in lo..=hi {
                        if comp.row_hidden(sheet, r) {
                            push_run(&mut local, r);
                        }
                    }
                    local
                }));
            }
            handles
                .into_iter()
                .map(|h| h.join().unwrap_or_default())
                .collect()
        });
        let cap: usize = parts.iter().map(|p| p.len()).sum();
        let mut merged: Vec<(u32, u32)> = Vec::with_capacity(cap + 1);
        for p in parts {
            for (lo, hi) in p {
                if let Some(last) = merged.last_mut()
                    && lo == last.1 + 1
                {
                    last.1 = hi;
                } else {
                    merged.push((lo, hi));
                }
            }
        }
        merged
    };

    let tail_start = scan_end.saturating_add(1).max(start_r);
    if compiled.blank_row_hidden && tail_start <= end_r {
        if let Some(last) = runs.last_mut()
            && tail_start == last.1 + 1
        {
            last.1 = end_r;
        } else {
            runs.push((tail_start, end_r));
        }
    }
    sheetcore::layout::HiddenIndices::from_intervals(runs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn popup_keeps_checked_drafts_on_source_changes_and_cancels_superseded_reads() {
        let mut sheet = Sheet::new("Data");
        let text = |s: &str| Cell {
            value: Value::Text(s.into()),
            formula: None,
        };
        sheet.set(1, 0, text("A"));
        sheet.filter = Some(sheetcore::sort_filter::Filter::new(0, 0, 3, 0));
        let mut popup = FilterPopup::open(&sheet, 0, 0, egui::Pos2::ZERO).unwrap();
        Arc::make_mut(&mut popup.changed_choices).insert("A".into(), false);
        let index = popup.list.items.iter().position(|(v, _)| v == "A").unwrap();
        popup.list.items[index].1 = false;
        sheet.set(1, 1, text("unrelated"));
        let old = popup.cancel_load.clone();
        assert!(popup.refresh_source(&sheet, 1));
        assert!(Arc::ptr_eq(&old, &popup.cancel_load));
        sheet.set(2, 0, text("B"));
        assert!(popup.refresh_source(&sheet, 2));
        assert!(old.load(Ordering::Relaxed));
        assert!(popup.list.items.contains(&("A".into(), false)));
        assert!(popup.list.items.contains(&("B".into(), true)));
        sheet.filter.as_mut().unwrap().range.2 = 100_000;
        assert!(!popup.refresh_source(&sheet, 3));
        let popup = FilterPopup::open(&sheet, 3, 0, egui::Pos2::ZERO).unwrap();
        let flag = popup.cancel_load.clone();
        assert!(popup.loading.is_some());
        drop(popup);
        assert!(flag.load(Ordering::Relaxed));
    }

    #[test]
    fn filter_snapshot_preserves_blank_numeric_order_and_checked_search_results() {
        let mut sheet = Sheet::new("Filter");
        for (r, v) in [
            (2, "10"),
            (3, "2"),
            (270, "2"),
            (300, "apple"),
            (301, "Apricot"),
        ] {
            sheet.set(
                r,
                1,
                Cell {
                    value: Value::Text(v.into()),
                    formula: None,
                },
            );
        }
        let column = FilterColumn::new(&sheet, (0, 0, 1_048_575, 1), 1);
        sheet.remove(301, 1);
        let mut choices = Choices::prepare(
            column,
            Some(&["2".to_string(), "Apricot".into()].into_iter().collect()),
            &HashMap::new(),
            None,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(!choices.numeric);
        assert_eq!(
            choices.items,
            [
                ("", false),
                ("2", true),
                ("10", false),
                ("Apricot", true),
                ("apple", false)
            ]
            .map(|(s, on)| (s.to_string(), on))
        );
        choices.search("AP");
        assert_eq!(choices.shown, [3, 4]);
        assert_eq!(choices.checked, 1);
        choices.search("apr");
        assert_eq!(choices.shown, [3]);
        assert_eq!(choices.checked, 1);
        choices.search("none");
        assert!(choices.shown.is_empty());
        choices.search("");
        assert_eq!(choices.checked, 2);
        let numeric = FilterColumn::new(&sheet, (0, 1, 299, 1), 1);
        assert!(
            Choices::prepare(numeric, None, &HashMap::new(), None, &AtomicBool::new(false))
                .unwrap()
                .numeric
        );
    }

    #[test]
    fn select_all_uses_inversion_set_and_fast_filter_matches_reference() {
        let mut sheet = Sheet::new("MillionFast");
        for r in 1..=200u32 {
            sheet.set(
                r,
                0,
                Cell {
                    value: if r % 3 == 0 {
                        Value::Number(r as f64)
                    } else {
                        Value::Text(format!("ID-{r}").into())
                    },
                    formula: None,
                },
            );
        }
        let mut f = Filter::new(0, 0, 500, 0);
        f.allows.insert(
            0,
            ["3".to_string(), "ID-4".to_string(), "".to_string()]
                .into_iter()
                .collect(),
        );
        assert_eq!(compute_filter_hidden_fast(&sheet, &f), f.hidden_rows(&sheet));
        f.allows.clear();
        f.criteria.insert(0, Criterion::between("10", "50"));
        assert_eq!(compute_filter_hidden_fast(&sheet, &f), f.hidden_rows(&sheet));

        // Verify O(1) bulk override + sparse toggle produces exact allowed set
        let col = FilterColumn::new(&sheet, (0, 0, 200, 0), 0);
        let mut choices =
            Choices::prepare(col, None, &HashMap::new(), None, &AtomicBool::new(false)).unwrap();
        choices.default_override = Some(false);
        choices.toggled.insert(1);
        choices.checked_total = 1;
        let expected = choices.items[1].0.clone();
        let allowed = choices.into_allowed_set();
        assert_eq!(allowed, [expected].into_iter().collect());
    }
}
