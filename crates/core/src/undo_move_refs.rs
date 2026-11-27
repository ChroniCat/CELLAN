//! Individual workbook references changed by a cut, kept with the cell action.
use crate::model::Workbook;

#[derive(Default)]
pub struct MoveReferences {
    names: Vec<(String, Option<usize>, String, String)>,
    charts: Vec<(u64, u64, usize, usize, String, String)>,
}

impl MoveReferences {
    pub fn plan(wb: &Workbook, rewrite: impl Fn(&str, &str) -> String) -> Self {
        let mut edits = Self::default();
        for (name, scope, text) in &wb.names {
            let own = scope
                .and_then(|i| wb.sheets.get(i))
                .map_or("", |s| s.name.as_str());
            let new = rewrite(text, own);
            if new != *text {
                edits.names.push((name.clone(), *scope, text.clone(), new));
            }
        }
        for sh in &wb.sheets {
            for chart in sh.charts.iter() {
                for (si, series) in chart.series.iter().enumerate() {
                    for (field, text) in [
                        &series.name_ref,
                        &series.cat_ref,
                        &series.val_ref,
                        &series.size_ref,
                    ]
                    .into_iter()
                    .enumerate()
                    {
                        if text.is_empty() {
                            continue;
                        }
                        let new = rewrite(text, &sh.name);
                        if new != *text {
                            edits.charts.push((
                                sh.identity,
                                chart.uid.raw(),
                                si,
                                field,
                                text.clone(),
                                new,
                            ));
                        }
                    }
                }
            }
        }
        edits
    }

    pub fn names_changed(&self) -> bool {
        !self.names.is_empty()
    }
    pub fn is_empty(&self) -> bool {
        self.names.is_empty() && self.charts.is_empty()
    }
    pub(super) fn bytes(&self) -> usize {
        self.names.capacity() * std::mem::size_of::<(String, Option<usize>, String, String)>()
            + self
                .names
                .iter()
                .map(|(n, _, a, b)| n.len() + a.len() + b.len())
                .sum::<usize>()
            + self.charts.capacity()
                * std::mem::size_of::<(u64, u64, usize, usize, String, String)>()
            + self
                .charts
                .iter()
                .map(|(_, _, _, _, a, b)| a.len() + b.len())
                .sum::<usize>()
    }

    fn apply_charts(&mut self, wb: &mut Workbook, redo: bool) {
        self.charts
            .retain(|(sheet, chart, si, field, before, after)| {
                let Some(sh) = wb.sheets.iter_mut().find(|sh| sh.identity == *sheet) else {
                    return false;
                };
                let Some(ch) = sh.charts.iter_mut().find(|ch| ch.uid.raw() == *chart) else {
                    return false;
                };
                let Some(series) = ch.series.get_mut(*si) else {
                    return false;
                };
                let text = match field {
                    0 => &mut series.name_ref,
                    1 => &mut series.cat_ref,
                    2 => &mut series.val_ref,
                    _ => &mut series.size_ref,
                };
                let (expected, new) = if redo {
                    (before, after)
                } else {
                    (after, before)
                };
                if text != expected {
                    return false;
                }
                text.clone_from(new);
                ch.ref_fixes.push((expected.clone(), new.clone()));
                sh.charts_dirty = true;
                true
            });
    }

    pub(super) fn matches(&self, wb: &Workbook, redo: bool) -> bool {
        self.names.iter().all(|(key, scope, before, after)| {
            wb.names.iter().any(|(n, s, text)| {
                n == key && s == scope && text == if redo { before } else { after }
            })
        }) && self
            .charts
            .iter()
            .all(|(sheet, chart, idx, field, before, after)| {
                let series = wb
                    .sheets
                    .iter()
                    .find(|sh| sh.identity == *sheet)
                    .and_then(|sh| sh.charts.iter().find(|ch| ch.uid.raw() == *chart))
                    .and_then(|ch| ch.series.get(*idx));
                series.is_some_and(|s| {
                    let text = match field {
                        0 => &s.name_ref,
                        1 => &s.cat_ref,
                        2 => &s.val_ref,
                        _ => &s.size_ref,
                    };
                    text == if redo { before } else { after }
                })
            })
    }

    pub fn replay(&mut self, wb: &mut Workbook, redo: bool) -> bool {
        self.names.retain(|(key, scope, before, after)| {
            let Some((_, _, text)) = wb
                .names
                .iter_mut()
                .find(|(name, local, _)| name == key && local == scope)
            else {
                return false;
            };
            let (expected, new) = if redo {
                (before, after)
            } else {
                (after, before)
            };
            if text != expected {
                return false;
            }
            text.clone_from(new);
            wb.names_dirty = true;
            true
        });
        self.apply_charts(wb, redo);
        self.names_changed()
    }
}
