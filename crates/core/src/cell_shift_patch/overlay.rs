use super::*;
use crate::cf::CfSheet;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct SheetOverlay {
    name: String,
    styles: OverlayStyles,
    notes: Vec<((u32, u32), Note)>,
    threads: Vec<((u32, u32), ThreadedComment)>,
    rich: Vec<((u32, u32), Vec<RichRun>)>,
    links: Vec<((u32, u32), Hyperlink)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    ignored_errors: Vec<(u32, u32)>,
    merges: Vec<Rect>,
    cf: CfSheet,
    dv: Vec<DataValidation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    chart_refs: Vec<ChartRefsSnapshot>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    sparklines: Vec<crate::sparkline::Sparkline>,
}

/// Current metadata of the authoritative workbook. Captured at join time, so
/// subsequent ordinary edits or a structural move take precedence over history.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CellShiftOverlay {
    sheets: Vec<SheetOverlay>,
    palette: Vec<(CellStyle, StyleExtras)>,
    sources: Vec<(String, SourceGeometry)>,
    names: Vec<(String, Option<usize>, String)>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct OverlayStyles {
    // Column, inclusive first/last row, portable palette index.
    runs: Vec<(u32, u32, u32, u16)>,
    rows: Vec<(u32, u16)>,
    cols: Vec<(u32, u16)>,
}

impl OverlayStyles {
    fn capture(sh: &Sheet, mut palette: impl FnMut(u16) -> u16) -> Self {
        let mut out = Self::default();
        let mut last = HashMap::<u32, usize>::new();
        sh.styles.visit_column_style_runs(|col, r1, r2, id| {
            let idx = palette(id);
            if let Some((_, _, end, prev)) = last.get(&col).and_then(|&i| out.runs.get_mut(i))
                && *end + 1 == r1
                && *prev == idx
            {
                *end = r2;
            } else {
                last.insert(col, out.runs.len());
                out.runs.push((col, r1, r2, idx));
            }
        });
        for (map, items) in [
            (&sh.row_styles, &mut out.rows),
            (&sh.col_styles, &mut out.cols),
        ] {
            let mut ids: Vec<_> = map.iter().collect();
            ids.sort_unstable_by_key(|(at, _)| **at);
            items.extend(ids.into_iter().map(|(&at, &id)| (at, palette(id))));
        }
        out
    }

    fn valid(&self, palette: usize) -> bool {
        self.runs
            .iter()
            .all(|&(c, r1, r2, i)| r1 <= r2 && valid_point(r2, c) && (i as usize) < palette)
            && self
                .rows
                .iter()
                .all(|&(r, i)| valid_point(r, 0) && (i as usize) < palette)
            && self
                .cols
                .iter()
                .all(|&(c, i)| valid_point(0, c) && (i as usize) < palette)
    }

    fn apply(&self, sh: &mut Sheet, ids: &[u16]) {
        let mut desired = crate::store::CellStyles::default();
        for &(c, r1, r2, i) in &self.runs {
            desired.insert_rect(r1, c, r2, c, ids[i as usize]);
        }
        let rows = self
            .rows
            .iter()
            .map(|&(r, i)| (r, ids[i as usize]))
            .collect::<crate::model::CellMetadata<_, _>>();
        let cols = self
            .cols
            .iter()
            .map(|&(c, i)| (c, ids[i as usize]))
            .collect::<crate::model::CellMetadata<_, _>>();
        let mut changed = false;
        if sh.styles != desired {
            sh.styles.diff_into_journal(&desired, &mut sh.style_edits);
            sh.styles = desired;
            changed = true;
        }
        if sh.row_styles != rows {
            sh.row_styles = rows;
            sh.row_styles_dirty = true;
            changed = true;
        }
        if sh.col_styles != cols {
            sh.col_styles = cols;
            sh.col_styles_dirty = true;
            changed = true;
        }
        if changed {
            sh.styles_dirty = true;
            sh.bump_display_style_revision();
        }
    }
}

