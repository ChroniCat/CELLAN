use super::*;
use sheetcore::{model::Sheet, sort_filter::FilterColumn};
use std::collections::{HashMap, HashSet};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

#[derive(Default)]
pub(super) struct Choices {
    pub(super) items: Vec<(String, bool)>,
    folded: Vec<String>,
    shown: Vec<usize>,
    checked: usize,
    pub(super) checked_total: usize,
    query: String,
    pub(super) numeric: bool,
}

impl Choices {
    fn prepare(
        column: FilterColumn,
        allows: Option<&HashSet<String>>,
        changes: &HashMap<String, bool>,
        cancel: &AtomicBool,
    ) -> Option<Self> {
        let (values, numeric) = column.choices_while(|| !cancel.load(Ordering::Relaxed))?;
        let mut list = Self {
            numeric,
            ..Self::default()
        };
        for (i, value) in values.into_iter().enumerate() {
            if i % 256 == 0 && cancel.load(Ordering::Relaxed) {
                return None;
            }
            let checked = changes
                .get(&value)
                .copied()
                .unwrap_or_else(|| allows.is_none_or(|a| a.contains(&value)));
            list.shown.push(list.items.len());
            list.checked += usize::from(checked);
            list.folded.push(value.to_lowercase());
            list.items.push((value, checked));
        }
        list.checked_total = list.checked;
        Some(list)
    }

    fn search(&mut self, text: &str) {
        if self.query == text {
            return;
        }
        self.query.clear();
        self.query.push_str(text);
        let q = text.to_lowercase();
        self.shown.clear();
        self.checked = 0;
        for (i, value) in self.folded.iter().enumerate() {
            if value.contains(&q) {
                self.shown.push(i);
                self.checked += usize::from(self.items[i].1);
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
        egui::ScrollArea::vertical()
            .id_salt("filter_list")
            .max_height(height)
            .auto_shrink([false, false])
            .show_rows(ui, 20.0, 1 + self.shown.len(), |ui, rows| {
                for row in rows {
                    if row == 0 {
                        let all = !self.shown.is_empty() && self.checked == self.shown.len();
                        if checkbox(ui, "(全选)", all, self.checked > 0 && !all) {
                            let changes = Arc::make_mut(changes);
                            for &i in &self.shown {
                                self.items[i].1 = !all;
                                changes.insert(self.items[i].0.clone(), !all);
                            }
                            self.checked_total -= self.checked;
                            self.checked = if all { 0 } else { self.shown.len() };
                            self.checked_total += self.checked;
                        }
                    } else {
                        let i = self.shown[row - 1];
                        let (value, on) = &mut self.items[i];
                        let label = if value.is_empty() {
                            "（空白）"
                        } else {
                            value.as_str()
                        };
                        if checkbox(ui, label, *on, false) {
                            *on = !*on;
                            Arc::make_mut(changes).insert(value.clone(), *on);
                            if *on {
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
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(rect, 0.0, egui::Color32::from_rgb(229, 241, 251));
    }
    let boxr = egui::Rect::from_min_size(
        egui::pos2(rect.left() + 6.0, rect.center().y - 6.0),
        egui::vec2(12.0, 12.0),
    );
    p.rect_filled(boxr, 0.0, egui::Color32::WHITE);
    p.rect_stroke(
        boxr,
        0.0,
        egui::Stroke::new(1.0, egui::Color32::from_rgb(120, 120, 120)),
        egui::StrokeKind::Inside,
    );
    if mixed {
        p.rect_filled(boxr.shrink(3.0), 0.0, ui_kit::TEXT);
    } else if checked {
        let stroke = egui::Stroke::new(1.6, ui_kit::TEXT);
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
        ui_kit::TEXT,
    );
    resp.clicked()
}

impl FilterPopup {
    pub(super) fn open(sheet: &Sheet, doc_gen: u64, col: u32, anchor: egui::Pos2) -> Option<Self> {
        let filter = sheet.filter.as_ref()?;
        let range = filter.range;
        let column = FilterColumn::new(sheet, range, col);
        let mut popup = Self {
            col,
            list: Choices::default(),
            loading: None,
            source: (sheet.identity, sheet.cell_revision(), doc_gen, range),
            column_snapshot: column,
            initial_allows: filter.allows.get(&col).cloned().map(Arc::new),
            changed_choices: Arc::default(),
            cancel_load: Arc::default(),
            search: String::new(),
            anchor,
            crit_dlg: None,
            ops_open: false,
            ops_anchor: egui::Pos2::ZERO,
            focus_search: true,
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
        let cancel = self.cancel_load.clone();
        let prepare = move || Choices::prepare(column, allows.as_deref(), &changes, &cancel);
        let range = self.source.3;
        if range.2.saturating_sub(range.0) <= 2048 {
            self.list = prepare().unwrap();
            self.loading = None;
        } else {
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                if let Some(list) = prepare() {
                    let _ = tx.send(list);
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
        choices.search("none");
        assert!(choices.shown.is_empty());
        choices.search("");
        assert_eq!(choices.checked, 2);
        let numeric = FilterColumn::new(&sheet, (0, 1, 299, 1), 1);
        assert!(
            Choices::prepare(numeric, None, &HashMap::new(), &AtomicBool::new(false))
                .unwrap()
                .numeric
        );
    }
}
