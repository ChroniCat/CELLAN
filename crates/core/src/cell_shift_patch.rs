//! Portable compare-and-swap history for a partial row or column shift in a room.
mod merge_history;
mod overlay;
use merge_history::MergeHistory;
pub use overlay::CellShiftOverlay;

use std::collections::{BTreeSet, HashMap};
use std::hash::Hash;

use serde::{Deserialize, Serialize};

use crate::{
    dv::DataValidation,
    external::{Kind, Source},
    hyperlinks::Hyperlink,
    model::{Cell, CellJournal, CellRef, Sheet, Workbook},
    notes::{Note, ThreadedComment},
    richtext::RichRun,
    structural::CellShift,
    style::CellStyle,
    sync::ValueWire,
};

type Rect = (u32, u32, u32, u32);
const MAX_ENTRIES: usize = 200_000;

fn valid_point(r: u32, c: u32) -> bool {
    r < crate::sync::MAX_ROWS && c < crate::sync::MAX_COLS
}
fn valid_rect(r: &Rect) -> bool {
    r.0 <= r.2 && r.1 <= r.3 && valid_point(r.2, r.3)
}
fn valid_note(r: u32, c: u32, n: &Note) -> bool {
    (n.r, n.c) == (r, c)
        && n.text.len() <= crate::sync::MAX_THREAD_ENTRY_TEXT_BYTES
        && n.author.len() <= crate::sync::MAX_SHORT_TEXT
}
fn valid_thread(r: u32, c: u32, t: &ThreadedComment) -> bool {
    (t.r, t.c) == (r, c)
        && t.entries.len() <= crate::sync::MAX_THREAD_ENTRIES
        && t.entries.iter().all(crate::sync::thread_entry_valid)
}
fn valid_link(r: u32, c: u32, l: &Hyperlink) -> bool {
    l.span.0 < crate::sync::MAX_ROWS - r && l.span.1 < crate::sync::MAX_COLS - c
}
fn valid_changes<T>(
    points: &[ShiftPoint<Option<T>>],
    valid: impl Fn(u32, u32, &T) -> bool,
) -> bool {
    points.iter().all(|p| {
        valid_point(p.row, p.col)
            && p.before
                .iter()
                .chain(&p.after)
                .all(|v| valid(p.row, p.col, v))
    })
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShiftValue {
    pub value: ValueWire,
    pub formula: Option<String>,
}

impl ShiftValue {
    fn read(sheet: &Sheet, row: u32, col: u32) -> Self {
        let cell = sheet.get(row, col);
        let formula = cell
            .as_ref()
            .and_then(|c| c.formula.as_deref())
            .map(str::to_owned);
        // Cached formula results depend on each client's calculation schedule.
        let value = cell
            .as_ref()
            .filter(|_| formula.is_none())
            .map_or(ValueWire::Empty, |c| (&c.value).into());
        Self { value, formula }
    }

    pub fn to_cell(&self) -> Cell {
        Cell {
            value: (&self.value).into(),
            formula: self.formula.as_deref().map(Into::into),
        }
    }
}

#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize)]
struct StyleExtras {
    theme: crate::style_theme::XfTheme,
    pattern: Option<crate::style_ext::FillPattern>,
    quote: bool,
}

impl StyleExtras {
    fn read(wb: &Workbook, id: u16) -> Self {
        Self {
            theme: wb.styles.theme_of(id),
            pattern: wb.styles.pattern_of(id).cloned(),
            quote: wb.styles.quote_of(id),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShiftCell {
    pub sheet: usize,
    pub row: u32,
    pub col: u32,
    pub before: ShiftValue,
    pub after: ShiftValue,
    pub before_style: Option<CellStyle>,
    pub after_style: Option<CellStyle>,
    before_extras: Option<StyleExtras>,
    after_extras: Option<StyleExtras>,
}

impl ShiftCell {
    /// Restore an explicit format, including theme/pattern/quotePrefix, for a winning cell write.
    pub fn apply_style(&self, wb: &mut Workbook, forward: bool) {
        let Some(sh) = wb.sheets.get_mut(self.sheet) else {
            return;
        };
        let style = pick(&self.before_style, &self.after_style, forward);
        let id = style.as_ref().map(|s| {
            let extras = pick(&self.before_extras, &self.after_extras, forward)
                .clone()
                .unwrap_or_default();
            wb.styles
                .intern_with_ext(s.clone(), extras.theme, extras.pattern, extras.quote)
        });
        if sh.explicit_style_of(self.row, self.col) != id {
            let key = CellRef::new(self.row, self.col);
            match id {
                Some(i) => {
                    sh.styles.insert(key, i);
                }
                None => {
                    sh.styles.remove(&key);
                }
            }
            sh.style_edits.insert(key);
            sh.styles_dirty = true;
            sh.bump_display_style_revision();
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ShiftPoint<T> {
    pub row: u32,
    pub col: u32,
    pub before: T,
    pub after: T,
}

trait Point: Copy + Ord + Hash {
    fn coords(self) -> (u32, u32);
    fn point(row: u32, col: u32) -> Self;
}
impl Point for CellRef {
    fn coords(self) -> (u32, u32) {
        (self.row, self.col)
    }
    fn point(row: u32, col: u32) -> Self {
        Self::new(row, col)
    }
}
impl Point for (u32, u32) {
    fn coords(self) -> (u32, u32) {
        self
    }
    fn point(row: u32, col: u32) -> Self {
        (row, col)
    }
}

fn changes<K: Point, V: PartialEq + Clone>(
    before: &HashMap<K, V>,
    after: &HashMap<K, V>,
) -> Vec<ShiftPoint<Option<V>>> {
    if std::ptr::eq(before, after) {
        return Vec::new();
    }
    let keys: BTreeSet<_> = before.keys().chain(after.keys()).copied().collect();
    keys.into_iter()
        .filter_map(|k| {
            let (a, b) = (before.get(&k), after.get(&k));
            let (row, col) = k.coords();
            (a != b).then(|| ShiftPoint {
                row,
                col,
                before: a.cloned(),
                after: b.cloned(),
            })
        })
        .collect()
}

fn pick<'a, T>(a: &'a T, b: &'a T, after: bool) -> &'a T {
    if after { b } else { a }
}

fn map_matches<K: Point, V: PartialEq>(
    map: &HashMap<K, V>,
    items: &[ShiftPoint<Option<V>>],
    forward: bool,
) -> bool {
    items
        .iter()
        .all(|p| map.get(&K::point(p.row, p.col)) == pick(&p.before, &p.after, !forward).as_ref())
}

fn apply_map<K: Point, V: Clone>(
    map: &mut HashMap<K, V>,
    items: &[ShiftPoint<Option<V>>],
    forward: bool,
) {
    for p in items {
        let key = K::point(p.row, p.col);
        if let Some(v) = pick(&p.before, &p.after, forward) {
            map.insert(key, v.clone());
        } else {
            map.remove(&key);
        }
    }
}

fn merge_map(sheet: &Sheet) -> HashMap<(u32, u32), Rect> {
    sheet.merges.iter().map(|&r| ((r.0, r.1), r)).collect()
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct SourceGeometry {
    at: (u32, u32),
    target: Option<String>,
    rows: u32,
    cols: u32,
    internal: Option<(String, Rect)>,
    detached: bool,
}

impl SourceGeometry {
    fn read(src: &Source) -> Self {
        let (kind, detached) = match &src.kind {
            Kind::Unbound(inner) => (inner.as_ref(), true),
            k => (k, false),
        };
        let internal = match kind {
            Kind::Internal { sheet, rect } => Some((sheet.clone(), *rect)),
            _ => None,
        };
        Self {
            at: src.at,
            target: src.target_sheet.clone(),
            rows: src.rows,
            cols: src.cols,
            internal,
            detached,
        }
    }

    // The local CSV path, import options, source kind, source sheet and size of
    // the imported data belong to this document. Only coordinates can travel.
    fn compatible(&self, src: &Source) -> bool {
        let other = Self::read(src);
        self.rows == other.rows
            && self.cols == other.cols
            && self.internal.as_ref().map(|i| &i.0) == other.internal.as_ref().map(|i| &i.0)
    }

    fn write(&self, src: &mut Source) {
        if !self.compatible(src) {
            return;
        }
        src.at = self.at;
        src.target_sheet.clone_from(&self.target);
        let mut kind = match &src.kind {
            Kind::Unbound(inner) => (**inner).clone(),
            k => k.clone(),
        };
        if let (Kind::Internal { rect, .. }, Some((_, next))) = (&mut kind, &self.internal) {
            *rect = *next;
        }
        src.kind = if self.detached {
            Kind::Unbound(Box::new(kind))
        } else {
            kind
        };
    }
    fn valid(&self) -> bool {
        valid_point(self.at.0, self.at.1)
            && self.rows <= crate::sync::MAX_ROWS
            && self.cols <= crate::sync::MAX_COLS
            && self
                .target
                .as_ref()
                .is_none_or(|n| n.len() <= crate::sync::MAX_SHORT_TEXT)
            && self
                .internal
                .as_ref()
                .is_none_or(|(n, r)| n.len() <= crate::sync::MAX_SHORT_TEXT && valid_rect(r))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct NameChange {
    name: String,
    scope: Option<usize>,
    before: String,
    after: String,
}

fn shared_name(name: &str) -> bool {
    !name.starts_with("_sc_src_") // Legacy import definitions may contain local file paths.
}

fn valid_defined_name(name: &str, scope: Option<usize>) -> bool {
    !name.is_empty() && name.chars().count() <= 255 && shared_name(name)
        && scope.is_none_or(|index| index < crate::sync::MAX_SHEET_INDEX)
}

fn valid_name_formula(text: &str) -> bool {
    text.len() <= crate::sync::MAX_THREAD_ENTRY_TEXT_BYTES
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct SourceChange {
    name: String,
    before: SourceGeometry,
    after: SourceGeometry,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct RuleChange<T> {
    sheet: usize,
    index: usize,
    before: T,
    after: T,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CellShiftPatch {
    sheet: usize,
    sheet_names: Vec<(usize, String)>,
    pub rect: Rect,
    pub horizontal: bool,
    pub insert: bool,
    pub cells: Vec<ShiftCell>,
    pub notes: Vec<ShiftPoint<Option<Note>>>,
    pub threads: Vec<ShiftPoint<Option<ThreadedComment>>>,
    pub merges: Vec<ShiftPoint<Option<Rect>>>,
    merge_history: MergeHistory,
    rich: Vec<ShiftPoint<Option<Vec<RichRun>>>>,
    links: Vec<ShiftPoint<Option<Hyperlink>>>,
    cf: Vec<RuleChange<crate::cf::CfBlock>>,
    dv: Vec<RuleChange<DataValidation>>,
    sources: Vec<SourceChange>,
    names: Vec<NameChange>,
}

impl CellShiftPatch {
    pub fn target_sheet(&self) -> usize {
        self.sheet
    }

    pub fn operation(&self) -> CellShift {
        let direction = if self.horizontal {
            crate::cells::Shift::Horizontal
        } else {
            crate::cells::Shift::Vertical
        };
        CellShift::new(self.rect, direction, self.insert)
    }

    pub fn referenced_sheets(&self) -> Vec<usize> {
        let mut sheets = BTreeSet::new();
        sheets.insert(self.sheet);
        sheets.extend(self.cells.iter().map(|c| c.sheet));
        sheets.extend(self.cf.iter().map(|c| c.sheet));
        sheets.extend(self.dv.iter().map(|c| c.sheet));
        sheets.extend(self.names.iter().filter_map(|n| n.scope));
        sheets.into_iter().collect()
    }

    pub fn is_empty(&self) -> bool {
        self.entries() == 0
    }

    fn entries(&self) -> usize {
        self.cells.len()
            + self.notes.len()
            + self.threads.len()
            + self.merges.len()
            + self.merge_history.extra_entries()
            + self.rich.len()
            + self.links.len()
            + self.cf.len()
            + self.dv.len()
            + self.sources.len() + self.names.len()
    }

    pub fn wire_valid(&self) -> bool {
        let max_sheet = crate::sync::MAX_SHEET_INDEX;
        let valid_name = |s: &String| !s.is_empty() && s.len() <= crate::sync::MAX_SHORT_TEXT;
        self.entries() <= MAX_ENTRIES
            && valid_rect(&self.rect)
            && self.sheet < max_sheet
            && self.sheet_names.len() <= max_sheet
            && self
                .sheet_names
                .iter()
                .all(|(i, n)| *i < max_sheet && valid_name(n))
            && self.sheet_names.iter().any(|(i, _)| *i == self.sheet)
            && self
                .cells
                .iter()
                .all(|c| c.sheet < max_sheet && valid_point(c.row, c.col))
            && valid_changes(&self.notes, valid_note)
            && valid_changes(&self.threads, valid_thread)
            && valid_changes(&self.merges, |r, c, v| {
                valid_rect(v) && (v.0, v.1) == (r, c)
            })
            && self.merge_history.valid(self.merges.len())
            && valid_changes(&self.links, valid_link)
            && valid_changes(&self.rich, |_, _, _| true)
            && self.cf.iter().all(|c| {
                c.sheet < max_sheet
                    && c.before
                        .ranges
                        .iter()
                        .chain(&c.after.ranges)
                        .all(valid_rect)
            })
            && self.dv.iter().all(|c| {
                c.sheet < max_sheet
                    && c.before
                        .ranges
                        .iter()
                        .chain(&c.after.ranges)
                        .all(valid_rect)
            })
            && {
                let mut seen = std::collections::HashSet::with_capacity(self.names.len());
                self.names.iter().all(|n| {
                    valid_defined_name(&n.name, n.scope)
                        && n.before != n.after
                        && valid_name_formula(&n.before)
                        && valid_name_formula(&n.after)
                        && seen.insert((n.scope, n.name.as_str()))
                })
            }
            && self
                .sources
                .iter()
                .all(|s| valid_name(&s.name) && s.before.valid() && s.after.valid())
    }

    pub fn prepare(wb: &Workbook, sheet: usize, op: CellShift) -> Option<Self> {
        op.preflight(wb, sheet).ok()?;
        let a = &wb.sheets[sheet];
        // Only the target has moving cell metadata. Formula rewriting on other
        // sheets needs their shared cell storage and conditional/validation rules.
        let mut result = Workbook {
            styles: wb.styles.clone(),
            names: wb.names.clone(),
            sources: wb.sources.clone(),
            sheets: wb
                .sheets
                .iter()
                .enumerate()
                .map(|(i, s)| {
                    let mut copy = s.search_snapshot();
                    copy.cf = s.cf.clone();
                    copy.dv = s.dv.clone();
                    if i == sheet {
                        copy.merges = s.merges.clone();
                        copy.notes = s.notes.clone();
                        copy.threads = s.threads.clone();
                        copy.rich = s.rich.clone();
                        copy.hyperlinks = s.hyperlinks.clone();
                        copy.spilled = s.spilled.clone();
                    }
                    copy
                })
                .collect(),
            ..Default::default()
        };
        if !op.apply(&mut result, sheet) {
            return None;
        }
        let mut name_counts: HashMap<(Option<usize>, &str), usize> =
            HashMap::with_capacity(wb.names.len());
        for (name, scope, _) in &wb.names {
            *name_counts.entry((*scope, name.as_str())).or_insert(0) += 1;
        }
        let mut names = Vec::new();
        for ((name, scope, before), (_, _, after)) in wb.names.iter().zip(&result.names) {
            if shared_name(name) && before != after {
                if name_counts.get(&(*scope, name.as_str())).copied().unwrap_or(0) != 1 {
                    return None;
                }
                names.push(NameChange {
                    name: name.clone(),
                    scope: *scope,
                    before: before.clone(),
                    after: after.clone(),
                });
            }
        }
        let b = &result.sheets[sheet];
        let mut patch = Self {
            sheet,
            sheet_names: Vec::new(),
            rect: op.rect,
            horizontal: op.direction == crate::cells::Shift::Horizontal,
            insert: op.insert,
            cells: Vec::new(),
            notes: changes(&a.notes, &b.notes),
            threads: changes(&a.threads, &b.threads),
            rich: changes(&a.rich, &b.rich),
            links: changes(&a.hyperlinks, &b.hyperlinks),
            merges: if a.merges == b.merges {
                Vec::new()
            } else {
                changes(&merge_map(a), &merge_map(b))
            },
            cf: Vec::new(),
            dv: Vec::new(),
            sources: Vec::new(),
            names,
            merge_history: MergeHistory::default(),
        };
        if a.merges != b.merges {
            patch.merge_history = MergeHistory::capture(&a.merges, &b.merges, &patch.merges);
        }
        // A shift can replace only the runs of two equal text values. Carry
        // their cell clocks as well so an optimistic later edit wins on replicas.
        let rich_keys: BTreeSet<_> = patch
            .rich
            .iter()
            .map(|p| CellRef::new(p.row, p.col))
            .collect();
        for (idx, (a, b)) in wb.sheets.iter().zip(&result.sheets).enumerate() {
            let prior_entries = patch.entries();
            let mut touched = CellJournal::default();
            a.cells().diff_into_journal(b.cells(), &mut touched);
            a.styles.diff_into_journal(&b.styles, &mut touched);
            if idx == sheet {
                for &key in &rich_keys {
                    touched.insert(key);
                }
            }
            if touched.len() > MAX_ENTRIES {
                return None;
            }
            let mut coords: Vec<_> = touched.iter().collect();
            coords.sort_unstable();
            for key in coords {
                let (row, col) = (key.row, key.col);
                let (before, after) =
                    (ShiftValue::read(a, row, col), ShiftValue::read(b, row, col));
                let (old_id, new_id) =
                    (a.explicit_style_of(row, col), b.explicit_style_of(row, col));
                if before == after
                    && old_id == new_id
                    && !(idx == sheet && rich_keys.contains(&key))
                {
                    continue;
                }
                let old_style = old_id.map(|id| wb.styles.get(id).clone());
                let new_style = new_id.map(|id| result.styles.get(id).clone());
                let extras = |wb: &Workbook, id: Option<u16>| {
                    id.map(|id| StyleExtras::read(wb, id))
                        .filter(|e| *e != StyleExtras::default())
                };
                patch.cells.push(ShiftCell {
                    sheet: idx,
                    row,
                    col,
                    before,
                    after,
                    before_style: old_style,
                    after_style: new_style,
                    before_extras: extras(wb, old_id),
                    after_extras: extras(&result, new_id),
                });
            }
            for (index, (old, new)) in a.cf.blocks.iter().zip(&b.cf.blocks).enumerate() {
                if old != new {
                    patch.cf.push(RuleChange {
                        sheet: idx,
                        index,
                        before: old.clone(),
                        after: new.clone(),
                    });
                }
            }
            for (index, (old, new)) in a.dv.iter().zip(&b.dv).enumerate() {
                if old != new {
                    patch.dv.push(RuleChange {
                        sheet: idx,
                        index,
                        before: old.clone(),
                        after: new.clone(),
                    });
                }
            }
            if idx == sheet
                || patch.entries() != prior_entries
                || patch.names.iter().any(|n| n.scope == Some(idx))
            {
                patch.sheet_names.push((idx, a.name.clone()));
            }
        }
        for (old, new) in wb.sources.iter().zip(&result.sources) {
            if old != new {
                patch.sources.push(SourceChange {
                    name: old.name.clone(),
                    before: SourceGeometry::read(old),
                    after: SourceGeometry::read(new),
                });
            }
        }
        patch.can_install(wb).then_some(patch)
    }

    /// Recompute on the authoritative document before accepting a guest's first request.
    pub fn validate_forward(&self, wb: &Workbook) -> bool {
        self.canonical_forward(wb).is_some()
    }

    /// A guest may lack the host's private source configuration. Derive those
    /// binding changes on the host, and verify every other field byte-for-byte.
    pub fn canonical_forward(&self, wb: &Workbook) -> Option<Self> {
        if !self.wire_valid() {
            return None;
        }
        let mut canonical = Self::prepare(wb, self.sheet, self.operation())?;
        let host_sources: HashMap<_, _> = canonical.sources.iter().map(|s| (&s.name, s)).collect();
        if self.sources.len() > host_sources.len()
            || self
                .sources
                .iter()
                .any(|s| host_sources.get(&s.name) != Some(&s))
        {
            return None;
        }
        // A guest joining without the base file may only know a subset of names.
        let host_names: HashMap<_, _> = canonical
            .names
            .iter()
            .map(|n| ((n.scope, &n.name), n))
            .collect();
        if self.names.len() > host_names.len()
            || self
                .names
                .iter()
                .any(|n| host_names.get(&(n.scope, &n.name)) != Some(&n))
        {
            return None;
        }
        let expected_sheet_names = self
            .referenced_sheets()
            .into_iter()
            .map(|idx| wb.sheets.get(idx).map(|s| (idx, s.name.clone())))
            .collect::<Option<Vec<_>>>()?;
        let sheet_names = std::mem::replace(&mut canonical.sheet_names, expected_sheet_names);
        let names = std::mem::replace(&mut canonical.names, self.names.clone());
        let sources = std::mem::replace(&mut canonical.sources, self.sources.clone());
        if canonical != *self {
            return None;
        }
        canonical.sheet_names = sheet_names;
        canonical.sources = sources;
        canonical.names = names;
        Some(canonical)
    }

    pub fn can_install(&self, wb: &Workbook) -> bool {
        self.wire_valid()
            && wb.sheets.get(self.sheet).is_some()
            && self
                .sheet_names
                .iter()
                .all(|(i, n)| wb.sheets.get(*i).is_some_and(|s| s.name == *n))
            && self.names.iter().all(|n| n.scope.is_none_or(|i| i < wb.sheets.len()))
            && self.cells.iter().all(|c| {
                wb.sheets.get(c.sheet).is_some()
                    && c.row < crate::sync::MAX_ROWS
                    && c.col < crate::sync::MAX_COLS
            })
            && self.cf.iter().all(|c| {
                wb.sheets
                    .get(c.sheet)
                    .and_then(|s| s.cf.blocks.get(c.index))
                    .is_some()
            })
            && self.dv.iter().all(|c| {
                wb.sheets
                    .get(c.sheet)
                    .and_then(|s| s.dv.get(c.index))
                    .is_some()
            })
    }

    pub fn matches(&self, wb: &Workbook, forward: bool) -> bool {
        self.matches_document(wb, forward, false)
    }

    pub fn matches_replica(&self, wb: &Workbook, forward: bool) -> bool {
        self.matches_document(wb, forward, true)
    }

    fn effective_operation(&self, forward: bool) -> CellShift {
        let direction = if self.horizontal {
            crate::cells::Shift::Horizontal
        } else {
            crate::cells::Shift::Vertical
        };
        CellShift::new(self.rect, direction, self.insert == forward)
    }

    fn merges_complete(&self, sheet: &Sheet, effective_op: CellShift, forward: bool) -> bool {
        let Some(mut expected) = effective_op.shifted_merges(&sheet.merges) else {
            return false;
        };
        let mut actual_merges = sheet.merges.clone();
        self.merge_history
            .install_merges(&mut actual_merges, &self.merges, forward);
        let mut actual = actual_merges.to_vec();
        expected.sort_unstable();
        actual.sort_unstable();
        if self.insert || forward {
            return expected == actual;
        }
        // Undoing a delete (`!self.insert && !forward`): `actual` must contain
        // `expected` plus only deleted merges restored wholly inside `self.rect`.
        let (r1, c1, r2, c2) = self.rect;
        let inside_deleted_rect = |m: Rect| m.0 >= r1 && m.2 <= r2 && m.1 >= c1 && m.3 <= c2;
        let mut ei = 0;
        for m in actual {
            if ei < expected.len() && expected[ei] == m {
                ei += 1;
            } else if !inside_deleted_rect(m) {
                return false;
            }
        }
        ei == expected.len()
    }

    fn matches_document(&self, wb: &Workbook, forward: bool, allow_missing_sources: bool) -> bool {
        if !self.can_install(wb) {
            return false;
        }
        let sheet = &wb.sheets[self.sheet];
        let effective_op = self.effective_operation(forward);
        if effective_op.preflight_merges(sheet).is_err()
            || effective_op.preflight_objects(wb, self.sheet).is_err()
        {
            return false;
        }
        let rich_changes: BTreeSet<_> = self.rich.iter().map(|p| (p.row, p.col)).collect();
        let cells_match = self.cells.iter().all(|c| {
            let sh = &wb.sheets[c.sheet];
            let id = sh.explicit_style_of(c.row, c.col);
            let extras = id
                .map(|id| StyleExtras::read(wb, id))
                .filter(|e| *e != StyleExtras::default());
            let new_rich = c.before != c.after
                && sh.rich.contains_key(&(c.row, c.col))
                && (c.sheet != self.sheet || !rich_changes.contains(&(c.row, c.col)));
            !new_rich
                && ShiftValue::read(sh, c.row, c.col) == *pick(&c.before, &c.after, !forward)
                && id.map(|i| wb.styles.get(i))
                    == pick(&c.before_style, &c.after_style, !forward).as_ref()
                && extras == *pick(&c.before_extras, &c.after_extras, !forward)
        });
        if !cells_match
            || !map_matches(&sheet.notes, &self.notes, forward)
            || !map_matches(&sheet.threads, &self.threads, forward)
            || !map_matches(&sheet.rich, &self.rich, forward)
            || !map_matches(&sheet.hyperlinks, &self.links, forward)
        {
            return false;
        }
        if !self.merge_history.matches(sheet, &self.merges, forward)
            || !self.merges_complete(sheet, effective_op, forward)
        {
            return false;
        }
        self.cf
            .iter()
            .all(|c| wb.sheets[c.sheet].cf.blocks[c.index] == *pick(&c.before, &c.after, !forward))
            && self
                .dv
                .iter()
                .all(|c| wb.sheets[c.sheet].dv[c.index] == *pick(&c.before, &c.after, !forward))
            && self.source_baseline_matches(wb, forward, allow_missing_sources)
            && self.names_match(wb, forward, allow_missing_sources)
    }

    fn names_match(&self, wb: &Workbook, forward: bool, allow_missing: bool) -> bool {
        let mut expected: HashMap<_, _> = self
            .names
            .iter()
            .map(|n| ((n.scope, n.name.as_str()), n))
            .collect();
        let mut matched = std::collections::HashSet::with_capacity(expected.len());
        let mut shift = self.operation().reference_shift();
        shift.target_sheet.clone_from(&wb.sheets[self.sheet].name);
        if !forward {
            shift.delete = !shift.delete;
        }
        for (name, scope, text) in &wb.names {
            let key = (*scope, name.as_str());
            if matched.contains(&key) {
                return false;
            }
            if let Some(delta) = expected.remove(&key) {
                if text != pick(&delta.before, &delta.after, !forward) {
                    return false;
                }
                matched.insert(key);
            } else if shared_name(name) {
                shift.own_sheet.clear();
                if let Some(sh) = scope.and_then(|i| wb.sheets.get(i)) {
                    shift.own_sheet.push_str(&sh.name);
                }
                if crate::refs::rewrite(text, &shift) != *text {
                    return false;
                }
            }
        }
        allow_missing || expected.is_empty()
    }

    fn source_baseline_matches(&self, wb: &Workbook, forward: bool, allow_missing: bool) -> bool {
        if wb.sources.is_empty() && self.sources.is_empty() {
            return true;
        }
        let mut existing: HashMap<_, _> =
            self.sources.iter().map(|s| (s.name.as_str(), s)).collect();
        let mut shift = self.operation().reference_shift();
        if !forward {
            shift.delete = !shift.delete;
        }
        for src in &wb.sources {
            if let Some(change) = existing.remove(src.name.as_str()) {
                if SourceGeometry::read(src) != *pick(&change.before, &change.after, !forward) {
                    return false;
                }
            } else {
                // A new binding in a band being replayed has no reversible
                // delta. Reject the entire transaction if its anchor would move.
                let mut moved = src.clone();
                crate::external::remap_source_slice(
                    std::slice::from_mut(&mut moved),
                    &wb.sheets[self.sheet].name,
                    wb.sheets.len() == 1,
                    shift.axis,
                    shift.row_filter.or(shift.col_filter),
                    |a, b| shift.map_range(a, b),
                );
                if moved != *src {
                    return false;
                }
            }
        }
        allow_missing || existing.is_empty()
    }

    pub fn apply(&self, wb: &mut Workbook, forward: bool) -> bool {
        self.matches(wb, forward) && self.apply_committed(wb, forward)
    }

    /// Apply an accepted transaction on a replica, regardless of locally cached inputs.
    pub fn apply_committed(&self, wb: &mut Workbook, forward: bool) -> bool {
        if !self.can_install(wb) {
            return false;
        }
        for c in &self.cells {
            let sh = &mut wb.sheets[c.sheet];
            let desired = pick(&c.before, &c.after, forward);
            if ShiftValue::read(sh, c.row, c.col) != *desired {
                sh.set(c.row, c.col, desired.to_cell());
            }
            c.apply_style(wb, forward);
        }
        self.replay_metadata(wb, forward)
    }

    /// Apply just the accepted metadata and invalidate coordinate-sensitive work.
    pub fn replay_metadata(&self, wb: &mut Workbook, forward: bool) -> bool {
        self.replay_metadata_for_cells(wb, forward, |_, _| true)
    }

    /// A newer local value takes its own formatting runs with it. Other metadata,
    /// such as comments, is independent of the value's last-writer timestamp.
    pub fn replay_metadata_for_cells(
        &self,
        wb: &mut Workbook,
        forward: bool,
        accepts_rich: impl Fn(u32, u32) -> bool,
    ) -> bool {
        if !self.can_install(wb) {
            return false;
        }
        let sh = &mut wb.sheets[self.sheet];
        if !self.notes.is_empty() {
            apply_map(&mut sh.notes, &self.notes, forward);
            sh.notes_dirty = true;
        }
        if !self.threads.is_empty() {
            apply_map(&mut sh.threads, &self.threads, forward);
            sh.threads_dirty = true;
        }
        for p in self.rich.iter().filter(|p| accepts_rich(p.row, p.col)) {
            apply_map(&mut sh.rich, std::slice::from_ref(p), forward);
            sh.rich_dirty = true;
        }
        if !self.links.is_empty() {
            apply_map(&mut sh.hyperlinks, &self.links, forward);
            sh.hyperlinks_dirty = true;
        }
        self.merge_history.install(sh, &self.merges, forward);
        sh.coordinates_shifted();
        for c in &self.cf {
            let sh = &mut wb.sheets[c.sheet];
            sh.cf.blocks[c.index].clone_from(pick(&c.before, &c.after, forward));
            sh.cf_dirty = true;
        }
        for c in &self.dv {
            let sh = &mut wb.sheets[c.sheet];
            sh.dv[c.index].clone_from(pick(&c.before, &c.after, forward));
            sh.dv_dirty = true;
        }
        if !self.names.is_empty() {
            let positions: HashMap<_, _> = wb.names.iter().enumerate()
                .map(|(i, (name, scope, _))| ((*scope, name.clone()), i)).collect();
            for change in &self.names {
                let formula = pick(&change.before, &change.after, forward);
                if let Some(&i) = positions.get(&(change.scope, change.name.clone())) {
                    wb.names[i].2.clone_from(formula);
                } else {
                    wb.names.push((change.name.clone(), change.scope, formula.clone()));
                }
            }
            wb.names_dirty = true;
        }
        for s in &self.sources {
            if let Some(src) = wb.sources.iter_mut().find(|local| local.name == s.name) {
                pick(&s.before, &s.after, forward).write(src);
            }
        }
        wb.dirty = true;
        true
    }
}