fn entries<K: Point, V: Clone>(map: &HashMap<K, V>) -> Vec<((u32, u32), V)> {
    let mut out: Vec<_> = map.iter().map(|(k, v)| (k.coords(), v.clone())).collect();
    out.sort_unstable_by_key(|(k, _)| *k);
    out
}

fn replace_entries<K: Point, V: PartialEq + Clone>(
    map: &mut HashMap<K, V>,
    items: &[((u32, u32), V)],
) -> bool {
    if map.len() == items.len()
        && items
            .iter()
            .all(|((r, c), v)| map.get(&K::point(*r, *c)) == Some(v))
    {
        return false;
    }
    map.clear();
    map.extend(
        items
            .iter()
            .map(|((r, c), v)| (K::point(*r, *c), v.clone())),
    );
    true
}

impl CellShiftOverlay {
    pub fn wire_valid(&self) -> bool {
        let count: usize = self
            .sheets
            .iter()
            .map(|s| {
                s.notes.len()
                    + s.threads.len()
                    + s.rich.len()
                    + s.links.len()
                    + s.ignored_errors.len()
                    + s.merges.len()
                    + s.cf.blocks.len()
                    + s.dv.len()
                    + s.chart_refs.len()
                    + s.sparklines.len()
                    + s.styles.rows.len()
                    + s.styles.cols.len()
                    + s.styles.runs.len()
            })
            .sum();
        self.sheets.len() <= crate::sync::MAX_SHEET_INDEX
            && self.palette.len() <= u16::MAX as usize
            && self
                .sheets
                .iter()
                .flat_map(|s| &s.styles.runs)
                // Applying a uniform full column creates at most one slot
                // per storage page. Bound the work of decoding long runs.
                .map(|&(_, a, b, _)| {
                    u64::from(
                        (b / crate::store::PAGE_ROWS).saturating_sub(a / crate::store::PAGE_ROWS),
                    ) + 1
                })
                .sum::<u64>()
                <= crate::sync::MAX_BATCH_ITEMS as u64
            && count + self.sources.len() + self.names.len() <= crate::sync::MAX_BATCH_ITEMS
            && self.names.iter().all(|(n, scope, text)| {
                valid_defined_name(n, *scope)
                    && super::valid_name_formula(text)
                    && scope.is_none_or(|i| i < self.sheets.len())
            })
            && self.sheets.iter().all(|s| {
                s.name.len() <= crate::sync::MAX_SHORT_TEXT
                    && s.styles.valid(self.palette.len())
                    && s.merges.iter().all(valid_rect)
                    && s.notes
                        .iter()
                        .all(|&((r, c), ref n)| valid_point(r, c) && valid_note(r, c, n))
                    && s.threads
                        .iter()
                        .all(|&((r, c), ref t)| valid_point(r, c) && valid_thread(r, c, t))
                    && s.links
                        .iter()
                        .all(|&((r, c), ref l)| valid_point(r, c) && valid_link(r, c, l))
                    && s.rich.iter().all(|((r, c), _)| valid_point(*r, *c))
                    && s.ignored_errors.iter().all(|&(r, c)| valid_point(r, c))
                    && s.cf.blocks.iter().flat_map(|b| &b.ranges).all(valid_rect)
                    && s.dv.iter().flat_map(|d| &d.ranges).all(valid_rect)
                    && s.chart_refs.iter().all(ChartRefsSnapshot::valid)
                    && s.sparklines.iter().all(valid_sparkline)
            })
            && self
                .sources
                .iter()
                .all(|(n, g)| n.len() <= crate::sync::MAX_SHORT_TEXT && g.valid())
    }

    pub fn capture(wb: &Workbook) -> Self {
        use crate::participant::CoordParticipant;

        let mut overlay = Self {
            sheets: wb
                .sheets
                .iter()
                .map(|s| SheetOverlay {
                    name: s.name.clone(),
                    styles: OverlayStyles::default(),
                    notes: Vec::new(),
                    threads: Vec::new(),
                    rich: Vec::new(),
                    links: Vec::new(),
                    ignored_errors: Vec::new(),
                    merges: Vec::new(),
                    cf: CfSheet::default(),
                    dv: Vec::new(),
                    chart_refs: Vec::new(),
                    sparklines: Vec::new(),
                })
                .collect(),
            palette: Vec::new(),
            sources: Vec::new(),
            names: Vec::new(),
        };
        let mut ids = HashMap::new();
        for participant in CoordParticipant::ALL {
            overlay.capture_participant(participant, wb, &mut ids);
        }
        overlay
    }

    /// Exhaustive per-participant snapshot capture driven by `CoordParticipant::ALL`.
    fn capture_participant(
        &mut self,
        participant: crate::participant::CoordParticipant,
        wb: &Workbook,
        ids: &mut HashMap<u16, u16>,
    ) {
        use crate::participant::CoordParticipant as P;
        match participant {
            P::CellsAndStyles => {
                for (dst, s) in self.sheets.iter_mut().zip(&wb.sheets) {
                    dst.styles = OverlayStyles::capture(s, |id| {
                        *ids.entry(id).or_insert_with(|| {
                            let i = self.palette.len() as u16;
                            self.palette
                                .push((wb.styles.get(id).clone(), StyleExtras::read(wb, id)));
                            i
                        })
                    });
                }
            }
            P::CellNotesAndThreads => {
                for (dst, s) in self.sheets.iter_mut().zip(&wb.sheets) {
                    dst.notes = entries(&s.notes);
                    dst.threads = entries(&s.threads);
                }
            }
            P::RichAndHyperlinks => {
                for (dst, s) in self.sheets.iter_mut().zip(&wb.sheets) {
                    dst.rich = entries(&s.rich);
                    dst.links = entries(&s.hyperlinks);
                }
            }
            P::Merges => {
                for (dst, s) in self.sheets.iter_mut().zip(&wb.sheets) {
                    dst.merges = s.merges.to_vec();
                }
            }
            P::ConditionalAndValidation => {
                for (dst, s) in self.sheets.iter_mut().zip(&wb.sheets) {
                    dst.cf = s.cf.clone();
                    dst.dv = s.dv.clone();
                }
            }
            P::Charts => {
                for (dst, s) in self.sheets.iter_mut().zip(&wb.sheets) {
                    dst.chart_refs = s.charts.iter().map(ChartRefsSnapshot::read).collect();
                }
            }
            P::Sparklines => {
                for (dst, s) in self.sheets.iter_mut().zip(&wb.sheets) {
                    let mut v = s.sparklines.clone();
                    v.sort_by_key(|sp| (sp.r, sp.c));
                    dst.sparklines = v;
                }
            }
            P::DefinedNames => {
                self.names = wb
                    .names
                    .iter()
                    .filter(|(n, _, _)| shared_name(n))
                    .cloned()
                    .collect();
            }
            P::ExternalSources => {
                self.sources = wb
                    .sources
                    .iter()
                    .map(|s| (s.name.clone(), SourceGeometry::read(s)))
                    .collect();
            }
            P::IgnoredErrors => {
                for (dst, s) in self.sheets.iter_mut().zip(&wb.sheets) {
                    let mut v: Vec<_> = s.ignored_errors.iter().copied().collect();
                    v.sort_unstable();
                    dst.ignored_errors = v;
                }
            }
            P::Tables
            | P::Pivots
            | P::ArraysAndSpills
            | P::AutoFilter
            | P::ScenariosAndProtectedRanges
            | P::LayoutAndPage => {}
        }
    }

    pub fn apply_to(&self, wb: &mut Workbook) -> bool {
        use crate::participant::CoordParticipant;

        if !self.wire_valid()
            || self.sheets.len() != wb.sheets.len()
            || self
                .sheets
                .iter()
                .zip(&wb.sheets)
                .any(|(a, b)| a.name != b.name)
        {
            return false;
        }
        let palette: Vec<_> = self
            .palette
            .iter()
            .map(|(s, e)| {
                wb.styles
                    .intern_with_ext(s.clone(), e.theme, e.pattern.clone(), e.quote)
            })
            .collect();
        for participant in CoordParticipant::ALL {
            self.apply_participant(participant, wb, &palette);
        }
        for sh in &mut wb.sheets {
            sh.coordinates_shifted();
        }
        wb.bump_coord_epoch();
        wb.dirty = true;
        true
    }

    /// Exhaustive per-participant overlay application driven by `CoordParticipant::ALL`.
    fn apply_participant(
        &self,
        participant: crate::participant::CoordParticipant,
        wb: &mut Workbook,
        palette: &[u16],
    ) {
        use crate::participant::CoordParticipant as P;
        match participant {
            P::CellsAndStyles => {
                for (overlay, sh) in self.sheets.iter().zip(&mut wb.sheets) {
                    overlay.styles.apply(sh, palette);
                }
            }
            P::CellNotesAndThreads => {
                for (overlay, sh) in self.sheets.iter().zip(&mut wb.sheets) {
                    sh.notes_dirty |= replace_entries(&mut sh.notes, &overlay.notes);
                    sh.threads_dirty |= replace_entries(&mut sh.threads, &overlay.threads);
                }
            }
            P::RichAndHyperlinks => {
                for (overlay, sh) in self.sheets.iter().zip(&mut wb.sheets) {
                    sh.rich_dirty |= replace_entries(&mut sh.rich, &overlay.rich);
                    sh.hyperlinks_dirty |= replace_entries(&mut sh.hyperlinks, &overlay.links);
                }
            }
            P::Merges => {
                for (overlay, sh) in self.sheets.iter().zip(&mut wb.sheets) {
                    if sh.merges != overlay.merges {
                        sh.merges = overlay.merges.clone().into();
                        sh.merges_dirty = true;
                    }
                }
            }
            P::ConditionalAndValidation => {
                for (overlay, sh) in self.sheets.iter().zip(&mut wb.sheets) {
                    if sh.cf != overlay.cf {
                        sh.cf.clone_from(&overlay.cf);
                        sh.cf_dirty = true;
                    }
                    if sh.dv != overlay.dv {
                        sh.dv.clone_from(&overlay.dv);
                        sh.dv_dirty = true;
                    }
                }
            }
            P::Charts => {
                for (overlay, sh) in self.sheets.iter().zip(&mut wb.sheets) {
                    if !overlay.chart_refs.is_empty() && sh.charts.len() == overlay.chart_refs.len()
                    {
                        let mut changed = false;
                        for (snap, ch) in overlay.chart_refs.iter().zip(sh.charts.iter_mut()) {
                            changed |= snap.apply_to_chart(ch);
                        }
                        sh.charts_dirty |= changed;
                    }
                }
            }
            P::Sparklines => {
                for (overlay, sh) in self.sheets.iter().zip(&mut wb.sheets) {
                    let mut cur = sh.sparklines.clone();
                    cur.sort_by_key(|sp| (sp.r, sp.c));
                    if cur != overlay.sparklines {
                        sh.sparklines.clone_from(&overlay.sparklines);
                        sh.sparklines_dirty = true;
                    }
                }
            }
            P::DefinedNames => {
                if !wb
                    .names
                    .iter()
                    .filter(|(n, _, _)| shared_name(n))
                    .eq(&self.names)
                {
                    wb.names.retain(|(n, _, _)| !shared_name(n));
                    wb.names.extend_from_slice(&self.names);
                    wb.names_dirty = true;
                }
            }
            P::ExternalSources => {
                for (name, geo) in &self.sources {
                    if let Some(src) = wb.sources.iter_mut().find(|local| local.name == *name) {
                        geo.write(src);
                    }
                }
            }
            P::IgnoredErrors => {
                for (overlay, sh) in self.sheets.iter().zip(&mut wb.sheets) {
                    let mut cur: Vec<_> = sh.ignored_errors.iter().copied().collect();
                    cur.sort_unstable();
                    if cur != overlay.ignored_errors {
                        sh.ignored_errors = overlay.ignored_errors.iter().copied().collect();
                        sh.ignored_errors_dirty = true;
                    }
                }
            }
            P::Tables
            | P::Pivots
            | P::ArraysAndSpills
            | P::AutoFilter
            | P::ScenariosAndProtectedRanges
            | P::LayoutAndPage => {}
        }
    }
}
