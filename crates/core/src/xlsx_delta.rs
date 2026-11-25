//! Patch ordinary cell edits against the exact worksheet captured during load
//! or save. Unsupported cell representations use the full serializer.
use crate::model::{Cell, CellRef, Sheet, Value, Workbook};
use crate::store::{CellStyles, Store};
use crate::style::StyleTable;
#[path = "xlsx_font_delta.rs"]
mod font_delta;
#[path = "xlsx_stream_delta.rs"]
pub(super) mod stream;
use anyhow::{Context, Result};
use quick_xml::{
    Reader,
    events::{BytesStart, Event},
};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs::File,
    io::{BufWriter, Write},
    path::Path,
    sync::Arc,
};
use zip::{ZipArchive, ZipWriter, write::SimpleFileOptions};

#[derive(Clone, Debug)]
pub(crate) struct SourceCells {
    pub(crate) layout: crate::patch::layout_xml::Baseline,
    store: Arc<Store>,
    signature: (u32, u64),
    strings: Option<(u32, u64)>,
    style_part: Option<(u32, u64)>,
    style_map: Option<Arc<StyleMap>>,
    notes: Option<NoteSource>,
    styles: CellStyles,
    pub(super) row_styles: crate::model::CellMetadata<u32,u16>,
    pub(super) col_styles: crate::model::CellMetadata<u32,u16>,
    pub(super) rich: crate::model::CellMetadata<(u32, u32), Vec<crate::richtext::RichRun>>,
    arrays: Arc<Vec<(CellRef, Rect)>>,
    spill_shapes: crate::model::CellMetadata<CellRef, (u32, u32)>,
    spilled: crate::model::CellSet<CellRef>,
    clean_shell: Option<Arc<str>>,
    sst_arcs: Option<Arc<[Arc<str>]>>,
}

type Rect = (u32, u32, u32, u32);
#[cfg(test)]
pub(super) use stream::sst;

#[cfg(test)]
pub(super) fn split_grid(
    input: impl std::io::Read,
    part: &str,
    shell: &mut dyn Write,
    grid: Option<&mut dyn Write>,
) -> Result<(bool, Option<String>)> {
    let mut result = None;
    stream::xml_input(input, part, |xml| {
        result = Some(stream::split_grid(xml, shell, grid)?);
        Ok(true)
    })?;
    result.context("无法读取工作表数据")
}

#[derive(Default)]
struct StyleChanges {
    updates: HashMap<CellRef, Option<u16>>,
    rows: BTreeMap<u32, Option<u16>>,
    cols: BTreeMap<u32, Option<u16>>,
    derived_axis: bool,
    dynamic_index: Option<u32>,
    dynamic: HashMap<CellRef, (Option<Rect>, Option<Rect>)>,
    // A derived XF belongs to a specific pre-edit raw XF. Confirm that
    // original on the worksheet too, including model-default/ambiguous cells.
    sources: HashMap<CellRef, u16>,
}

impl StyleChanges {
    fn matches(&self, key: CellRef, style: Option<u32>) -> bool {
        self.sources.get(&key).is_none_or(|&s| u32::from(s) == style.unwrap_or(0))
    }
}

#[derive(Clone, Debug)]
pub(super) struct StyleMap {
    // Only a unique original cellXf can be reused: distinct raw XFs may
    // collapse to one modeled style (e.g. unmodeled alignment properties).
    xfs: HashMap<u16, Option<u16>>,
    derived_from: HashMap<u16, Option<u16>>,
    // umya may materialize a default font/alignment. These XF numbers are
    // candidates only for editing the existing cell, never for assigning a
    // style elsewhere. The worksheet must still prove its original s attribute.
    origin_only: HashMap<u16, Option<u16>>,
    original: StyleTable,
}

impl StyleMap {
    pub(super) fn new(ids: &HashMap<u16, u16>, styles: &StyleTable) -> Arc<Self> {
        let mut xfs = HashMap::new();
        for (&xf, &id) in ids {
            xfs.entry(id)
                .and_modify(|slot| *slot = None)
                .or_insert(Some(xf));
        }
        Arc::new(Self {
            xfs,
            derived_from: HashMap::new(),
            origin_only: HashMap::new(),
            original: styles.clone(),
        })
    }

    pub(super) fn recovered(
        ids: &HashMap<u16, u16>,
        defaults: HashMap<u16, Option<u16>>,
        styles: &StyleTable,
    ) -> Arc<Self> {
        let mut map = Self::new(ids, styles);
        Arc::make_mut(&mut map).origin_only = defaults;
        map
    }

    fn editing_origin(&self, id: u16, styles: &StyleTable) -> Option<u16> {
        if self.xfs.contains_key(&id) {
            return self.xf(id, styles);
        }
        self.unchanged(id, styles)
            .then(|| self.origin_only.get(&id).copied().flatten())
            .flatten()
    }

    fn unchanged(&self, id: u16, current: &StyleTable) -> bool {
        let old = &self.original;
        usize::from(id) < current.len()
            && usize::from(id) < old.len()
            && old.get(id) == current.get(id)
            && old.theme_of(id) == current.theme_of(id)
            && old.pattern_of(id) == current.pattern_of(id)
            && old.diag_of(id) == current.diag_of(id)
            && old.quote_of(id) == current.quote_of(id)
    }

    fn xf(&self, id: u16, current: &StyleTable) -> Option<u16> {
        self.unchanged(id, current).then(|| self.xfs.get(&id).copied().flatten()).flatten()
    }
}

pub(super) struct SavedStyles {
    signature: Signature,
    map: Arc<StyleMap>,
}

impl SavedStyles {
    pub(super) fn from_direct(
        signature: Signature,
        ids: &HashMap<u16, u16>,
        styles: &StyleTable,
    ) -> Self {
        Self {
            signature,
            map: StyleMap::new(ids, styles),
        }
    }

    pub(super) fn attach(&self, source: &mut SourceCells) {
        if source.style_part == Some(self.signature) {
            source.set_style_map(self.map.clone());
        }
    }
}

fn signature<R: std::io::Read + std::io::Seek>(
    zip: &mut ZipArchive<R>,
    part: &str,
) -> Option<(u32, u64)> {
    let entry = zip.by_index_raw(zip.index_for_name(part)?).ok()?;
    Some((entry.crc32(), entry.size()))
}

type Signature = (u32, u64);

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct NoteTarget {
    id: String,
    kind: String,
    path: String,
}

#[derive(Clone, Debug)]
struct NoteSource {
    rels: Option<Signature>,
    targets: Arc<Vec<NoteTarget>>,
    parts: Vec<(String, Signature)>,
}

impl NoteSource {
    fn matches_legacy_notes<R: std::io::Read + std::io::Seek>(
        &self,
        zip: &mut ZipArchive<R>,
        sheet: &Sheet,
    ) -> bool {
        let mut comments = self.targets.iter().filter(|t| t.kind != "vmlDrawing");
        let Some(target) = comments.next() else {
            return sheet.notes.is_empty();
        };
        if target.kind != "comments" || comments.next().is_some() {
            return false;
        }
        super::read_zip_text(zip, &target.path).is_ok_and(|xml| {
            let disk = crate::notes::parse_comments(&xml);
            disk.len() == sheet.notes.len()
                && disk
                    .iter()
                    .all(|n| sheet.notes.get(&CellRef::new(n.r, n.c)) == Some(n))
        })
    }

    fn capture<R: std::io::Read + std::io::Seek>(
        zip: &mut ZipArchive<R>,
        part: &str,
        prior: Option<&Self>,
    ) -> Result<Self> {
        let (dir, name) = part.rsplit_once('/').context("工作表部件路径无效")?;
        let relname = format!("{dir}/_rels/{name}.rels");
        let rels = signature(zip, &relname);
        // A save receipt normally only needs central-directory lookups. An
        // actual relationship change is the only reason to parse the XML again.
        let targets = if let Some(old) = prior.filter(|n| n.rels == rels) {
            old.targets.clone()
        } else if rels.is_some() {
            let xml = super::read_zip_text(zip, &relname)?;
            let doc = roxmltree::Document::parse(&xml)?;
            let root = doc.root_element();
            anyhow::ensure!(
                root.tag_name().name() == "Relationships",
                "工作表批注关系无效"
            );
            let mut links = Vec::new();
            for rel in root
                .children()
                .filter(|n| n.is_element() && n.tag_name().name() == "Relationship")
            {
                let kind = rel
                    .attribute("Type")
                    .unwrap_or("")
                    .trim_end_matches('/')
                    .rsplit('/')
                    .next()
                    .unwrap_or("");
                if !matches!(kind, "comments" | "threadedComment" | "vmlDrawing")
                    || rel
                        .attribute("TargetMode")
                        .is_some_and(|m| m.eq_ignore_ascii_case("External"))
                {
                    continue;
                }
                let raw = rel.attribute("Target").context("批注关系缺少目标")?;
                let path = if raw.starts_with("xl/") {
                    raw.to_string()
                } else {
                    crate::fixups::resolve_target(&relname, raw)
                };
                links.push(NoteTarget {
                    id: rel.attribute("Id").context("批注关系缺少标识")?.into(),
                    kind: kind.into(),
                    path,
                });
            }
            links.sort();
            Arc::new(links)
        } else {
            Arc::new(Vec::new())
        };
        let mut paths: BTreeSet<String> = targets.iter().map(|t| t.path.clone()).collect();
        if targets.iter().any(|t| t.kind == "threadedComment") {
            // This is also how notes::load_notes identifies the author tables.
            paths.extend(
                zip.file_names()
                    .filter(|p| p.starts_with("xl/persons/") && p.ends_with(".xml"))
                    .map(str::to_owned),
            );
        }
        let mut parts = Vec::with_capacity(paths.len());
        for path in paths {
            let stamp =
                signature(zip, &path).with_context(|| format!("批注关联部件不存在：{path}"))?;
            parts.push((path, stamp));
        }
        Ok(Self {
            rels,
            targets,
            parts,
        })
    }
}

fn note_edits_with_source(wb: &Workbook) -> impl Iterator<Item = (&Sheet, &str, &SourceCells)> {
    wb.sheets
        .iter()
        .filter(|s| s.notes_dirty && !wb.load_incomplete)
        .filter_map(|s| Some((s, s.source_part.as_deref()?, s.source_cells.as_ref()?)))
}

fn check_note_sources(zip: &mut ZipArchive<File>, wb: &Workbook) -> Result<()> {
    for (sheet, part, source) in note_edits_with_source(wb) {
        let prior = source.notes.as_ref().with_context(|| {
            format!(
                "未能核实工作表「{}」的原始批注部件，请重新打开工作簿",
                sheet.name
            )
        })?;
        let current = NoteSource::capture(zip, part, Some(prior))?;
        anyhow::ensure!(
            zip.index_for_name(part).is_some()
                && prior.parts == current.parts
                && prior.targets == current.targets,
            "工作表「{}」或其批注已被其他程序修改，原文件未被覆盖，请重新打开工作簿",
            sheet.name
        );
    }
    Ok(())
}

pub(super) fn verify_note_sources(path: &Path, wb: &Workbook) -> Result<()> {
    if note_edits_with_source(wb).next().is_none() {
        return Ok(());
    }
    let mut zip = ZipArchive::new(File::open(path).context("无法读取批注基准，原文件未被覆盖")?)?;
    check_note_sources(&mut zip, wb)
}

impl SourceCells {
    pub(super) fn reusable_grid(
        &self,
        zip: &mut ZipArchive<File>,
        part: &str,
        sheet: &Sheet,
    ) -> bool {
        self.styles == sheet.styles
            && self.rich == sheet.rich
            && self.arrays.as_ref() == &sheet.arrays
            && self.spill_shapes == sheet.spill_shapes
            && self.spilled == sheet.spilled
            && signature(zip, part) == Some(self.signature)
            && signature(zip, "xl/styles.xml") == self.style_part
            && signature(zip, "xl/sharedStrings.xml") == self.strings
    }

    /// Stage supported value and formula-cache edits before umya handles comments
    /// or drawings. The private file is discarded immediately after splitting it.
    pub(super) fn stage_grid_edits(
        &self,
        zip: &mut ZipArchive<File>,
        part: &str,
        sheet: &Sheet,
        styles: &StyleTable,
    ) -> Result<Option<tempfile::NamedTempFile>> {
        let Some(changes) = coordinates(sheet, self, styles, None, |_, _, _| None) else {
            return Ok(None);
        };
        if changes.formula_changed {
            return Ok(None);
        }
        let mut staged = tempfile::NamedTempFile::new()?;
        let mut writer = BufWriter::new(staged.as_file_mut());
        let ok = stream_supported(stream::xml_input(zip.by_name(part)?, part, |input| {
            stream::worksheet(input, &mut writer, sheet, &self.store, changes, &mut false)
        }))?;
        writer.flush()?;
        drop(writer);
        Ok(ok.then_some(staged))
    }

    fn original_xf(&self, sheet: &Sheet, key: CellRef, styles: &StyleTable) -> Option<(u16, u16)> {
        match self.styles.get(&key).copied() {
            Some(id) => Some((id, self.style_map.as_ref()?.editing_origin(id, styles)?)),
            None if !sheet.row_styles.contains_key(&key.row) && !sheet.col_styles.contains_key(&key.col) => Some((0, 0)),
            None => None,
        }
    }

    pub(super) fn has_style_map(&self) -> bool { self.style_map.is_some() }

    pub(super) fn set_style_map(&mut self, styles: Arc<StyleMap>) {
        self.style_map = Some(styles);
    }

    pub(super) fn set_clean_fast_meta(
        &mut self,
        clean_shell: Option<Arc<str>>,
        sst_arcs: Option<Arc<[Arc<str>]>>,
    ) {
        self.clean_shell = clean_shell;
        self.sst_arcs = sst_arcs;
    }

    pub(super) fn base_styles(&self) -> &CellStyles {
        &self.styles
    }

    pub(super) fn style_unchanged(&self, id: u16, current: &StyleTable) -> bool {
        self.style_map
            .as_ref()
            .is_none_or(|m| m.unchanged(id, current))
    }

    pub(super) fn existing_xf(&self, id: u16, current: &StyleTable) -> Option<u16> {
        let map = self.style_map.as_ref()?;
        if map.derived_from.contains_key(&id) {
            return None;
        }
        map.xf(id, current)
    }

    pub(super) fn trusted_signatures(
        &self,
    ) -> ((u32, u64), Option<(u32, u64)>, Option<(u32, u64)>) {
        (self.signature, self.strings, self.style_part)
    }

    pub(super) fn capture<R: std::io::Read + std::io::Seek>(
        zip: &mut ZipArchive<R>,
        part: &str,
        sheet: &Sheet,
    ) -> Option<Self> {
        let style_part = signature(zip, "xl/styles.xml");
        let style_map = sheet
            .source_cells
            .as_ref()
            .filter(|old| old.style_part == style_part)
            .and_then(|old| old.style_map.clone());
        let prior = sheet.source_cells.as_ref();
        let store = prior
            .filter(|s| &s.store.version == sheet.cell_version())
            .map(|s| s.store.clone())
            .unwrap_or_else(|| Arc::new(sheet.cells().clone()));
        let notes = NoteSource::capture(zip, part, prior.and_then(|s| s.notes.as_ref()))
            .ok()
            .filter(|next| {
                // An unrelated save may have copied another editor's notes verbatim.
                // Only adopt the new note baseline if it still describes our model.
                sheet.notes_dirty
                    || prior.is_none_or(|s| {
                        s.notes.as_ref().is_some_and(|old| {
                            (old.parts == next.parts && old.targets == next.targets)
                                || next.matches_legacy_notes(zip, sheet)
                        })
                    })
            });
        let strings = signature(zip, "xl/sharedStrings.xml");
        let keep_shell = prior
            .filter(|_| {
                !sheet.layout_dirty
                    && !sheet.freeze_dirty
                    && !sheet.filter_dirty
                    && !sheet.page_dirty
                    && !sheet.merges_dirty
                    && !sheet.col_styles_dirty
            })
            .and_then(|old| old.clean_shell.clone());
        let keep_sst_arcs = prior
            .filter(|old| old.strings == strings)
            .and_then(|old| old.sst_arcs.clone());
        Some(Self {
            layout: crate::patch::layout_xml::Baseline::capture(sheet),
            store,
            notes,
            signature: signature(zip, part)?,
            strings,
            style_part,
            style_map,
            styles: sheet.styles.clone(),
            row_styles: sheet.row_styles.clone(),
            col_styles: sheet.col_styles.clone(),
            rich: sheet.rich.clone(),
            arrays: Arc::new(sheet.arrays.clone()),
            spill_shapes: sheet.spill_shapes.clone(),
            spilled: sheet.spilled.clone(),
            clean_shell: keep_shell,
            sst_arcs: keep_sst_arcs,
        })
    }
}

#[derive(Clone, Debug, Default)]
pub(super) struct DeltaSummary {
    pub patched: bool,
    pub handled_layout: bool,
    pub handled_freeze: bool,
    pub handled_filter: bool,
    pub handled_page: bool,
    pub trusted_parts: HashMap<String, (u32, u64)>,
}

#[derive(Default)]
struct SharedFormulaState {
    promote_master: HashMap<CellRef, (u32, Rect)>,
    unshare: HashSet<CellRef>,
}

fn formula_range_rect(s: &str) -> Option<Rect> {
    let (a, b) = s.split_once(':').unwrap_or((s, s));
    let coord = |tok: &str| {
        let clean = if tok.contains('$') {
            std::borrow::Cow::Owned(tok.replace('$', ""))
        } else {
            std::borrow::Cow::Borrowed(tok)
        };
        crate::sparse::parse_ref(&clean)
            .filter(|&(r, c)| r < crate::sync::MAX_ROWS && c < crate::sync::MAX_COLS)
    };
    let (r1, c1) = coord(a)?;
    let (r2, c2) = coord(b)?;
    Some((r1.min(r2), c1.min(c2), r1.max(r2), c1.max(c2)))
}

struct Changes {
    keys: BTreeSet<CellRef>,
    bulk_dirty_count: usize,
    any_text_changed: bool,
    styles: StyleChanges,
    formula_changed: bool,
    layout: Option<crate::patch::layout_xml::Changes>,
    freeze_edit: bool,
    filter_xml: Option<String>,
    page_edit: bool,
}

impl Default for Changes {
    fn default() -> Self {
        Self {
            keys: BTreeSet::new(),
            bulk_dirty_count: 0,
            any_text_changed: false,
            styles: StyleChanges::default(),
            formula_changed: false,
            layout: None,
            freeze_edit: false,
            filter_xml: None,
            page_edit: false,
        }
    }
}

impl Changes {
    #[inline]
    fn dirty_count(&self) -> usize {
        self.bulk_dirty_count.max(self.keys.len())
    }

    fn ensure_keys_populated(&mut self, sheet: &Sheet, base: &Store) {
        if self.bulk_dirty_count > 0 && self.keys.is_empty() {
            let mut raw = Vec::with_capacity(self.bulk_dirty_count);
            raw.extend(sheet.cells().changed_keys(base));
            raw.extend(
                base.changed_keys(sheet.cells())
                    .filter(|k| !sheet.contains(k.row, k.col)),
            );
            raw.sort_unstable();
            self.keys = raw.into_iter().collect();
        }
    }
}

fn rich_changed(sheet: &Sheet, key: CellRef) -> bool {
    let k = (key.row, key.col);
    sheet.rich_dirty && sheet.source_cells.as_ref().is_some_and(|s| s.rich.get(&k) != sheet.rich.get(&k))
}

/// Excel's fixed array definition and footprint stay in the source XML.
/// A value in a tail is replaceable only while the evaluator owns it. Typing
/// over the same address removes that ownership, even for an equal number.
fn cache_only_arrays(sheet: &Sheet, source: &SourceCells, keys: &BTreeSet<CellRef>, editable_dynamic: bool) -> bool {
    if sheet.arrays != *source.arrays { return false; }
    if sheet.arrays.is_empty() && (editable_dynamic || (sheet.spilled.is_empty() && source.spilled.is_empty()
        && sheet.spill_shapes.is_empty() && source.spill_shapes.is_empty())) { return true; }
    let mut restricted = BTreeSet::new();
    if !editable_dynamic {
        restricted.extend(sheet.spilled.symmetric_difference(&source.spilled).copied());
        restricted.extend(keys.iter().filter(|k| sheet.spilled.contains(k) || sheet.spill_shapes.contains_key(k)).copied());
        for key in sheet.spill_shapes.keys().chain(source.spill_shapes.keys()) {
            if sheet.spill_shapes.get(key) != source.spill_shapes.get(key) { restricted.insert(*key); }
        }
    }
    for &(anchor,(top,left,bottom,right)) in &sheet.arrays {
        if top > bottom || left > right || bottom >= crate::sync::MAX_ROWS || right >= crate::sync::MAX_COLS || anchor != CellRef::new(top,left) { return false; }
        let rows = CellRef::new(top,0)..=CellRef::new(bottom,u32::MAX);
        let inside = |k: &&CellRef| (left..=right).contains(&k.col);
        let cache = |key: &CellRef| {
            let before_f = source.store.formula_of(key.row, key.col);
            let after_f = sheet.formula_of(key.row, key.col);
            if rich_changed(sheet,*key) { return false; }
            if *key == anchor {
                return before_f.is_some_and(|f| Some(f) == after_f);
            }
            sheet.spilled.contains(key) && before_f.is_none() && after_f.is_none()
        };
        let mut changed = keys.range(rows.clone()).filter(inside).peekable();
        let ownership: Vec<_> = restricted.range(rows).filter(inside).copied().collect();
        let old_shape = source.spill_shapes.get(&anchor);
        let shape = sheet.spill_shapes.get(&anchor);
        if changed.peek().is_none() && ownership.is_empty() && old_shape == shape { continue; }
        let expected = (bottom-top+1,right-left+1);
        if old_shape.is_some_and(|s| *s != expected) || shape.is_some_and(|s| *s != expected)
            || !cache(&anchor) || !changed.all(cache) || !ownership.iter().all(cache) {
            return false;
        }
        for k in ownership { restricted.remove(&k); }
    }
    restricted.is_empty()
}

#[inline]
fn same_opt_str(a: Option<&str>, b: Option<&str>) -> bool {
    match (a, b) {
        (None, None) => true,
        (Some(x), Some(y)) => (x.as_ptr() == y.as_ptr() && x.len() == y.len()) || x == y,
        _ => false,
    }
}

fn coordinates(
    sheet: &Sheet,
    source: &SourceCells,
    styles: &StyleTable,
    dynamic_index: Option<u32>,
    mut derive_xf: impl FnMut(u16, u16, u16) -> Option<u16>,
) -> Option<Changes> {
    let editable_dynamic = dynamic_index.is_some();
    let base = &source.store;
    let can_bulk_fast = source.clean_shell.is_some()
        && sheet.len() >= 32_768
        && !sheet.source_needs_dom_repair
        && !sheet.styles_dirty
        && sheet.style_edits.is_empty()
        && !sheet.row_styles_dirty
        && !sheet.col_styles_dirty
        && sheet.row_styles.is_empty()
        && sheet.col_styles.is_empty()
        && !sheet.merges_dirty
        && !sheet.rich_dirty
        && !sheet.layout_dirty
        && !sheet.freeze_dirty
        && !sheet.filter_dirty
        && !sheet.page_dirty
        && sheet.arrays.is_empty()
        && source.arrays.is_empty()
        && sheet.spill_shapes.is_empty()
        && source.spill_shapes.is_empty()
        && sheet.spilled.is_empty()
        && source.spilled.is_empty()
        && sheet.rich.is_empty()
        && source.rich.is_empty();
    let mut raw_keys = Vec::new();
    let mut bulk_dirty_count = 0usize;
    let mut formula_changed = false;
    let mut any_text_changed = false;
    let track_text = source.sst_arcs.is_some();
    for key in sheet.cells().changed_keys(base) {
        if !formula_changed {
            let bf = base.formula_of(key.row, key.col);
            let sf = sheet.formula_of(key.row, key.col);
            formula_changed |= !same_opt_str(bf, sf);
        }
        if track_text && !any_text_changed {
            any_text_changed = matches!(sheet.value(key.row, key.col), crate::model::Value::Text(_))
                || base
                    .get(key.row, key.col)
                    .is_some_and(|v| matches!(v.value, crate::model::Value::Text(_)));
        }
        if can_bulk_fast && raw_keys.len() > 4_096 {
            if bulk_dirty_count == 0 {
                bulk_dirty_count = raw_keys.len() + 1;
                raw_keys.clear();
                raw_keys.shrink_to_fit();
            } else {
                bulk_dirty_count += 1;
            }
        } else if bulk_dirty_count > 0 {
            bulk_dirty_count += 1;
        } else {
            raw_keys.push(key);
        }
    }
    for key in base
        .changed_keys(sheet.cells())
        .filter(|k| !sheet.contains(k.row, k.col))
    {
        formula_changed |= base.formula_of(key.row, key.col).is_some();
        if track_text && !any_text_changed {
            any_text_changed = base
                .get(key.row, key.col)
                .is_some_and(|v| matches!(v.value, crate::model::Value::Text(_)));
        }
        if bulk_dirty_count > 0 {
            bulk_dirty_count += 1;
        } else if can_bulk_fast && raw_keys.len() > 4_096 {
            bulk_dirty_count = raw_keys.len() + 1;
            raw_keys.clear();
            raw_keys.shrink_to_fit();
        } else {
            raw_keys.push(key);
        }
    }
    let mut keys: BTreeSet<CellRef> = if bulk_dirty_count > 0 {
        BTreeSet::new()
    } else {
        raw_keys.sort_unstable();
        raw_keys.into_iter().collect()
    };
    if sheet.rich_dirty {
        for &(row, col) in sheet.rich.keys().chain(source.rich.keys()) {
            let key = CellRef::new(row, col);
            if rich_changed(sheet, key) { keys.insert(key); }
        }
    }
    if !cache_only_arrays(sheet,source,&keys,editable_dynamic) { return None; }
    let mut changed_styles = StyleChanges {dynamic_index,..Default::default()};
    if sheet.styles_dirty || !sheet.style_edits.is_empty() {
        for key in sheet
            .styles
            .changed_keys(&source.styles)
            .chain(source.styles.changed_keys(&sheet.styles))
        {
            let value = match sheet.explicit_style_of(key.row, key.col) {
                Some(id) => {
                    let map = source.style_map.as_ref()?;
                    if let Some(xf) = map.xf(id, styles) {
                        if let Some(origin) = map.derived_from.get(&id) {
                            let (_, raw) = source.original_xf(sheet, key, styles)?;
                            if *origin != Some(raw) && raw != xf {
                                return None;
                            }
                            changed_styles.sources.insert(key, raw);
                        }
                        Some(xf)
                    }
                    else {
                        if map.xfs.contains_key(&id) { return None; }
                        let (old, raw) = source.original_xf(sheet, key, styles)?;
                        if !map.unchanged(old, styles)
                            || styles.diag_of(id) != styles.diag_of(old)
                            || styles.quote_of(id) != styles.quote_of(old) { return None; }
                        let xf = derive_xf(raw, old, id)?;
                        changed_styles.sources.insert(key, raw);
                        Some(xf)
                    }
                }
                None => None,
            };
            changed_styles.updates.insert(key, value);
            keys.insert(key);
        }
    }
    // Axis defaults need package XF numbers, not CELLAN's interned IDs.
    // Keep the model sparse; only occupied inheriting cells also receive an s.
    for (dirty, prior, current, axis) in [
        (sheet.row_styles_dirty, &source.row_styles, &sheet.row_styles, &mut changed_styles.rows),
        (sheet.col_styles_dirty, &source.col_styles, &sheet.col_styles, &mut changed_styles.cols),
    ] {
        if !dirty { continue; }
        for pos in prior.keys().chain(current.keys()) {
            let old = prior.get(pos).copied();
            let new = current.get(pos).copied();
            if old == new || axis.contains_key(pos) { continue; }
            let xf = match new {
                None => None,
                Some(id) => {
                    let map = source.style_map.as_ref()?;
                    let original = || match old {
                        Some(old) => map.xf(old,styles),
                        None if map.xfs.get(&0).is_none_or(|v| *v == Some(0)) => Some(0),
                        None => None,
                    };
                    if let Some(xf) = map.xf(id,styles) {
                        if let Some(origin) = map.derived_from.get(&id) {
                            let base = original()?;
                            if *origin != Some(base) && xf != base { return None; }
                        }
                        Some(xf)
                    } else {
                        if map.xfs.contains_key(&id) { return None; }
                        let raw = original()?;
                        let old = old.unwrap_or(0);
                        if !map.unchanged(old,styles)
                            || styles.diag_of(id) != styles.diag_of(old)
                            || styles.quote_of(id) != styles.quote_of(old) { return None; }
                        changed_styles.derived_axis = true;
                        Some(derive_xf(raw,old,id)?)
                    }
                }
            };
            axis.insert(*pos,xf);
        }
    }
    if !changed_styles.rows.is_empty() || !changed_styles.cols.is_empty() {
        keys.extend(sheet.iter().filter_map(|(key,_)| {
            (sheet.explicit_style_of(key.row,key.col).is_none() &&
                (changed_styles.rows.contains_key(&key.row) || changed_styles.cols.contains_key(&key.col)))
                .then_some(key)
        }));
    }
    for &key in &keys {
        if !sheet.contains(key.row,key.col) || sheet.explicit_style_of(key.row,key.col).is_some() { continue; }
        let inherited = if let Some(&id) = sheet.row_styles.get(&key.row) {
            Some((id,changed_styles.rows.get(&key.row)))
        } else { sheet.col_styles.get(&key.col).map(|&id| (id,changed_styles.cols.get(&key.col))) };
        if let Some((id,update)) = inherited {
            let xf = match update { Some(Some(xf)) => *xf, _ => source.style_map.as_ref()?.xf(id,styles)? };
            changed_styles.updates.insert(key,Some(xf));
        } else if changed_styles.rows.contains_key(&key.row) || changed_styles.cols.contains_key(&key.col) {
            changed_styles.updates.insert(key,None);
        }
    }
    if editable_dynamic {
        let fixed: HashSet<_> = sheet.arrays.iter().map(|(k,_)| k).collect();
        for &key in source.spill_shapes.keys().chain(sheet.spill_shapes.keys()) {
            if fixed.contains(&key) { continue; }
            let before = source.spill_shapes.get(&key);
            let after = sheet.spill_shapes.get(&key);
            if before != after || keys.contains(&key) {
                keys.insert(key);
                let rect = |shape: Option<&(u32,u32)>| match shape {
                    Some(&s) => Some(Some(super::dynamic::area(key,s)?)),
                    None => Some(None),
                };
                changed_styles.dynamic.insert(key, (rect(before)?,rect(after)?));
            }
        }
    }
    Some(Changes {
        keys,
        bulk_dirty_count,
        any_text_changed,
        styles: changed_styles,
        formula_changed,
        ..Default::default()
    })
}

fn attr<'a>(e: &'a BytesStart<'a>, key: &str) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == key)
        .map(|a| a.value.into_owned())
}

pub(super) fn safe_xml_text(text: &str) -> bool {
    !(text.chars().any(|c| c < ' ' && c != '\t' && c != '\n')
        // A literal _xHHHH_ requires SpreadsheetML-specific escaping; ordinary
        // identifiers such as _xlfn.XLOOKUP contain no encoded character.
        || text.as_bytes().windows(7).any(|w| w.starts_with(b"_x") && w[6] == b'_' && w[2..6].iter().all(u8::is_ascii_hexdigit))
        || text.contains('\u{fffe}')
        || text.contains('\u{ffff}'))
}

fn xml_text(text: &str) -> Option<String> {
    safe_xml_text(text).then(|| quick_xml::escape::escape(text).into_owned())
}

/// Leave Excel's formula definition intact on cache-only writes, including
/// shared formula anchors and derived cells. Preserve the explicit style ID.
#[allow(clippy::too_many_arguments)]
fn replacement(
    key: CellRef,
    start: &BytesStart<'_>,
    body: &str,
    old: Option<&Cell>,
    new: Option<&Cell>,
    styles: &StyleChanges,
    sheet: &Sheet,
    base: &Store,
    shared_state: &mut SharedFormulaState,
    extra_keys: &mut Vec<CellRef>,
) -> Option<String> {
    let mut style = None;
    let shapes = styles.dynamic.get(&key);
    let mut cm = None;
    for a in start.attributes() {
        let a = a.ok()?;
        match a.key.as_ref() {
            "r" | "t" => {}
            "s" => style = Some(a.value.parse::<u32>().ok()?),
            "cm" => cm = Some(a.value.parse::<u32>().ok()?),
            _ => return None,
        }
    }
    if cm != shapes.and_then(|(old,_)| old.and(styles.dynamic_index)) { return None; }
    if !styles.matches(key, style) { return None; }
    if let Some(&xf) = styles.updates.get(&key) {
        style = xf.map(u32::from);
    }
    let same_formula = old.and_then(|c| c.formula.as_ref()) == new.and_then(|c| c.formula.as_ref());
    let mut definition: Option<std::borrow::Cow<'_, str>> = None;
    let mut saw_formula_tag = false;
    let body_bytes = body.as_bytes();
    let fast_body_ok = if body_bytes.is_empty() {
        true
    } else if body_bytes.starts_with(b"<v>") && body_bytes.ends_with(b"</v>") && body_bytes.len() >= 7 {
        super::sheets::stream::simd_memchr(b'<', &body_bytes[3..body_bytes.len() - 4]).is_none()
    } else if body_bytes.starts_with(b"<f>") && shapes.is_none() {
        if let Some(rel) = super::sheets::stream::simd_memchr(b'<', &body_bytes[3..]) {
            let f_close = 3 + rel;
            if body_bytes[f_close..].starts_with(b"</f>") {
                let after_f = &body_bytes[f_close + 4..];
                let v_ok = after_f.is_empty()
                    || (after_f.starts_with(b"<v>")
                        && after_f.ends_with(b"</v>")
                        && after_f.len() >= 7
                        && super::sheets::stream::simd_memchr(
                            b'<',
                            &after_f[3..after_f.len() - 4],
                        )
                        .is_none());
                if v_ok {
                    if old.and_then(|c| c.formula.as_ref()).is_none() {
                        return None;
                    }
                    saw_formula_tag = true;
                    if same_formula {
                        definition = Some(std::borrow::Cow::Borrowed(&body[..f_close + 4]));
                    }
                    true
                } else {
                    false
                }
            } else {
                false
            }
        } else {
            false
        }
    } else {
        false
    };
    if !fast_body_ok {
        let mut xml = Reader::from_str(body);
        loop {
            let offset = xml.buffer_position() as usize;
            let event = xml.read_event().ok()?;
            match event {
                Event::Start(ref e) | Event::Empty(ref e) => {
                    let tag = e.name();
                    if !matches!(tag.as_ref(), "f" | "v" | "is") {
                        return None;
                    }
                    if matches!(event, Event::Start(_)) {
                        xml.read_to_end(tag).ok()?;
                    }
                    if tag.as_ref() == "f" {
                        saw_formula_tag = true;
                        let raw_def = &body[offset..xml.buffer_position() as usize];
                        // A what-if table anchor, for example, has an OOXML formula
                        // definition even though the model only knows its cached number.
                        if old.and_then(|c| c.formula.as_ref()).is_none() {
                            return None;
                        }
                        if let Some((Some(rect),_)) = shapes {
                            if attr(e,"t").as_deref() != Some("array") || crate::arr::parse_a1_range(&attr(e,"ref")?)? != *rect
                                || e.attributes().count() != 2 { return None; }
                            definition = Some(std::borrow::Cow::Borrowed(raw_def));
                        } else if shapes.is_none() && attr(e, "t").as_deref() == Some("shared") {
                            for a in e.attributes() {
                                let a = a.ok()?;
                                if !matches!(a.key.as_ref(), "t" | "si" | "ref") {
                                    return None;
                                }
                            }
                            let si: u32 = attr(e, "si")?.parse().ok()?;
                            if let Some(ref_str) = attr(e, "ref") {
                                if same_formula {
                                    definition = Some(std::borrow::Cow::Borrowed(raw_def));
                                } else {
                                    let rect = formula_range_rect(&ref_str)?;
                                    if (rect.0, rect.1) != (key.row, key.col) {
                                        return None;
                                    }
                                    if rect.0 == rect.2 || rect.1 == rect.3 {
                                        // 1D shared formula range: promote the next untouched child in `rect` to master.
                                        let mut promoted = None;
                                        for rr in rect.0..=rect.2 {
                                            for cc in rect.1..=rect.3 {
                                                if (rr, cc) <= (key.row, key.col) {
                                                    continue;
                                                }
                                                let b_f = base.formula_of(rr, cc);
                                                let s_f = sheet.formula_of(rr, cc);
                                                if b_f.is_some() && b_f == s_f {
                                                    promoted = Some(CellRef::new(rr, cc));
                                                    break;
                                                }
                                            }
                                            if promoted.is_some() {
                                                break;
                                            }
                                        }
                                        if let Some(nk) = promoted {
                                            shared_state
                                                .promote_master
                                                .insert(nk, (si, (nk.row, nk.col, rect.2, rect.3)));
                                            extra_keys.push(nk);
                                        }
                                    } else {
                                        let area = u64::from(rect.2 - rect.0 + 1)
                                            .saturating_mul(u64::from(rect.3 - rect.1 + 1));
                                        if area > 16_384 {
                                            return None;
                                        }
                                        for rr in rect.0..=rect.2 {
                                            for cc in rect.1..=rect.3 {
                                                if (rr, cc) <= (key.row, key.col) {
                                                    continue;
                                                }
                                                let b_f = base.formula_of(rr, cc);
                                                let s_f = sheet.formula_of(rr, cc);
                                                if b_f.is_some() && b_f == s_f {
                                                    let ck = CellRef::new(rr, cc);
                                                    shared_state.unshare.insert(ck);
                                                    extra_keys.push(ck);
                                                }
                                            }
                                        }
                                    }
                                    definition = None;
                                }
                            } else if let Some((want_si, new_rect)) = shared_state.promote_master.remove(&key) {
                                if si != want_si || !same_formula {
                                    return None;
                                }
                                let formula = new?.formula.as_deref()?;
                                let f = crate::add_fn_prefixes(formula.strip_prefix('=').unwrap_or(formula));
                                definition = Some(std::borrow::Cow::Owned(format!(
                                    "<f t=\"shared\" ref=\"{}\" si=\"{si}\">{}</f>",
                                    super::dynamic::reference(new_rect),
                                    xml_text(&f)?
                                )));
                            } else if shared_state.unshare.remove(&key) {
                                if !same_formula {
                                    return None;
                                }
                                definition = None;
                            } else if same_formula {
                                definition = Some(std::borrow::Cow::Borrowed(raw_def));
                            } else {
                                // Child shared formula edited or cleared: standalone replacement needs no master rewrite.
                                definition = None;
                            }
                        } else if (shapes.is_some() || !same_formula)
                            && e.attributes().next().is_some()
                        {
                            return None;
                        } else if same_formula {
                            definition = Some(std::borrow::Cow::Borrowed(raw_def));
                        }
                    }
                }
                Event::Text(t) if t.as_ref().trim().is_empty() => {}
                Event::Eof => break,
                _ => return None,
            }
        }
    }
    if shared_state.promote_master.contains_key(&key) || shared_state.unshare.contains(&key) {
        return None;
    }
    let mut ref_buf = [0u8; 16];
    let reference = super::fresh::format_a1_buf(key.row, key.col, &mut ref_buf);
    let mut out = String::with_capacity(64);
    out.push_str("<c r=\"");
    out.push_str(reference);
    out.push('"');
    if let Some(style) = style {
        use std::fmt::Write as _;
        let _ = write!(out, " s=\"{style}\"");
    }

    let Some(cell) = new else {
        return Some(if style.is_some() {
            format!("{out}/>")
        } else {
            String::new()
        });
    };
    if let Some((old,new)) = shapes {
        if old.is_some() && !saw_formula_tag { return None; }
        if let Some(rect) = new {
            out.push_str(&format!(" cm=\"{}\"", styles.dynamic_index?));
            let formula = cell.formula.as_deref()?;
            let f = crate::add_fn_prefixes(formula.strip_prefix('=').unwrap_or(formula));
            let f = format!("<f t=\"array\" ref=\"{}\">{}</f>",super::dynamic::reference(*rect),xml_text(&f)?);
            if cell.value == Value::Error(crate::formula::ErrKind::Spill) {
                return Some(format!("{out} t=\"e\">{f}<v>#SPILL!</v></c>"));
            }
            return model_cell_xml(out, cell, Some(&f));
        }
        return cell_with_runs(out,cell,None,sheet.rich.get(&(key.row,key.col)).map(Vec::as_slice));
    }
    if same_formula && cell.formula.is_some() && !saw_formula_tag {
        return None;
    }
    cell_with_runs(out, cell, definition.as_deref(), sheet.rich.get(&(key.row, key.col)).map(Vec::as_slice))
}

pub(super) fn supports_runs(cell: &Cell, runs: &[crate::richtext::RichRun]) -> bool {
    let Value::Text(text) = &cell.value else { return false; };
    cell.formula.is_none() && runs.iter().flat_map(|r| r.text.bytes()).eq(text.bytes())
        && runs.iter().all(|r| safe_xml_text(&r.text) && r.font.as_ref().is_none_or(|f| safe_xml_text(f))
            && r.size.is_none_or(|v| v.is_finite() && v > 0.0))
}

pub(super) fn cell_with_runs(out: String, cell: &Cell, original_formula: Option<&str>, runs: Option<&[crate::richtext::RichRun]>) -> Option<String> {
    let Some(runs) = runs.filter(|r| !r.is_empty()) else {
        return model_cell_xml(out,cell,original_formula);
    };
    if !supports_runs(cell,runs) { return None; }
    Some(format!("{out} t=\"inlineStr\">{}</c>", crate::richtext::runs_to_inline(runs)))
}

pub(super) fn model_cell_xml(mut out: String, cell: &Cell, original_formula: Option<&str>) -> Option<String> {
    match &cell.value {
        Value::Empty => {}
        Value::Number(n) if n.is_finite() => {}
        Value::Number(_) => return None,
        Value::Bool(_) => out.push_str(" t=\"b\""),
        Value::Error(crate::formula::ErrKind::Spill) => return None,
        Value::Error(_) => out.push_str(" t=\"e\""),
        Value::Text(s) if cell.formula.is_some() => {
            if !safe_xml_text(s) {
                return None;
            }
            out.push_str(" t=\"str\"");
        }
        Value::Text(s) => {
            if !safe_xml_text(s) {
                return None;
            }
            out.push_str(" t=\"inlineStr\"");
        }
    }
    out.push('>');
    if let Some(f) = &cell.formula {
        if let Some(definition) = original_formula {
            if definition.is_empty() {
                return None;
            }
            out.push_str(definition);
        } else {
            let f = crate::add_fn_prefixes(f.strip_prefix('=').unwrap_or(f));
            let esc = xml_text(&f)?;
            out.push_str("<f>");
            out.push_str(&esc);
            out.push_str("</f>");
        }
    }
    match &cell.value {
        Value::Empty => {}
        Value::Number(n) => {
            out.push_str("<v>");
            // SAFETY: `push_f64_xml` only appends valid ASCII decimal bytes.
            super::fresh::push_f64_xml(unsafe { out.as_mut_vec() }, *n);
            out.push_str("</v>");
        }
        Value::Bool(b) => {
            out.push_str(if *b { "<v>1</v>" } else { "<v>0</v>" });
        }
        Value::Error(e) => {
            out.push_str("<v>");
            out.push_str(e.text());
            out.push_str("</v>");
        }
        Value::Text(s) if cell.formula.is_some() => {
            out.push_str("<v>");
            out.push_str(&quick_xml::escape::escape(s.as_ref()));
            out.push_str("</v>");
        }
        Value::Text(s) => {
            out.push_str("<is><t xml:space=\"preserve\">");
            out.push_str(&quick_xml::escape::escape(s.as_ref()));
            out.push_str("</t></is>");
        }
    }
    out.push_str("</c>");
    Some(out)
}

/// Keep unchanged XML as borrowed slices so even a large edited sheet only
/// needs one decompressed representation in memory.
struct Edit {
    from: usize,
    to: usize,
    text: String,
}

fn new_cell(key: CellRef, sheet: &Sheet, base: &Store, styles: &StyleChanges) -> Option<String> {
    if base.get(key.row, key.col).is_some() {
        return None;
    }
    let mut dummy_shared = SharedFormulaState::default();
    let mut dummy_extra = Vec::new();
    let cur = sheet.get(key.row, key.col);
    replacement(
        key,
        &BytesStart::new("c"),
        "",
        None,
        cur.as_deref(),
        styles,
        sheet,
        base,
        &mut dummy_shared,
        &mut dummy_extra,
    )
}

fn expanded_tag(tag: &BytesStart<'_>, omit: &str) -> Option<String> {
    let mut root = BytesStart::new(tag.name().as_ref().to_owned());
    for a in tag.attributes() {
        let a = a.ok()?;
        if a.key.as_ref() != omit {
            root.push_attribute(a);
        }
    }
    let mut w = quick_xml::Writer::new(Vec::new());
    w.write_event(Event::Start(root)).ok()?;
    String::from_utf8(w.into_inner()).ok()
}

#[allow(clippy::too_many_arguments)]
fn row_edits(
    xml: &str,
    sheet: &Sheet,
    base: &Store,
    mut keys: BTreeSet<CellRef>,
    styles: &StyleChanges,
    changed_sst: &mut bool,
    shared_state: &mut SharedFormulaState,
    future_keys: &mut BTreeSet<CellRef>,
) -> Option<Vec<Edit>> {
    let mut reader = Reader::from_str(xml);
    let mut edits = Vec::new();
    let mut row = None;
    loop {
        let from = reader.buffer_position() as usize;
        let event = reader.read_event().ok()?;
        let to = reader.buffer_position() as usize;
        match event {
            Event::Start(ref e) | Event::Empty(ref e) if e.name().as_ref() == "row" => {
                let r = attr(e, "r")?.parse::<u32>().ok()?.checked_sub(1)?;
                if keys.first().is_none_or(|key| key.row != r) { return None; }
                if matches!(event, Event::Empty(_)) {
                    let mut text = expanded_tag(e, "spans")?;
                    while let Some(k) = keys.first().copied().filter(|k| k.row == r) {
                        if shared_state.promote_master.contains_key(&k) || shared_state.unshare.contains(&k) {
                            return None;
                        }
                        text.push_str(&new_cell(k, sheet, base, styles)?);
                        keys.pop_first();
                    }
                    text.push_str("</row>");
                    edits.push(Edit { from, to, text });
                } else {
                    if attr(e, "spans").is_some() {
                        edits.push(Edit {
                            from,
                            to,
                            text: expanded_tag(e, "spans")?,
                        });
                    }
                    row = Some(r);
                }
            }
            Event::End(e) if row.is_some() && e.name().as_ref() == "row" => {
                let mut text = String::new();
                while let Some(k) = keys.first().copied().filter(|k| Some(k.row) == row) {
                    if shared_state.promote_master.contains_key(&k) || shared_state.unshare.contains(&k) {
                        return None;
                    }
                    text.push_str(&new_cell(k, sheet, base, styles)?);
                    keys.pop_first();
                }
                if !text.is_empty() {
                    edits.push(Edit {
                        from,
                        to: from,
                        text,
                    });
                }
                row = None;
            }
            Event::Start(ref e) | Event::Empty(ref e)
                if row.is_some() && e.name().as_ref() == "c" =>
            {
                let (r, c) = crate::sparse::parse_ref(&attr(e, "r")?)?;
                if Some(r) != row {
                    return None;
                }
                let mut text = String::new();
                let key = CellRef::new(r, c);
                while let Some(k) = keys.first().copied().filter(|k| *k < key) {
                    if shared_state.promote_master.contains_key(&k) || shared_state.unshare.contains(&k) {
                        return None;
                    }
                    text.push_str(&new_cell(k, sheet, base, styles)?);
                    keys.pop_first();
                }
                if !text.is_empty() {
                    edits.push(Edit {
                        from,
                        to: from,
                        text,
                    });
                }
                let body = if matches!(event, Event::Start(_)) {
                    let range = reader.read_to_end(e.name()).ok()?;
                    &xml[range.start as usize..range.end as usize]
                } else {
                    ""
                };
                if keys.remove(&key) {
                    let old = base.get(r, c);
                    let new = sheet.get(r, c);
                    let needs_shared_rewrite = shared_state.promote_master.contains_key(&key)
                        || shared_state.unshare.contains(&key);
                    let text = if let Some(xf) = styles
                        .updates
                        .get(&key)
                        .filter(|_| !needs_shared_rewrite && old == new && !rich_changed(sheet, key))
                    {
                        let old_style = attr(e, "s").map(|value| value.parse::<u32>()).transpose().ok()?;
                        if !styles.matches(key, old_style) { return None; }
                        // Style-only changes leave rich strings, metadata, shared formulas
                        // and unknown children of the original cell byte-for-byte intact.
                        let mut tag = expanded_tag(e, "s")?;
                        tag.pop();
                        if let Some(xf) = xf {
                            tag.push_str(&format!(" s=\"{xf}\""));
                        }
                        if matches!(event, Event::Empty(_)) {
                            tag.push_str("/>");
                        } else {
                            tag.push('>');
                            tag.push_str(body);
                            tag.push_str("</c>");
                        }
                        tag
                    } else {
                        *changed_sst |= attr(e, "t").as_deref() == Some("s");
                        let mut extra = Vec::new();
                        let rep = replacement(
                            key,
                            e,
                            body,
                            old.as_deref(),
                            new.as_deref(),
                            styles,
                            sheet,
                            base,
                            shared_state,
                            &mut extra,
                        )?;
                        for nk in extra {
                            if nk.row == r {
                                if nk <= key {
                                    return None;
                                }
                                keys.insert(nk);
                            } else if nk.row > r {
                                future_keys.insert(nk);
                            } else {
                                return None;
                            }
                        }
                        rep
                    };
                    let to = reader.buffer_position() as usize;
                    if let Some(previous) = edits.last_mut().filter(|e| e.to == from) {
                        previous.text.push_str(&text);
                        previous.to = to;
                    } else {
                        edits.push(Edit { from, to, text });
                    }
                }
            }
            // Unknown row/sheetData children have their own ordering rules.
            Event::Start(_) | Event::Empty(_) => return None,
            Event::Eof => break,
            _ => {}
        }
        if keys.is_empty() {
            break;
        }
    }
    keys.is_empty().then_some(edits)
}

fn write_spans(out: &mut impl Write, xml: &str, edits: &[Edit]) -> Result<()> {
    let mut copied = 0;
    for e in edits {
        out.write_all(&xml.as_bytes()[copied..e.from])?;
        out.write_all(e.text.as_bytes())?;
        copied = e.to;
    }
    out.write_all(&xml.as_bytes()[copied..])?;
    Ok(())
}

/// Direct children and their original XML byte spans. Keep markup outside a
/// replacement unchanged, including namespaces, extension lists and comments.
struct Child<'a> {
    tag: BytesStart<'a>,
    from: usize,
    to: usize,
}

fn children<'a>(xml: &'a str, root_name: &str) -> Option<(Vec<Child<'a>>, usize)> {
    let mut reader = Reader::from_str(xml);
    let mut root = None;
    let mut parts = Vec::new();
    loop {
        let from = reader.buffer_position() as usize;
        match reader.read_event().ok()? {
            Event::Start(e) if root.is_none() => {
                if e.local_name().as_ref() != root_name {
                    return None;
                }
                root = Some(e.name().as_ref().to_string());
            }
            ev @ (Event::Start(_) | Event::Empty(_)) if root.is_some() => {
                let empty = matches!(&ev, Event::Empty(_));
                let tag = match ev {
                    Event::Empty(tag) | Event::Start(tag) => tag,
                    _ => unreachable!(),
                };
                if !empty {
                    reader.read_to_end(tag.name()).ok()?;
                }
                parts.push(Child {
                    tag,
                    from,
                    to: reader.buffer_position() as usize,
                });
            }
            Event::End(e) if root.as_deref() == Some(e.name().as_ref()) => {
                return Some((parts, from));
            }
            Event::Eof => return None,
            _ => {}
        }
    }
}

fn merge_edit(xml: &str, sheet: &Sheet) -> Option<Edit> {
    let (parts, end) = children(xml, "worksheet")?;
    // We emit unprefixed worksheet children only in an unprefixed worksheet.
    let sheet_data = parts
        .iter()
        .position(|e| e.tag.name().as_ref() == "sheetData")?;
    let mut ranges: BTreeSet<_> = sheet.merges.iter().copied().collect();
    if ranges.len() != sheet.merges.len()
        || ranges
            .iter()
            .any(|&(r, c, z, d)| r > z || c > d || z >= 1_048_576 || d >= 16_384)
    {
        return None;
    }
    let mut merged = String::new();
    let mut span = None;
    if let Some(existing) = parts
        .iter()
        .find(|e| e.tag.local_name().as_ref() == "mergeCells")
    {
        if existing.tag.name().as_ref() != "mergeCells"
            || existing
                .tag
                .attributes()
                .any(|a| a.map_or(true, |a| a.key.as_ref() != "count"))
        {
            return None;
        }
        span = Some((existing.from, existing.to));
        if !xml[existing.from..existing.to].trim_end().ends_with("/>") {
            let source = &xml[existing.from..existing.to];
            for el in children(source, "mergeCells")?.0 {
                if el.tag.name().as_ref() != "mergeCell" {
                    return None;
                }
                let rect = crate::arr::parse_a1_range(&attr(&el.tag, "ref")?)?;
                if ranges.remove(&rect) {
                    merged.push_str(&source[el.from..el.to]);
                }
            }
        }
    }
    for rect in ranges {
        let overlaps =
            |(r, c, z, d): Rect| r <= rect.2 && rect.0 <= z && c <= rect.3 && rect.1 <= d;
        // Excel cannot merge a portion of an array or its dynamic spill.
        if sheet.arrays.iter().any(|(_, area)| overlaps(*area))
            || sheet.spill_shapes.iter().any(|(k, &(h, w))| {
                overlaps((
                    k.row,
                    k.col,
                    k.row.saturating_add(h.saturating_sub(1)),
                    k.col.saturating_add(w.saturating_sub(1)),
                ))
            })
            || sheet
                .spilled
                .iter()
                .any(|k| overlaps((k.row, k.col, k.row, k.col)))
        {
            return None;
        }
        merged.push_str(&format!(
            "<mergeCell ref=\"{}\"/>",
            crate::arr::a1_range(rect)
        ));
    }
    if !merged.is_empty() {
        merged = format!(
            "<mergeCells count=\"{}\">{merged}</mergeCells>",
            sheet.merges.len()
        );
    }
    let (from, to) = span.unwrap_or_else(|| {
        // CT_Worksheet: mergeCells follows customSheetViews and precedes phoneticPr.
        let before = parts
            .iter()
            .skip(sheet_data + 1)
            .find(|e| {
                !matches!(
                    e.tag.local_name().as_ref(),
                    "sheetCalcPr"
                        | "sheetProtection"
                        | "protectedRanges"
                        | "scenarios"
                        | "autoFilter"
                        | "sortState"
                        | "dataConsolidate"
                        | "customSheetViews"
                )
            })
            .map_or(end, |e| e.from);
        (before, before)
    });
    Some(Edit {
        from,
        to,
        text: merged,
    })
}

fn replace_spans(xml: &str, edits: impl IntoIterator<Item = Edit>) -> Vec<u8> {
    let mut out = Vec::with_capacity(xml.len());
    let mut last = 0;
    for edit in edits {
        out.extend_from_slice(&xml.as_bytes()[last..edit.from]);
        out.extend_from_slice(edit.text.as_bytes());
        last = edit.to;
    }
    out.extend_from_slice(&xml.as_bytes()[last..]);
    out
}

type PackageParts = HashMap<String, Vec<u8>>;

pub(super) fn invalidate_chain(
    zip: &mut ZipArchive<File>,
    manual: bool,
) -> Result<Option<(PackageParts, HashSet<String>)>> {
    const RELS: &str = "xl/_rels/workbook.xml.rels";
    const TYPES: &str = "[Content_Types].xml";
    const BOOK: &str = "xl/workbook.xml";
    let rels = super::read_zip_text(zip, RELS)?;
    let Some((children_rels, _)) = children(&rels, "Relationships") else {
        return Ok(None);
    };
    let mut deleted = HashSet::new();
    let mut replacements = PackageParts::new();
    let mut edits = vec![];
    for c in children_rels {
        if c.tag.local_name().as_ref() == "Relationship"
            && attr(&c.tag, "Type").is_some_and(|s| s.ends_with("/calcChain"))
        {
            let Some(target) = attr(&c.tag, "Target").filter(|t| !t.contains('&')) else {
                return Ok(None);
            };
            if attr(&c.tag, "TargetMode").is_some_and(|m| m != "Internal") {
                return Ok(None);
            }
            let part = crate::fixups::resolve_target(RELS, &target);
            if zip.index_for_name(&part).is_none() {
                return Ok(None);
            }
            deleted.insert(part);
            edits.push(Edit {
                from: c.from,
                to: c.to,
                text: String::new(),
            });
        }
    }
    if zip.index_for_name("xl/calcChain.xml").is_some() && !deleted.contains("xl/calcChain.xml") {
        return Ok(None);
    }
    if !edits.is_empty() {
        replacements.insert(RELS.into(), replace_spans(&rels, edits));
        let xml = super::read_zip_text(zip, TYPES)?;
        let Some((entries, _)) = children(&xml, "Types") else {
            return Ok(None);
        };
        let mut removed = HashSet::new();
        let mut edits = Vec::new();
        for c in entries {
            if c.tag.local_name().as_ref() == "Override"
                && let Some(part) = attr(&c.tag, "PartName")
            {
                let part = part.trim_start_matches('/');
                if deleted.contains(part) {
                    if !attr(&c.tag, "ContentType").is_some_and(|s| s.ends_with(".calcChain+xml")) {
                        return Ok(None);
                    }
                    removed.insert(part.to_string());
                    edits.push(Edit {
                        from: c.from,
                        to: c.to,
                        text: String::new(),
                    });
                }
            }
        }
        if removed != deleted {
            return Ok(None);
        }
        replacements.insert(TYPES.into(), replace_spans(&xml, edits));
    }
    let xml = super::read_zip_text(zip, BOOK)?;
    let Some(updated) = recalc_on_open(&xml,manual)? else { return Ok(None); };
    replacements.insert(BOOK.into(), updated);
    Ok(Some((replacements, deleted)))
}

/// Mark the persisted caches for Excel without discarding a still-valid dependency chain.
pub(super) fn recalc_on_open(xml: &str, manual: bool) -> Result<Option<Vec<u8>>> {
    let Some((parts, close)) = children(&xml, "workbook") else {
        return Ok(None);
    };
    let mut tag = BytesStart::new("calcPr");
    let mut empty = true;
    let mut from = close;
    let mut to = close;
    if let Some(e) = parts
        .iter()
        .find(|p| p.tag.local_name().as_ref() == "calcPr")
    {
        tag = BytesStart::new(e.tag.name().as_ref().to_owned());
        for a in e.tag.attributes() {
            let a = a?;
            if a.key.as_ref() != "calcId" && (manual || a.key.as_ref() != "fullCalcOnLoad") {
                tag.push_attribute(a);
            }
        }
        let mut reader = Reader::from_str(&xml[e.from..e.to]);
        empty = matches!(reader.read_event()?, Event::Empty(_));
        from = e.from;
        to = e.from + reader.buffer_position() as usize;
    } else {
        let Some(sheets) = parts
            .iter()
            .find(|p| p.tag.local_name().as_ref() == "sheets")
        else {
            return Ok(None);
        };
        if let Some((prefix, _)) = sheets.tag.name().as_ref().split_once(':') {
            tag = BytesStart::new(format!("{prefix}:calcPr"));
        }
        if let Some(next) = parts.iter().find(|p| {
            matches!(
                p.tag.local_name().as_ref(),
                "oleSize"
                    | "customWorkbookViews"
                    | "pivotCaches"
                    | "smartTagPr"
                    | "smartTagTypes"
                    | "webPublishing"
                    | "fileRecoveryPr"
                    | "webPublishObjects"
                    | "extLst"
            )
        }) {
            from = next.from;
            to = from;
        }
        if manual {
            tag.push_attribute(("calcMode", "manual"));
        }
    }
    tag.push_attribute(("calcId", "0"));
    if !manual {
        tag.push_attribute(("fullCalcOnLoad", "1"));
    }
    let mut w = quick_xml::Writer::new(Vec::new());
    w.write_event(if empty {
        Event::Empty(tag)
    } else {
        Event::Start(tag)
    })?;
    Ok(Some(replace_spans(
            &xml,
            [Edit {
                from,
                to,
                text: String::from_utf8(w.into_inner())?,
            }],
        )))

}

#[cfg(test)]
pub(super) fn write_if_supported(base: &Path, target: &Path, wb: &Workbook) -> Result<bool> {
    Ok(write_with_style_receipt(base, target, wb, &mut None)?.patched)
}

/// Parser limitations decline this private attempt. Source ZIP, destination IO
/// and CRC failures must still abort the save and preserve the original file.
fn stream_supported(result: Result<bool>) -> Result<bool> {
    match result {
        Err(error) if error.chain().any(|e| e.is::<quick_xml::Error>() || e.is::<std::str::Utf8Error>())
            && !error.chain().any(|e| e.is::<std::io::Error>() ||
                matches!(e.downcast_ref::<quick_xml::Error>(),Some(quick_xml::Error::Io(_)))) => Ok(false),
        other => other,
    }
}

fn filter_xml_for_sheet(sheet: &Sheet) -> String {
    match sheet.filter.as_ref() {
        Some(f)
            if !sheet.tables.iter().any(|t| {
                t.header_row
                    && t.range.0 <= f.range.0
                    && t.range.1 <= f.range.1
                    && t.range.2 >= f.range.2
                    && t.range.3 >= f.range.3
            }) =>
        {
            crate::patch::auto_filter_xml(f)
        }
        _ => String::new(),
    }
}

fn plain_new_sheet_supported(sheet: &Sheet) -> bool {
    sheet.styles.is_empty()
        && sheet.row_styles.is_empty()
        && sheet.col_styles.is_empty()
        && sheet.merges.is_empty()
        && sheet.notes.is_empty()
        && sheet.charts.is_empty()
        && sheet.pictures.is_empty()
        && sheet.tables.is_empty()
        && sheet.cf.blocks.is_empty()
        && sheet.dv.is_empty()
        && sheet.rich.is_empty()
        && sheet.arrays.is_empty()
        && sheet.spill_shapes.is_empty()
        && sheet.hyperlinks.is_empty()
        && sheet.textboxes.is_empty()
        && sheet.sparklines.is_empty()
        && sheet.scenarios.is_empty()
        && sheet.ignored_errors.is_empty()
        && sheet.protection.is_none()
        && sheet.tab_color.is_none()
        && sheet.freeze.is_none()
        && sheet.filter.is_none()
        && sheet.cols.is_empty()
        && sheet.rows.is_empty()
        && sheet.hidden_cols.is_empty()
        && sheet.hidden_rows.is_empty()
        && sheet.hidden_rows_user.is_empty()
        && sheet.row_levels.is_empty()
        && sheet.col_levels.is_empty()
        && !sheet.layout_dirty
        && !sheet.freeze_dirty
        && !sheet.filter_dirty
        && !sheet.page_dirty
        && !sheet.merges_dirty
        && !sheet.notes_dirty
        && sheet.iter().all(|(key, cell)| {
            key.row < crate::sync::MAX_ROWS
                && key.col < crate::sync::MAX_COLS
                && cell.formula.as_ref().is_none_or(|f| safe_xml_text(f))
                && match &cell.value {
                    Value::Number(n) => n.is_finite(),
                    Value::Text(s) => safe_xml_text(s),
                    Value::Error(crate::formula::ErrKind::Spill) => false,
                    _ => true,
                }
        })
}

fn contains_subslice_fast(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    if haystack.len() < needle.len() {
        return false;
    }
    let first = needle[0];
    let last_start = haystack.len() - needle.len();
    let mut pos = 0usize;
    while pos <= last_start {
        let Some(rel) = super::sheets::stream::simd_memchr(first, &haystack[pos..=last_start])
        else {
            return false;
        };
        let i = pos + rel;
        if &haystack[i..i + needle.len()] == needle {
            return true;
        }
        pos = i + 1;
    }
    false
}

fn sst_root_has_count_attrs(input: impl std::io::Read) -> Result<Option<bool>> {
    use std::io::Read as _;
    let mut head = Vec::with_capacity(1024);
    input.take(1024).read_to_end(&mut head)?;
    if super::sheets::utf16_encoding(&head).is_some() {
        return Ok(None);
    }
    let body = head.strip_prefix(b"\xef\xbb\xbf").unwrap_or(&head);
    let Ok(s) = std::str::from_utf8(body) else {
        // Truncated multi-byte UTF-8 only happens after `<sst ...>`, trim trailing incomplete UTF-8.
        let valid_up_to = match std::str::from_utf8(body) {
            Ok(_) => body.len(),
            Err(e) => e.valid_up_to(),
        };
        if valid_up_to == 0 {
            return Ok(None);
        }
        let prefix = std::str::from_utf8(&body[..valid_up_to]).ok();
        let Some(prefix) = prefix else {
            return Ok(None);
        };
        return parse_sst_root_counts(prefix);
    };
    parse_sst_root_counts(s)
}

fn parse_sst_root_counts(prefix: &str) -> Result<Option<bool>> {
    let mut reader = Reader::from_str(prefix);
    loop {
        match reader.read_event() {
            Ok(Event::Start(ref e) | Event::Empty(ref e)) => {
                if e.local_name().as_ref() != "sst" {
                    return Ok(None);
                }
                for a in e.attributes() {
                    let Ok(a) = a else {
                        return Ok(None);
                    };
                    if matches!(a.key.as_ref(), "count" | "uniqueCount") {
                        return Ok(Some(true));
                    }
                }
                return Ok(Some(false));
            }
            Ok(Event::Decl(_) | Event::Comment(_) | Event::PI(_) | Event::Text(_)) => {}
            _ => return Ok(None),
        }
    }
}

/// Fast multi-core worksheet rewrite inside `xlsx_delta` when a large sheet has >= 16,384
/// dirty cells covering >= 50% of the sheet (e.g., 1M-formula chain recalculation or bulk fill),
/// preserving base `<worksheet>` header/footer verbatim while generating & deflating `<sheetData>`
/// across CPU cores via `fresh::write_sheet_parallel_zip`.
#[allow(clippy::too_many_arguments)]
fn try_parallel_high_dirty_sheet<R: std::io::Read + std::io::Seek>(
    zip: &mut ZipArchive<R>,
    part: &str,
    wb: &Workbook,
    sheet: &Sheet,
    changes: &Changes,
    sst_exists: bool,
) -> Result<Option<(Vec<u8>, Option<(u32, u64)>, bool)>> {
    let dirty_n = changes.dirty_count();
    let high_dirty = dirty_n > 4_096
        || (sheet.len() >= 32_768 && dirty_n.saturating_mul(10) >= sheet.len());
    let large_clean = sheet.len() >= 131_072
        && sheet
            .source_cells
            .as_ref()
            .is_some_and(|s| s.clean_shell.is_some());
    if wb.load_incomplete
        || sheet.source_needs_dom_repair
        || sheet.merges_dirty
        || sheet.rich_dirty
        || (!high_dirty && !large_clean)
        || !changes.styles.rows.is_empty()
        || !changes.styles.cols.is_empty()
        || !changes.styles.updates.is_empty()
        || !changes.styles.dynamic.is_empty()
        || changes.layout.is_some()
        || changes.freeze_edit
        || changes.filter_xml.is_some()
        || changes.page_edit
        || !sheet.arrays.is_empty()
        || !sheet.spill_shapes.is_empty()
        || !sheet.spilled.is_empty()
        || !sheet.rich.is_empty()
    {
        return Ok(None);
    }

    let mut direct_xf: Vec<Option<u16>> = vec![Some(0)];
    let has_any_styles = !sheet.styles.is_empty()
        || !sheet.row_styles.is_empty()
        || !sheet.col_styles.is_empty()
        || sheet
            .source_cells
            .as_ref()
            .is_some_and(|s| !s.styles.is_empty() || !s.row_styles.is_empty() || !s.col_styles.is_empty());
    if has_any_styles {
        let Some(map) = sheet.source_cells.as_ref().and_then(|s| s.style_map.as_ref()) else {
            return Ok(None);
        };
        if !map.derived_from.is_empty() || !map.origin_only.is_empty() {
            return Ok(None);
        }
        direct_xf.resize(wb.styles.len().max(1), None);
        direct_xf[0] = Some(0);
        for (&sid, &opt) in &map.xfs {
            let Some(xf) = opt else {
                return Ok(None);
            };
            if map.xf(sid, &wb.styles) != Some(xf) {
                return Ok(None);
            }
            let idx = usize::from(sid);
            if idx >= direct_xf.len() {
                direct_xf.resize(idx + 1, None);
            }
            direct_xf[idx] = Some(xf);
        }
        if direct_xf[0] != Some(0) {
            return Ok(None);
        }
    }

    // Zero-decompression fast path when `load_diag` captured a verified clean `<worksheet>` shell!
    if let Some(source) = sheet.source_cells.as_ref()
        && let Some(cached_shell) = source.clean_shell.as_deref()
        && let Some(sd_pos) = cached_shell.find("<sheetData/>")
    {
        let header = &cached_shell[..sd_pos];
        let footer = &cached_shell[sd_pos + "<sheetData/>".len()..];
        let updated_header = super::fresh::update_dimension_in_header(
            header,
            "worksheet",
            super::fresh::sheet_styled_bounds(sheet),
        );
        let mut shell = String::with_capacity(updated_header.len() + 12 + footer.len());
        shell.push_str(&updated_header);
        shell.push_str("<sheetData/>");
        shell.push_str(footer);

        let mut str_map: HashMap<&str, u32> = HashMap::new();
        let mut ptr_map: HashMap<(usize, usize), u32> = HashMap::new();
        let sst_lookup = if let Some(arcs) = source.sst_arcs.as_deref() {
            str_map.reserve(arcs.len());
            ptr_map.reserve(arcs.len());
            for (idx, arc) in arcs.iter().enumerate() {
                let id = idx as u32;
                ptr_map.insert((arc.as_ptr() as usize, arc.len()), id);
                str_map.entry(arc.as_ref()).or_insert(id);
            }
            Some(super::fresh::SheetSstLookup::from_maps(&str_map, &ptr_map))
        } else {
            None
        };
        let used_sst_before = sst_exists
            && if source.sst_arcs.is_some() {
                changes.any_text_changed
            } else {
                true
            };
        let Some((mini, sig)) =
            super::fresh::write_sheet_parallel_zip(part, &shell, sheet, sst_lookup, &direct_xf)?
        else {
            return Ok(None);
        };
        return Ok(Some((mini, sig, used_sst_before)));
    }

    let base_xml = super::read_zip_text(zip, part)?;
    let trimmed = base_xml.trim_start_matches('\u{feff}').trim_start();
    let after_decl = if let Some(rest) = trimmed.strip_prefix("<?xml") {
        let Some(end_decl) = rest.find("?>") else {
            return Ok(None);
        };
        rest[end_decl + 2..].trim_start()
    } else {
        trimmed
    };
    if !after_decl.starts_with("<worksheet")
        || !after_decl
            .as_bytes()
            .get("<worksheet".len())
            .is_some_and(|b| b.is_ascii_whitespace() || *b == b'>')
    {
        return Ok(None);
    }

    let bytes = base_xml.as_bytes();
    let Some(sd_rel) = super::sheets::stream::simd_memchr(b'<', bytes).and_then(|_| {
        let mut search = 0usize;
        while search + 10 <= bytes.len() {
            let rel = super::sheets::stream::simd_memchr(b's', &bytes[search..])?;
            let pos = search + rel;
            if pos > 0
                && bytes[pos - 1] == b'<'
                && bytes[pos..].starts_with(b"sheetData")
                && bytes
                    .get(pos + 9)
                    .is_some_and(|b| *b == b'>' || *b == b'/' || b.is_ascii_whitespace())
            {
                return Some(pos - 1);
            }
            search = pos + 1;
        }
        None
    }) else {
        return Ok(None);
    };
    let Some(gt_rel) = super::sheets::stream::simd_memchr(b'>', &bytes[sd_rel + 10..]) else {
        return Ok(None);
    };
    let sd_gt = sd_rel + 10 + gt_rel;
    let header = &base_xml[..sd_rel];
    let (data_inner, footer) = if bytes[sd_gt - 1] == b'/' {
        (&[][..], &base_xml[sd_gt + 1..])
    } else {
        let Some(close_pos) = base_xml.rfind("</sheetData>") else {
            return Ok(None);
        };
        if close_pos < sd_gt + 1 {
            return Ok(None);
        }
        (
            &bytes[sd_gt + 1..close_pos],
            &base_xml[close_pos + "</sheetData>".len()..],
        )
    };

    // Reject unmodeled `<row>` / `<c>` attributes if present in base `<sheetData>`.
    if contains_subslice_fast(data_inner, b"dataTable")
        || contains_subslice_fast(data_inner, b"vm=")
        || contains_subslice_fast(data_inner, b"cm=")
        || contains_subslice_fast(data_inner, b"ph=")
        || contains_subslice_fast(data_inner, b"collapsed=")
        || contains_subslice_fast(data_inner, b"thickTop=")
        || contains_subslice_fast(data_inner, b"thickBot=")
        || (sheet.rows.is_empty() && contains_subslice_fast(data_inner, b"ht="))
        || (sheet.hidden_rows.is_empty() && contains_subslice_fast(data_inner, b"hidden="))
        || (sheet.row_levels.is_empty() && contains_subslice_fast(data_inner, b"outlineLevel="))
    {
        return Ok(None);
    }

    let used_sst_before = sst_exists && contains_subslice_fast(data_inner, b"t=\"s\"");
    let updated_header = super::fresh::update_dimension_in_header(
        header,
        "worksheet",
        super::fresh::sheet_styled_bounds(sheet),
    );
    let mut shell = String::with_capacity(updated_header.len() + 12 + footer.len());
    shell.push_str(&updated_header);
    shell.push_str("<sheetData/>");
    shell.push_str(footer);

    let Some((mini, sig)) =
        super::fresh::write_sheet_parallel_zip(part, &shell, sheet, None, &direct_xf)?
    else {
        return Ok(None);
    };
    Ok(Some((mini, sig, used_sst_before)))
}

pub(super) fn write_with_style_receipt(
    base: &Path, target: &Path, wb: &Workbook, saved_styles: &mut Option<SavedStyles>,
) -> Result<DeltaSummary> {
    let mut zip = ZipArchive::new(File::open(base)?)?;
    check_note_sources(&mut zip, wb)?;
    let mut sheets = HashMap::new();
    let mut new_plain_sheets: HashMap<String, &Sheet> = HashMap::new();
    let mut parts = PackageParts::new();
    let mut formula_changed = false;
    let mut need_new_metadata = false;
    let mut any_layout = false;
    let mut any_freeze = false;
    let mut any_filter = false;
    let mut any_page = false;
    let sst = signature(&mut zip, "xl/sharedStrings.xml");
    let style_part = signature(&mut zip, "xl/styles.xml");
    let style_xml = std::cell::OnceCell::new();
    let dynamic = std::cell::OnceCell::new();
    let mut base_targets: Option<HashMap<String, String>> = None;
    let mut changed_xfs = None;
    let mut base_map = None;
    for sheet in &wb.sheets {
        let cell_or_meta_dirty = sheet.cell_data_needs_save()
            || sheet.styles_dirty
            || sheet.row_styles_dirty
            || sheet.col_styles_dirty
            || !sheet.style_edits.is_empty()
            || sheet.merges_dirty
            || sheet.notes_dirty
            || sheet.rich_dirty;
        let aux_dirty = sheet.layout_dirty || sheet.freeze_dirty || sheet.filter_dirty || sheet.page_dirty;
        if !cell_or_meta_dirty && !aux_dirty {
            continue;
        }
        let (Some(part), Some(source)) = (&sheet.source_part, &sheet.source_cells) else {
            if sheet.source_part.is_none()
                && sheet.source_cells.is_none()
                && wb.pivots_pending.is_empty()
                && wb.pivots_pending2.is_empty()
                && plain_new_sheet_supported(sheet)
            {
                let targets = match &base_targets {
                    Some(t) => t,
                    None => {
                        let book = super::read_zip_text(&mut zip, "xl/workbook.xml").ok();
                        let rels = super::read_zip_text(&mut zip, "xl/_rels/workbook.xml.rels").ok();
                        let map = crate::patch::sheet_targets_xml(book.as_deref(), rels.as_deref());
                        base_targets.insert(map)
                    }
                };
                let Some(part) = targets.get(&sheet.name).cloned() else {
                    return Ok(DeltaSummary::default());
                };
                let placeholder = super::read_zip_text(&mut zip, &part).ok();
                if placeholder
                    .as_deref()
                    .is_some_and(|xml| xml.ends_with("<sheetData/></worksheet>"))
                {
                    formula_changed |= sheet.has_formulas();
                    if new_plain_sheets.insert(part, sheet).is_some() {
                        return Ok(DeltaSummary::default());
                    }
                    continue;
                }
            }
            return Ok(DeltaSummary::default());
        };
        if sheet.source_needs_dom_repair
            && (sheet.row_styles_dirty || sheet.col_styles_dirty || sheet.layout_dirty || sheet.filter_dirty)
        {
            return Ok(DeltaSummary::default());
        }
        if signature(&mut zip, part) != Some(source.signature)
            || source.strings != sst
            || ((sheet.styles_dirty || sheet.row_styles_dirty || sheet.col_styles_dirty || !sheet.row_styles.is_empty() || !sheet.col_styles.is_empty() || !sheet.style_edits.is_empty())
                && source.style_part != style_part)
        {
            return Ok(DeltaSummary::default());
        }
        let has_live_dynamic = super::fresh::has_dynamic(sheet);
        let edit_dynamic = if source.spill_shapes.is_empty() && sheet.spill_shapes.is_empty() {
            None
        } else {
            match *dynamic.get_or_init(|| super::dynamic::editable(&mut zip)) {
                Some(cm) => Some(cm),
                None if source.spill_shapes.is_empty()
                    && has_live_dynamic
                    && zip.index_for_name(super::dynamic::PART).is_none() =>
                {
                    need_new_metadata = true;
                    Some(1)
                }
                None => None,
            }
        };
        let Some(mut changes) = coordinates(sheet, source, &wb.styles, edit_dynamic, |raw, old, id| {
            if changed_xfs.is_none() {
                let xml = style_xml.get_or_init(|| super::read_zip_text(&mut zip, "xl/styles.xml").ok());
                changed_xfs = Some(font_delta::Styles::new(xml.as_deref()?)?);
            }
            changed_xfs.as_mut()?.resolve(raw, old, id, &wb.styles)
        }) else {
            return Ok(DeltaSummary::default());
        };
        if sheet.layout_dirty || sheet.filter_dirty {
            let diff = crate::patch::layout_xml::Changes::diff_from_sheet(sheet, &source.layout);
            if !diff.rows.is_empty() || !diff.cols.is_empty() {
                changes.layout = Some(diff);
            }
            if sheet.layout_dirty {
                any_layout = true;
            }
        }
        if sheet.freeze_dirty {
            changes.freeze_edit = true;
            any_freeze = true;
        }
        if sheet.filter_dirty {
            if sheet
                .filter
                .as_ref()
                .is_none_or(|f| f.color_filters.is_empty())
            {
                changes.filter_xml = Some(filter_xml_for_sheet(sheet));
                any_filter = true;
            }
        }
        if sheet.page_dirty {
            changes.page_edit = true;
            any_page = true;
        }
        if !changes.styles.sources.is_empty() || changes.styles.derived_axis {
            let Some(map) = &source.style_map else { return Ok(DeltaSummary::default()); };
            if let Some(prior) = &base_map {
                if !Arc::ptr_eq(prior, map) { return Ok(DeltaSummary::default()); }
            } else { base_map = Some(map.clone()); }
        }
        if sheet.notes_dirty {
            let Some(notes) = crate::notes::edited_parts(&mut zip, part, sheet)? else {
                return Ok(DeltaSummary::default());
            };
            for (name, data) in notes {
                if parts.insert(name, data).is_some() {
                    return Ok(DeltaSummary::default());
                }
            }
        }
        formula_changed |= changes.formula_changed;
        let has_sheet_work = changes.dirty_count() > 0
            || sheet.merges_dirty
            || !changes.styles.rows.is_empty()
            || !changes.styles.cols.is_empty()
            || changes.layout.is_some()
            || changes.freeze_edit
            || changes.filter_xml.is_some()
            || changes.page_edit;
        if has_sheet_work {
            if sheets
                .insert(part.clone(), (sheet, &source.store, changes))
                .is_some()
            {
                return Ok(DeltaSummary::default());
            }
        }
    }
    let mut trusted_parts: HashMap<String, (u32, u64)> = HashMap::new();
    let mut updated_map = None;
    let mut updated_styles_sig: Option<Signature> = None;
    if let Some((xml, additions)) = changed_xfs.and_then(font_delta::Styles::finish) {
        let Some(old) = base_map else { return Ok(DeltaSummary::default()); };
        let mut mapped = old.xfs.clone();
        let mut derived_from = old.derived_from.clone();
        mapped.retain(|&id, value| value.is_none() || old.xf(id, &wb.styles).is_some());
        for (base, id, xf) in additions {
            derived_from.entry(id).and_modify(|origin| {
                if *origin != Some(base) { *origin = None; }
            }).or_insert(Some(base));
            mapped.entry(id).and_modify(|entry| {
                if *entry != Some(xf) { *entry = None; }
            }).or_insert(Some(xf));
        }
        let origin_only=old.origin_only.iter().filter(|(id,_)|old.unchanged(**id,&wb.styles))
            .map(|(&id,&origin)|(id,origin)).collect();
        updated_map = Some(Arc::new(StyleMap { xfs: mapped, derived_from, origin_only, original: wb.styles.clone() }));
        let style_bytes = xml.into_bytes();
        let mut crc = flate2::Crc::new();
        crc.update(&style_bytes);
        let sig = (crc.sum(), style_bytes.len() as u64);
        updated_styles_sig = Some(sig);
        trusted_parts.insert("xl/styles.xml".to_owned(), sig);
        parts.insert("xl/styles.xml".to_owned(), style_bytes);
    }
    let mut deleted = HashSet::new();
    if formula_changed {
        let Some((chain, remove)) = invalidate_chain(&mut zip, wb.calc_manual)? else {
            return Ok(DeltaSummary::default());
        };
        deleted = remove;
        for (name, data) in chain {
            if parts.insert(name, data).is_some() {
                return Ok(DeltaSummary::default());
            }
        }
    }
    // Fold calcMode / calcIteration / print_names / recalc_on_open into xl/workbook.xml
    // during this single ZIP write so save_plain_at does not perform a 2nd full-ZIP copy.
    if !wb.names_dirty && zip.index_for_name("xl/workbook.xml").is_some() {
        let wb_bytes = match parts.get("xl/workbook.xml") {
            Some(b) => Some(b.clone()),
            None => {
                let mut b = Vec::new();
                use std::io::Read;
                zip.by_name("xl/workbook.xml")
                    .ok()
                    .and_then(|mut e| e.read_to_end(&mut b).ok().map(|_| b))
            }
        };
        if let Some(wb_bytes) = wb_bytes
            && let Ok(wb_xml) = String::from_utf8(wb_bytes.clone())
        {
            let mode_xml = if crate::patch::calc_manual_xml(&wb_xml) == wb.calc_manual {
                std::borrow::Cow::Borrowed(wb_xml.as_str())
            } else {
                std::borrow::Cow::Owned(crate::patch::set_calc_mode(&wb_xml, wb.calc_manual))
            };
            let mut next_xml = crate::patch::set_calc_iteration(
                &mode_xml,
                wb.calc_iterate,
                wb.calc_max_iterations,
                wb.calc_max_change,
            );
            if any_page {
                for (idx, sheet) in wb.sheets.iter().enumerate() {
                    if sheet.page_dirty {
                        next_xml = crate::patch::set_print_names(
                            &next_xml,
                            idx as u32,
                            &sheet.name,
                            &sheet.page,
                        );
                    }
                }
            }
            let need_recalc = wb.needs_full_calc_on_load()
                && wb.sheets.iter().any(|s| s.cell_data_needs_save());
            let updated = if need_recalc {
                match recalc_on_open(&next_xml, false)? {
                    Some(bytes) => bytes,
                    None => return Ok(DeltaSummary::default()),
                }
            } else {
                next_xml.into_bytes()
            };
            if updated != wb_bytes {
                parts.insert("xl/workbook.xml".to_owned(), updated);
            }
        }
    }
    if need_new_metadata {
        for meta_part in [super::dynamic::RELS, super::dynamic::TYPES] {
            if zip.index_for_name(meta_part).is_none() {
                return Ok(DeltaSummary::default());
            }
            let mut xml = match parts.remove(meta_part) {
                Some(bytes) => String::from_utf8(bytes)?,
                None => super::read_zip_text(&mut zip, meta_part)?,
            };
            if super::dynamic::declare(&mut xml, meta_part).is_err() {
                return Ok(DeltaSummary::default());
            }
            parts.insert(meta_part.to_owned(), xml.into_bytes());
        }
    }
    if parts
        .keys()
        .any(|name| sheets.contains_key(name) || new_plain_sheets.contains_key(name) || deleted.contains(name))
    {
        return Ok(DeltaSummary::default());
    }

    let dir = target
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));

    // Parallelize multiple dirty worksheets across CPU cores into single-entry temp ZIPs,
    // then splice via O(1) `raw_copy_file`.
    enum ParallelDeltaPart {
        Mini(Vec<u8>),
        Temp(tempfile::TempPath),
    }
    let mut parallel_sheets: HashMap<String, ParallelDeltaPart> = HashMap::new();
    let mut changed_sst = false;
    let sst_exists = sst.is_some();
    if sheets.len() >= 2 {
        let mut jobs: Vec<(String, usize, &Sheet, &Store, Changes)> =
            Vec::with_capacity(sheets.len());
        for (part, (sheet, store, changes)) in sheets.drain() {
            let Some(idx) = zip.index_for_name(&part) else {
                return Ok(DeltaSummary::default());
            };
            jobs.push((part, idx, sheet, store, changes));
        }
        let workers = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2)
            .min(8)
            .min(jobs.len());
        let job_queue = std::sync::Mutex::new(jobs.into_iter());
        let results: std::sync::Mutex<
            Vec<Result<Option<(String, ParallelDeltaPart, bool, Option<(u32, u64)>)>>>,
        > = std::sync::Mutex::new(Vec::new());
        std::thread::scope(|scope| {
            for _ in 0..workers {
                let q = &job_queue;
                let res = &results;
                scope.spawn(move || {
                    loop {
                        let next = { q.lock().unwrap().next() };
                        let Some((part, idx, sheet, store, mut changes)) = next else {
                            break;
                        };
                        let one = (|| -> Result<
                            Option<(String, ParallelDeltaPart, bool, Option<(u32, u64)>)>,
                        > {
                            let mut z = ZipArchive::new(File::open(base)?)?;
                            if let Some((mini, sig, used_sst_before)) =
                                try_parallel_high_dirty_sheet(
                                    &mut z,
                                    &part,
                                    wb,
                                    sheet,
                                    &changes,
                                    sst_exists,
                                )?
                            {
                                return Ok(Some((
                                    part,
                                    ParallelDeltaPart::Mini(mini),
                                    used_sst_before,
                                    sig,
                                )));
                            }
                            changes.ensure_keys_populated(sheet, store);
                            let (f, tp) = tempfile::NamedTempFile::new_in(dir)?.into_parts();
                            let mut zw = ZipWriter::new(BufWriter::with_capacity(128 * 1024, f));
                            zw.start_file(
                                part.as_str(),
                                SimpleFileOptions::default()
                                    .compression_method(zip::CompressionMethod::Deflated)
                                    .compression_level(Some(1)),
                            )?;
                            let mut local_sst = false;
                            let mut vw = super::fresh::VerifiedXmlWriter::new(&mut zw);
                            let supported = stream_supported(stream::xml_input(
                                z.by_index(idx)?,
                                &part,
                                |input| {
                                    stream::worksheet(
                                        input,
                                        &mut vw,
                                        sheet,
                                        store,
                                        changes,
                                        &mut local_sst,
                                    )
                                },
                            ))?;
                            if !supported {
                                return Ok(None);
                            }
                            let sig = vw.finish(Some("worksheet"))?;
                            zw.finish()?.flush()?;
                            Ok(Some((part, ParallelDeltaPart::Temp(tp), local_sst, sig)))
                        })();
                        res.lock().unwrap().push(one);
                    }
                });
            }
        });
        for item in results.into_inner().unwrap() {
            let Some((part, pp, local_sst, sig)) = item? else {
                return Ok(DeltaSummary::default());
            };
            changed_sst |= local_sst;
            if let Some(s) = sig {
                trusted_parts.insert(part.clone(), s);
            }
            parallel_sheets.insert(part, pp);
        }
    }

    let tmp = tempfile::NamedTempFile::new_in(dir)?;
    let (file, tmp) = tmp.into_parts();
    let mut out = ZipWriter::new(BufWriter::with_capacity(256 * 1024, file));
    for index in 0..zip.len() {
        let part = zip
            .name_for_index(index)
            .context("缺少 ZIP 条目名")?
            .to_owned();
        if deleted.contains(&part) || part == "xl/sharedStrings.xml" {
            continue;
        }
        if let Some(data) = parts.remove(&part) {
            out.start_file(
                &part,
                SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated)
                    .compression_level(Some(1)),
            )?;
            out.write_all(&data)?;
        } else if let Some(pp) = parallel_sheets.remove(&part) {
            match pp {
                ParallelDeltaPart::Mini(mini) => {
                    let mut mini_zip = ZipArchive::new(std::io::Cursor::new(mini))?;
                    out.raw_copy_file(mini_zip.by_index_raw(0)?)?;
                }
                ParallelDeltaPart::Temp(tp) => {
                    let mut part_zip = ZipArchive::new(File::open(&tp)?)?;
                    out.raw_copy_file(part_zip.by_index_raw(0)?)?;
                }
            }
        } else if let Some((sheet, store, mut changes)) = sheets.remove(&part) {
            if let Some((mini, sig, used_sst_before)) =
                try_parallel_high_dirty_sheet(&mut zip, &part, wb, sheet, &changes, sst_exists)?
            {
                changed_sst |= used_sst_before;
                let mut mini_zip = ZipArchive::new(std::io::Cursor::new(mini))?;
                out.raw_copy_file(mini_zip.by_index_raw(0)?)?;
                if let Some(s) = sig {
                    trusted_parts.insert(part, s);
                }
            } else {
                changes.ensure_keys_populated(sheet, store);
                let entry = zip.by_index(index)?;
                let Some(sig_opt) = super::fresh::write_parallel_deflate_entry(
                    &mut out,
                    &part,
                    1,
                    Some("worksheet"),
                    |mut w| {
                        stream_supported(stream::xml_input(entry, &part, |input| {
                            stream::worksheet(
                                input,
                                &mut w,
                                sheet,
                                store,
                                changes,
                                &mut changed_sst,
                            )
                        }))
                    },
                )?
                else {
                    return Ok(DeltaSummary::default());
                };
                if let Some(s) = sig_opt {
                    trusted_parts.insert(part, s);
                }
            }
        } else if let Some(sheet) = new_plain_sheets.remove(&part) {
            out.start_file(
                &part,
                SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated)
                    .compression_level(Some(1)),
            )?;
            if let Some(s) = super::fresh::write_plain_new_sheet(sheet, &mut out)? {
                trusted_parts.insert(part, s);
            }
        } else {
            let raw = zip.by_index_raw(index)?;
            if !wb.load_incomplete && (raw.size() == 0 || raw.compressed_size() > 0) {
                trusted_parts.insert(part, (raw.crc32(), raw.size()));
            }
            out.raw_copy_file(raw)?;
        }
    }
    if !sheets.is_empty() || !parallel_sheets.is_empty() || !new_plain_sheets.is_empty() || !parts.is_empty() {
        return Ok(DeltaSummary::default());
    }
    if need_new_metadata {
        out.start_file(
            super::dynamic::PART,
            SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .compression_level(Some(1)),
        )?;
        out.write_all(super::dynamic::METADATA.as_bytes())?;
    }
    if let Some(i) = zip.index_for_name("xl/sharedStrings.xml") {
        let need_strip_counts = if changed_sst {
            match sst_root_has_count_attrs(zip.by_index(i)?)? {
                Some(false) => false,
                _ => true,
            }
        } else {
            false
        };
        if need_strip_counts {
            // Some indexed references became inline strings. Both counters are
            // optional; preserve indices, rich runs and every original <si>.
            let raw_size = zip.by_index_raw(i)?.size();
            if raw_size >= 2 * 1024 * 1024 {
                let entry = zip.by_index(i)?;
                let Some(sig_opt) = super::fresh::write_parallel_deflate_entry(
                    &mut out,
                    "xl/sharedStrings.xml",
                    1,
                    Some("sst"),
                    |mut w| {
                        stream_supported(stream::xml_input(entry, "xl/sharedStrings.xml", |input| {
                            stream::shared_strings(input, &mut w)
                        }))
                    },
                )?
                else {
                    return Ok(DeltaSummary::default());
                };
                if let Some(s) = sig_opt {
                    trusted_parts.insert("xl/sharedStrings.xml".to_owned(), s);
                }
            } else {
                out.start_file(
                    "xl/sharedStrings.xml",
                    SimpleFileOptions::default()
                        .compression_method(zip::CompressionMethod::Deflated)
                        .compression_level(Some(1)),
                )?;
                let mut vw = super::fresh::VerifiedXmlWriter::new(&mut out);
                if !stream_supported(stream::xml_input(
                    zip.by_index(i)?,
                    "xl/sharedStrings.xml",
                    |input| stream::shared_strings(input, &mut vw),
                ))? {
                    return Ok(DeltaSummary::default());
                }
                if let Some(s) = vw.finish(Some("sst"))? {
                    trusted_parts.insert("xl/sharedStrings.xml".to_owned(), s);
                }
            }
        } else {
            let raw = zip.by_index_raw(i)?;
            if !wb.load_incomplete && (raw.size() == 0 || raw.compressed_size() > 0) {
                trusted_parts.insert("xl/sharedStrings.xml".to_owned(), (raw.crc32(), raw.size()));
            }
            out.raw_copy_file(raw)?;
        }
    }
    out.finish()?.flush()?;
    drop(zip);
    if let Some(map) = updated_map {
        let signature = updated_styles_sig.context("缺少更新的样式表")?;
        *saved_styles = Some(SavedStyles { signature, map });
    }
    tmp.persist(target)?;
    let any_color_filter_dirty = wb.sheets.iter().any(|s| {
        s.filter_dirty
            && s.filter
                .as_ref()
                .is_some_and(|f| !f.color_filters.is_empty())
    });
    Ok(DeltaSummary {
        patched: true,
        handled_layout: any_layout,
        handled_freeze: any_freeze,
        handled_filter: any_filter && !any_color_filter_dirty,
        handled_page: any_page,
        trusted_parts,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::xlsx::{load_diag, read_part, save, save_atomic_with_receipt};

    fn fixture(rows: &str, dimension: &str) -> (tempfile::TempDir, std::path::PathBuf, String) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.xlsx");
        save(&path, &Workbook::new_blank()).unwrap();
        let part = crate::patch::sheet_targets(&path).unwrap()["Sheet1"].clone();
        let xml = format!(
            "<worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">{dimension}<sheetData>{rows}</sheetData></worksheet>"
        );
        crate::fixups::write_parts(&path, &[(part.clone(), xml.into_bytes())]).unwrap();
        (dir, path, part)
    }

    fn num(v: f64) -> Cell {
        Cell {
            value: Value::Number(v),
            formula: None,
        }
    }

    #[test]
    fn malformed_stream_declines_without_masking_an_io_error() {
        let mut reader = quick_xml::Reader::from_str("<x></y>");
        reader.read_event().unwrap();
        let malformed = reader.read_event().unwrap_err();
        assert!(!stream_supported(Err(malformed.into())).unwrap());
        let io = std::io::Error::other("write failed");
        assert!(stream_supported(Err(quick_xml::Error::from(io).into())).is_err());
    }

    #[test]
    fn value_only_edits_request_excel_recalculation_only_for_automatic_books() {
        for manual in [false,true] {
            let (_dir,path,_) = fixture("<row r='1'><c r='A1'><v>2</v></c><c r='B1'><f>A1*2</f><v>4</v></c></row>","");
            let (mut wb,_) = load_diag(&path).unwrap();
            wb.calc_manual = manual;
            wb.sheets[0].set(0,0,num(9.));
            assert!(save_atomic_with_receipt(&path,&wb).unwrap().used_cell_delta);
            let xml = String::from_utf8(read_part(&path,"xl/workbook.xml").unwrap()).unwrap();
            let doc = roxmltree::Document::parse(&xml).unwrap();
            let calc = doc.descendants().find(|n| n.tag_name().name()=="calcPr").unwrap();
            assert_eq!(calc.attribute("fullCalcOnLoad")==Some("1"),!manual);
            assert_eq!(calc.attribute("calcMode")==Some("manual"),manual);
        }
    }

    #[test]
    fn fallback_style_receipt_allows_a_second_edit_without_reopening() {
        let (dir, path, part) = fixture("<row r='1'><c r='A1'><v>2</v></c></row>", "");
        let (mut wb, _) = load_diag(&path).unwrap();
        let mut style = wb.styles.get(wb.sheets[0].style_of(0, 0)).clone();
        style.locked = false;
        let unlocked = wb.styles.intern(style.clone());
        // Force initial full-DOM save via unmodeled quotePrefix so we exercise StyleMap recovery.
        wb.styles.set_quote(unlocked, true);
        wb.sheets[0].set_style(0, 0, unlocked);
        let first = save_atomic_with_receipt(&path, &wb).unwrap();
        assert!(!first.used_cell_delta);
        first.adopt_document(&mut wb, &path, true);
        style.italic = true;
        style.valign = 1;
        let italic = wb.styles.intern(style);
        wb.styles.set_quote(italic, true);
        // Even a signed package with the same style table can contain a cell
        // whose actual XF does not agree with the recovered default alias.
        let other = dir.path().join("different-origin.xlsx");
        std::fs::copy(&path, &other).unwrap();
        let mut xml = String::from_utf8(read_part(&other, &part).unwrap()).unwrap();
        let span = roxmltree::Document::parse(&xml)
            .unwrap()
            .descendants()
            .find(|c| c.attribute("r") == Some("A1"))
            .unwrap()
            .attribute_node("s")
            .unwrap()
            .range_value();
        xml.replace_range(span, "0");
        crate::fixups::write_parts(&other, &[(part.clone(), xml.into_bytes())]).unwrap();
        let mut divergent = wb.clone();
        let mut zip = ZipArchive::new(File::open(&other).unwrap()).unwrap();
        divergent.sheets[0].source_cells =
            SourceCells::capture(&mut zip, &part, &divergent.sheets[0]);
        drop(zip);
        divergent.sheets[0].set_style(0, 0, italic);
        assert!(!write_if_supported(&other, &dir.path().join("guard.xlsx"), &divergent).unwrap());
        wb.sheets[0].set_style(0, 0, italic);
        assert!(
            save_atomic_with_receipt(&path, &wb)
                .unwrap()
                .used_cell_delta
        );
        let (saved, _) = load_diag(&path).unwrap();
        let style = saved.styles.get(saved.sheets[0].style_of(0, 0));
        assert!(style.italic && !style.locked);
        assert_eq!(style.valign, 1);
    }

    #[test]
    fn existing_xf_zero_and_new_inherited_values_survive_streaming_and_dom_saves() {
        for force_dom in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("mixed.xlsx");
            let mut wb = Workbook::new_blank();
            let normal = wb.styles.get(0).clone();
            let bold = wb.styles.intern(crate::style::CellStyle {
                bold: true,
                ..normal.clone()
            });
            let italic = wb.styles.intern(crate::style::CellStyle {
                italic: true,
                ..normal
            });
            let sheet = wb.sheet_mut(0);
            sheet.set_row_style(0, bold);
            sheet.set_col_style(1, italic);
            sheet.set(0, 0, num(1.));
            sheet.set(1, 1, num(2.));
            save(&path, &wb).unwrap();
            let part = crate::patch::sheet_targets(&path).unwrap()["Sheet1"].clone();
            let original = String::from_utf8(read_part(&path, &part).unwrap()).unwrap();
            let doc = roxmltree::Document::parse(&original).unwrap();
            let mut xml = original.clone();
            for cell in doc
                .descendants()
                .filter(|n| n.tag_name().name() == "c")
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
            {
                let range = cell.attribute_node("s").unwrap().range();
                let extra = if force_dom && cell.attribute("r") == Some("A1") {
                    "ph=\"1\""
                } else {
                    ""
                };
                xml.replace_range(range, extra);
            }
            crate::fixups::write_parts(&path, &[(part.clone(), xml.into_bytes())]).unwrap();
            let (mut wb, diag) = load_diag(&path).unwrap();
            assert!(diag.is_complete());
            let sheet = wb.sheet_mut(0);
            sheet.set(0, 0, num(10.));
            sheet.set(0, 1, num(20.)); // Both defaults apply; the row takes precedence.
            sheet.set(2, 1, num(30.));
            let check = |wb: &Workbook, stage: &str| {
                let style = |r, c| wb.styles.get(wb.sheets[0].style_of(r, c));
                assert!(!style(0, 0).bold && !style(1, 1).italic);
                assert_eq!(
                    (style(0, 1).bold, style(0, 1).italic, style(2, 1).italic),
                    (true, false, true),
                    "{stage} force_dom={force_dom} sheet={:?} {:?}",
                    wb.sheets[0].row_styles,
                    wb.sheets[0].col_styles
                );
            };
            check(&wb, "before");
            let receipt = save_atomic_with_receipt(&path, &wb).unwrap();
            assert_eq!(receipt.used_cell_delta, !force_dom);
            let xml = String::from_utf8(read_part(&path, &part).unwrap()).unwrap();
            let doc = roxmltree::Document::parse(&xml).unwrap();
            let xf = |address| {
                doc.descendants()
                    .find(|n| n.attribute("r") == Some(address))
                    .unwrap()
                    .attribute("s")
                    .unwrap_or("0")
            };
            assert_eq!((xf("A1"), xf("B2")), ("0", "0"));
            assert_ne!(xf("B1"), "0");
            assert_ne!(xf("B3"), "0");
            let saved = load_diag(&path).unwrap().0;
            check(&saved, "after");
            assert_eq!(saved.sheets[0].value(2, 1), Value::Number(30.));
        }
    }

    #[test]
    fn unsupported_axis_xml_uses_the_dom_save_and_keeps_neighbor_values() {
        for (column, columns, row) in [
            (false, "", "<row>"),
            (
                true,
                "<cols><col min='1' max='3'><extLst/></col></cols>",
                "<row r='1'>",
            ),
            (
                true,
                "<cols><col min='2'/><col max='3'/><col min='invalid' max='4'/></cols>",
                "<row r='1'>",
            ),
            (
                false,
                "<cols><col min='16383' max='4294967295' width='24' customWidth='1'/></cols>",
                "<row r='1'>",
            ),
        ] {
            let rows = if row == "<row>" {
                "<row><c><v>7</v></c><c><v>8</v></c></row>".to_owned()
            } else {
                format!("{row}<c r='A1'><v>7</v></c><c r='B1'><v>8</v></c></row>")
            };
            let (_dir, path, part) = fixture(&rows, columns);
            let (mut wb, diag) = load_diag(&path).unwrap();
            assert!(diag.is_complete(), "{:?}", diag.problems);
            let bold = wb.styles.intern(crate::style::CellStyle {
                bold: true,
                ..wb.styles.get(0).clone()
            });
            if column {
                wb.sheets[0].set_col_style(1, bold);
            } else {
                wb.sheets[0].set_row_style(0, bold);
            }
            let receipt = save_atomic_with_receipt(&path, &wb).unwrap();
            assert!(!receipt.used_cell_delta);
            let saved = load_diag(&path).unwrap().0;
            assert!(!saved.sheets[0].source_needs_dom_repair, "{columns} {row}: {}", String::from_utf8_lossy(&read_part(&path,&part).unwrap()));
            assert_eq!(
                saved.sheets[0].value(0, 0),
                Value::Number(7.),
                "{columns} {row}"
            );
            assert_eq!(saved.sheets[0].value(0, 1), Value::Number(8.));
            assert!(
                saved.styles.get(saved.sheets[0].style_of(0, 1)).bold,
                "{part}"
            );
            assert_eq!(
                saved.styles.get(saved.sheets[0].style_of(0, 0)).bold,
                !column
            );
            if columns.contains("4294967295") {
                assert!(saved.sheets[0].cols.size(16383) > 100.);
            }
        }
    }

    #[test]
    fn saves_axis_defaults_and_materializes_inherited_xfs_on_populated_cells() {
        let (_dir, source, part) = fixture(
            "<row r='3'><c r='B3'><v>3</v></c></row>",
            "<dimension ref='B3'/>",
        );
        let mut wb = load_diag(&source).unwrap().0;
        let bold = wb.styles.intern(crate::style::CellStyle {
            bold: true,
            ..wb.styles.get(0).clone()
        });
        let sheet = wb.sheet_mut(0);
        sheet.set_col_style(1, bold);
        sheet.set_row_style(4, bold);
        sheet.set(4, 0, num(5.));
        let target = source.with_file_name("axes.xlsx");
        assert!(write_with_style_receipt(&source, &target, &wb, &mut None).unwrap().patched);
        let mut wb = load_diag(&target).unwrap().0;
        let xml = String::from_utf8(read_part(&target, &part).unwrap()).unwrap();
        let doc = roxmltree::Document::parse(&xml).unwrap();
        let raw = |kind, attr, id, style| {
            doc.descendants()
                .find(|n| n.tag_name().name() == kind && n.attribute(attr) == Some(id))
                .unwrap()
                .attribute(style)
                .unwrap()
        };
        let xf = raw("col", "min", "2", "style");
        assert_ne!(xf, "0");
        assert_eq!(raw("row", "r", "5", "s"), xf);
        assert_eq!(raw("c", "r", "B3", "s"), xf);
        assert_eq!(raw("c", "r", "A5", "s"), xf);
        wb.sheets[0].set(8, 1, num(42.));
        let path = source.with_file_name("new-value.xlsx");
        assert!(write_with_style_receipt(&target, &path, &wb, &mut None).unwrap().patched);
        let xml = String::from_utf8(read_part(&path, &part).unwrap()).unwrap();
        let doc = roxmltree::Document::parse(&xml).unwrap();
        assert_eq!(
            doc.descendants()
                .find(|n| n.attribute("r") == Some("B9"))
                .unwrap()
                .attribute("s"),
            Some(xf)
        );
        wb.sheets[0].set_col_style(1, 0);
        wb.sheets[0].set_row_style(4, 0);
        assert!(write_with_style_receipt(&target, &path, &wb, &mut None).unwrap().patched);
        let back = load_diag(&path).unwrap().0;
        assert!(back.sheets[0].row_styles.is_empty() && back.sheets[0].col_styles.is_empty());
    }

    #[test]
    fn stream_missing_dimension_and_expand_shared_formulas() {
        let rows = r#"<row r="1"><c r="A1"><f t="shared" si="2" ref="A1:A2">IF(B1&gt;0,$C$1,0)+'A1'!B1+T[B2]</f><v>12</v></c></row><row r="2"><c r="A2"><f t="shared" si="2"/><v>4</v></c></row><row r="1048576"><c r="XFD1048576"><v>42</v></c></row>"#;
        let (_dir, path, _) = fixture(rows, "");
        let (wb, diag) = load_diag(&path).unwrap();
        assert!(diag.is_complete(), "{:?}", diag.problems);
        let sheet = &wb.sheets[0];
        assert_eq!(sheet.len(), 3);
        assert_eq!(sheet.value(1_048_575, 16_383), Value::Number(42.0));
        assert_eq!(
            sheet.get(1, 0).unwrap().formula.as_deref(),
            Some("=IF(B2>0,$C$1,0)+'A1'!B2+T[B2]")
        );
    }

    #[test]
    fn edits_existing_values_and_inserts_missing_rows_without_rebuilding_neighbors() {
        let kept = r#"<c r="D2" s="0" t="n"><v>03.00</v></c>"#;
        let rows = format!(
            r#"<row r="2" spans="1:4"><c r="A2" s="0"><v>1</v></c>{kept}</row><row r="5"/>"#
        );
        let (dir, path, part) = fixture(&rows, r#"<dimension ref="A2:D2"></dimension>"#);
        let (mut wb, diag) = load_diag(&path).unwrap();
        assert!(diag.is_complete());
        let sheet = &mut wb.sheets[0];
        sheet.remove(1, 0);
        sheet.set(0, 1, num(5.0));
        sheet.set(
            1,
            2,
            Cell {
                value: Value::Text(" 甲<&乙 ".into()),
                formula: None,
            },
        );
        sheet.set(4, 0, num(7.0));
        sheet.set(5, 0, num(8.0));
        let copy = dir.path().join("edited.xlsx");
        assert!(write_if_supported(&path, &copy, &wb).unwrap());
        let receipt = save_atomic_with_receipt(&copy, &wb).unwrap();
        receipt.adopt_document(&mut wb, &copy, true);
        let xml = String::from_utf8(read_part(&copy, &part).unwrap()).unwrap();
        assert!(xml.contains(kept) && xml.contains(r#"<c r="A2" s="0"/>"#));
        assert!(xml.contains(r#"ref="A1:D6""#));
        let (loaded, diag) = load_diag(&copy).unwrap();
        assert!(diag.is_complete(), "{:?}", diag.problems);
        for (r, c) in [(1, 0), (0, 1), (1, 2), (1, 3), (4, 0), (5, 0)] {
            assert_eq!(loaded.sheets[0].value(r, c), wb.sheets[0].value(r, c));
        }

        wb.sheets[0].set(0, 1, num(15.0));
        let next = dir.path().join("next.xlsx");
        assert!(write_if_supported(&copy, &next, &wb).unwrap());
        assert!(
            !write_if_supported(&path, &next, &wb).unwrap(),
            "an old snapshot cannot patch a different XML part"
        );
    }

    #[test]
    fn clearing_a_value_preserves_the_explicit_style_with_an_edited_merge() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("formatted.xlsx");
        let mut wb = Workbook::new_blank();
        let bold = wb.styles.intern(crate::style::CellStyle {
            bold: true,
            ..Default::default()
        });
        wb.sheets[0].set(0, 0, num(10.0));
        wb.sheets[0].set_style(0, 0, bold);
        save(&file, &wb).unwrap();
        let (mut wb, _) = load_diag(&file).unwrap();
        assert!(
            wb.styles.get(wb.sheets[0].style_of(0, 0)).bold,
            "baseline style missing"
        );
        wb.sheets[0].remove(0, 0);
        wb.sheets[0].merge_range(2, 1, 2, 2);
        let out = dir.path().join("cleared.xlsx");
        assert!(write_if_supported(&file, &out, &wb).unwrap());
        save(&out, &wb).unwrap();
        let (loaded, diag) = load_diag(&out).unwrap();
        assert!(diag.is_complete());
        let sheet = &loaded.sheets[0];
        assert_eq!(sheet.value(0, 0), Value::Empty);
        assert!(
            loaded.styles.get(sheet.style_of(0, 0)).bold,
            "sheet XML: {}",
            String::from_utf8(read_part(&out, sheet.source_part.as_deref().unwrap()).unwrap())
                .unwrap()
        );
    }

    #[test]
    fn editing_an_indexed_string_retains_other_indices_and_rich_runs() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("strings.xlsx");
        let mut wb = Workbook::new_blank();
        wb.sheets[0].set(
            0,
            0,
            Cell {
                value: Value::Text("first".into()),
                formula: None,
            },
        );
        wb.sheets[0].set(
            0,
            1,
            Cell {
                value: Value::Text("second".into()),
                formula: None,
            },
        );
        save(&path, &wb).unwrap();
        let sst = String::from_utf8(read_part(&path, "xl/sharedStrings.xml").unwrap()).unwrap();
        let rich = "<r><rPr><b/></rPr><t>second</t></r>";
        let xml = sst.replace("<t>second</t>", rich);
        assert!(xml.contains(rich));
        crate::fixups::write_parts(&path, &[("xl/sharedStrings.xml".into(), xml.into_bytes())])
            .unwrap();
        let (mut wb, _) = load_diag(&path).unwrap();
        wb.sheets[0].set(0, 0, num(5.0));
        let out = dir.path().join("indexed.xlsx");
        assert!(write_if_supported(&path, &out, &wb).unwrap());
        let xml = String::from_utf8(read_part(&out, "xl/sharedStrings.xml").unwrap()).unwrap();
        assert!(xml.contains(rich) && !xml.contains("uniqueCount="));
        let (loaded, diag) = load_diag(&out).unwrap();
        assert!(diag.is_complete());
        assert_eq!(loaded.sheets[0].value(0, 1), Value::Text("second".into()));
        assert!(loaded.sheets[0].rich[&(0, 1)][0].bold);
    }

    #[test]
    fn reuse_verified_cell_xf_including_style_only_blanks_and_repeat_save() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("styles.xlsx");
        let mut wb = Workbook::new_blank();
        let bold = wb.styles.intern(crate::style::CellStyle {
            bold: true,
            ..Default::default()
        });
        wb.sheets[0].set(0, 0, num(2.0));
        wb.sheets[0].set_style(0, 0, bold);
        save(&file, &wb).unwrap();
        let (mut wb, d) = load_diag(&file).unwrap();
        assert!(d.is_complete());
        let part = wb.sheets[0].source_part.clone().unwrap();
        let prior = read_part(&file, "xl/styles.xml").unwrap();
        let base_store = wb.sheets[0].source_cells.as_ref().unwrap().store.clone();
        let bold = wb.sheets[0].style_of(0, 0);
        wb.sheets[0].set_style(345, 3, bold);
        wb.sheets[0].merge_range(500, 1, 501, 2);
        let copy = dir.path().join("one.xlsx");
        assert!(write_if_supported(&file, &copy, &wb).unwrap());
        let receipt = save_atomic_with_receipt(&file, &wb).unwrap();
        receipt.adopt_document(&mut wb, &file, true);
        assert!(Arc::ptr_eq(
            &wb.sheets[0].source_cells.as_ref().unwrap().store,
            &base_store
        ));
        let (first, _) = load_diag(&file).unwrap();
        assert!(first.styles.get(first.sheets[0].style_of(345, 3)).bold);
        assert_eq!(first.sheets[0].merges, vec![(500, 1, 501, 2)]);
        wb.sheets[0].set_style(0, 0, 0);
        wb.sheets[0].set_style(346, 3, bold);
        wb.sheets[0].unmerge_range(500, 1, 501, 2);
        assert!(write_if_supported(&file, &copy, &wb).unwrap());
        assert_eq!(read_part(&copy, "xl/styles.xml").unwrap(), prior);
        let xml = String::from_utf8(read_part(&copy, &part).unwrap()).unwrap();
        assert!(!xml.contains("<mergeCells"));
        let (second, _) = load_diag(&copy).unwrap();
        assert!(!second.styles.get(second.sheets[0].style_of(0, 0)).bold);
        assert!(second.styles.get(second.sheets[0].style_of(346, 3)).bold);
        assert_eq!(second.sheets[0].value(0, 0), Value::Number(2.0));
        let custom = wb.styles.intern(crate::style::CellStyle {
            rotation: 39,
            ..Default::default()
        });
        wb.sheets[0].set_style(0, 1, custom);
        assert!(!write_if_supported(&file, &copy, &wb).unwrap());
    }

    #[test]
    fn new_font_xfs_survive_two_atomic_saves_and_preserve_raw_properties() {
        let (dir, file, part) = fixture(r#"<row r="1"><c r="A1"><v>42</v></c></row>"#, "");
        let styles = r#"<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:e="urn:test">
<fonts count="1"><font><sz val="11"/><name val="Calibri"/><family val="2"/></font></fonts>
<fills count="2"><fill><patternFill patternType="none"/></fill><fill><patternFill patternType="gray125"/></fill></fills>
<borders count="1"><border><left/><right/><top/><bottom/><diagonal/></border></borders>
<cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs>
<cellXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0" e:keep="yes"><alignment readingOrder="2"/></xf></cellXfs>
<cellStyles count="1"><cellStyle name="Normal" xfId="0" builtinId="0"/></cellStyles></styleSheet>"#;
        crate::fixups::write_parts(&file, &[("xl/styles.xml".into(), styles.as_bytes().into())]).unwrap();
        let (mut wb, d) = load_diag(&file).unwrap();
        assert!(d.is_complete(), "{:?}", d.problems);
        let mut style = wb.styles.get(wb.sheets[0].style_of(0, 0)).clone();
        style.bold = true;
        style.italic = true;
        style.underline = true;
        style.wrap = true;
        style.indent = 2;
        style.halign = 2;
        style.size = 15.0;
        let first = wb.styles.intern(style.clone());
        wb.sheets[0].set_style(0, 0, first);
        let probe = dir.path().join("delta.xlsx");
        for pass in 1..=2 {
            if pass == 2 {
                wb.sheets[0].set_style(100, 2, first);
                style.italic = false;
                style.double_underline = true;
                style.wrap = false;
                style.shrink_to_fit = true;
                style.indent = 1;
                style.halign = 3;
                style.size = 16.0;
                let second = wb.styles.intern(style.clone());
                wb.sheets[0].set_style(0, 0, second);
            }
            assert!(write_if_supported(&file, &probe, &wb).unwrap(), "delta pass {pass}");
            let receipt = save_atomic_with_receipt(&file, &wb).unwrap();
            assert!(receipt.used_cell_delta);
            receipt.adopt_document(&mut wb, &file, true);
            let raw = String::from_utf8(read_part(&file, "xl/styles.xml").unwrap()).unwrap();
            assert_eq!(raw.matches(r#"e:keep="yes""#).count(), pass + 1);
            assert_eq!(raw.matches(r#"readingOrder="2""#).count(), pass + 1);
            assert_eq!(raw.matches(r#"<family val="2"/>"#).count(), pass + 1);
            assert!(raw.contains(&format!(r#"<cellXfs count="{}">"#, pass + 1)));
            let sheet = String::from_utf8(read_part(&file, &part).unwrap()).unwrap();
            assert!(sheet.contains(&format!(r#"<c r="A1" s="{pass}"><v>42</v></c>"#)));
            let (loaded, diag) = load_diag(&file).unwrap();
            assert!(diag.is_complete(), "{:?}", diag.problems);
            let view = &loaded.sheets[0];
            let saved = loaded.styles.get(view.style_of(0, 0));
            assert!(saved.underline);
            assert_eq!(saved.double_underline, pass == 2);
            assert_eq!(view.value(0, 0), Value::Number(42.0));
            assert!(saved.bold);
            assert_eq!(saved.italic, pass == 1);
            assert_eq!(saved.size, style.size);
            assert_eq!((saved.wrap,saved.shrink_to_fit,saved.indent,saved.halign),
                       (style.wrap,style.shrink_to_fit,style.indent,style.halign));
            if pass == 2 {
                let copied = loaded.styles.get(view.style_of(100, 2));
                assert!(copied.bold && copied.italic);
                assert_eq!(copied.size, 15.0);
            }
        }
    }

    #[test]
    fn fill_font_and_number_edits_reopen_without_losing_raw_xf_fields() {
        let (dir, file, _) = fixture(r#"<row r="1"><c r="A1"><v>42</v></c><c r="B1"><v>17</v></c></row>"#, "");
        let base = r#"<xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0" e:keep="yes"><alignment readingOrder="2"/></xf>"#;
        let template = format!(r#"<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:e="urn:keep"><fonts count="1"><font><name val="Calibri"/><sz val="11"/><family val="2"/></font></fonts><fills count="2"><fill><patternFill patternType="none"/></fill><fill><patternFill patternType="gray125"/></fill></fills><borders count="1"><border/></borders><cellStyleXfs count="1"><xf fontId="0" fillId="0" numFmtId="0"/></cellStyleXfs><cellXfs count="1">{base}</cellXfs></styleSheet>"#);
        for (kind, styles_xml) in [("missing", template.clone()), ("empty", template.replace("<fonts ", "<numFmts count=\"0\"/><fonts "))] {
            let path = dir.path().join(format!("derived-{kind}.xlsx"));
            std::fs::copy(&file, &path).unwrap();
            crate::fixups::write_parts(&path, &[("xl/styles.xml".into(), styles_xml.as_bytes().into())]).unwrap();
            let (mut wb, diag) = load_diag(&path).unwrap();
            assert!(diag.is_complete());
            let custom = "0.000\" & units\"";
            for (fill, color, strike, format) in [
                (Some([12,34,56]), [21,43,65], true, custom),
                (None, [0,0,0], false, "0.00%"),
            ] {
                let mut s = wb.styles.get(wb.sheets[0].style_of(0,0)).clone();
                s.fill = fill;
                s.color = color;
                s.strikethrough = strike;
                s.format = format.into();
                let id = wb.styles.intern(s);
                wb.sheets[0].set_style(0,0,id);
                let receipt = save_atomic_with_receipt(&path, &wb).unwrap();
                assert!(receipt.used_cell_delta, "{kind}: {format}");
                receipt.adopt_document(&mut wb, &path, true);
                let raw = String::from_utf8(read_part(&path,"xl/styles.xml").unwrap()).unwrap();
                assert!(raw.contains(base));
                let doc = roxmltree::Document::parse(&raw).unwrap();
                let xfs = doc.descendants().find(|n| n.tag_name().name() == "cellXfs").unwrap();
                assert!(xfs.children().filter(|n| n.is_element()).all(|xf|
                    xf.attribute(("urn:keep","keep")) == Some("yes") &&
                    xf.children().any(|n| n.attribute("readingOrder") == Some("2"))));
                assert_eq!(doc.descendants().filter(|n| n.attribute("formatCode") == Some(custom)).count(), 1);
                let (view, diag) = load_diag(&path).unwrap();
                assert!(diag.is_complete(), "{:?}", diag.problems);
                let style = view.styles.get(view.sheets[0].style_of(0,0));
                assert_eq!((style.fill,style.color,style.strikethrough,style.format.as_str()), (fill,color,strike,format));
                assert_eq!(view.sheets[0].value(0,1), Value::Number(17.0));
                assert_eq!(view.styles.get(view.sheets[0].style_of(0,1)).fill, None);
                if kind == "empty" {
                    // The next edit starts from the actual file's style map.
                    wb = view;
                }
            }
        }
    }

    #[test]
    fn derived_xfs_preserve_border_extensions_and_theme_pattern_colors() {
        use crate::style_ext::FillPattern;
        use crate::style_theme::{ThemeRef, XfTheme};
        let (dir, path, part) = fixture(r#"<row r="1"><c r="A1" s="0"><v>3</v></c></row>"#, "");
        let seed = r#"<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:e="urn:vendor">
<fonts count="1"><font><name val="Calibri"/><family val="2"/><color theme="1"/><sz val="11"/><scheme val="minor"/></font></fonts>
<fills count="1"><fill><patternFill patternType="none"/></fill></fills>
<borders count="1"><border outline="0"><right e:keep="yes"/><diagonal/></border></borders>
<cellStyleXfs count="1"><xf/></cellStyleXfs><cellXfs count="1"><xf fontId="0" fillId="0" borderId="0" numFmtId="0" xfId="0" e:vendor="v"><alignment readingOrder="2"/></xf></cellXfs>
</styleSheet>"#;
        crate::fixups::write_parts(&path, &[("xl/styles.xml".into(), seed.as_bytes().to_vec())])
            .unwrap();
        let mut wb = load_diag(&path).unwrap().0;
        let base = wb.sheets[0].style_of(0, 0);
        let mut style = wb.styles.get(base).clone();
        style.font = "A & B".into();
        style.valign = 2;
        style.rotation = -30;
        style.border[0] = crate::style::Border {
            style: 1,
            color: [10, 20, 30],
        };
        style.fill = Some([31, 41, 51]);
        let id = wb.styles.intern(style);
        let font = ThemeRef { idx: 4, tint: 0.25 };
        wb.styles.set_theme(
            id,
            XfTheme {
                font: Some(font),
                fill: None,
            },
        );
        wb.styles.set_pattern(
            id,
            FillPattern {
                kind: "darkTrellis".into(),
                fg: Some([31, 41, 51]),
                fg_theme: None,
                fg_indexed: None,
                bg: None,
                bg_theme: Some((5, -0.25)),
                bg_indexed: None,
            },
        );
        wb.sheets[0].set_style(0, 0, id);
        let target = dir.path().join("styled.xlsx");
        assert!(write_if_supported(&path, &target, &wb).unwrap());
        let encoded = String::from_utf8(read_part(&target, "xl/styles.xml").unwrap()).unwrap();
        let doc = roxmltree::Document::parse(&encoded).unwrap();
        let last = |family| {
            doc.root_element()
                .children()
                .find(|e| e.tag_name().name() == family)
                .unwrap()
                .last_element_child()
                .unwrap()
        };
        let new_border = last("borders");
        assert_eq!(new_border.attribute("outline"), Some("0"));
        assert_eq!(
            new_border
                .children()
                .find(|e| e.tag_name().name() == "right")
                .unwrap()
                .attribute(("urn:vendor", "keep")),
            Some("yes")
        );
        let left = new_border.first_element_child().unwrap();
        assert_eq!(left.tag_name().name(), "left");
        assert_eq!(left.attribute("style"), Some("thin"));
        assert_eq!(
            left.first_element_child().unwrap().attribute("rgb"),
            Some("FF0A141E")
        );
        assert_eq!(
            last("cellXfs").attribute(("urn:vendor", "vendor")),
            Some("v")
        );
        assert_eq!(
            last("cellXfs")
                .first_element_child()
                .unwrap()
                .attribute("readingOrder"),
            Some("2")
        );
        assert_eq!(
            last("cellXfs")
                .first_element_child()
                .unwrap()
                .attribute("textRotation"),
            Some("120")
        );
        let xml = String::from_utf8(read_part(&target, &part).unwrap()).unwrap();
        assert!(xml.contains(r#"s="1""#));
        let back = load_diag(&target).unwrap().0;
        let sid = back.sheets[0].style_of(0, 0);
        assert_eq!(
            (
                back.styles.get(sid).font.as_str(),
                back.styles.get(sid).rotation,
                back.styles.get(sid).valign
            ),
            ("A & B", -30, 2)
        );
        assert_eq!(back.styles.theme_of(sid).font, Some(font));
        assert_eq!(back.styles.pattern_of(sid), wb.styles.pattern_of(id));
    }

    #[test]
    fn format_of_a_fresh_default_cell_can_be_changed_after_reopening() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("default-format.xlsx");
        let mut wb = Workbook::new_blank();
        wb.sheets[0].set(0,0,num(42.0));
        save(&file, &wb).unwrap();
        for (fill, code) in [(Some([12,34,56]), "0.000"), (None, "0.00%")] {
            let mut wb = load_diag(&file).unwrap().0;
            let original = wb.sheets[0].style_of(0,0);
            let mut style = wb.styles.get(original).clone();
            style.fill = fill;
            style.format = code.into();
            let id = wb.styles.intern(style);
            wb.sheets[0].set_style(0,0,id);
            // Explicitly removing the original theme-backed font is a different edit.
            let theme = wb.styles.theme_of(original);
            if theme != wb.styles.theme_of(id) {
                assert!(write_if_supported(&file, &dir.path().join("fixed-color.xlsx"), &wb).unwrap());
                wb.styles.set_theme(id, theme);
            }
            assert!(save_atomic_with_receipt(&file, &wb).unwrap().used_cell_delta, "{code}");
            let wb = load_diag(&file).unwrap().0;
            let saved = wb.styles.get(wb.sheets[0].style_of(0,0));
            assert_eq!((saved.fill,saved.format.as_str()),(fill,code));
        }
    }

    #[test]
    fn first_save_receipt_supports_font_change_without_reopening() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("created.xlsx");
        let mut wb = Workbook::new_blank();
        wb.sheets[0].set(0, 0, num(13.0));
        save_atomic_with_receipt(&file, &wb).unwrap().adopt_document(&mut wb, &file, true);
        let mut style = wb.styles.get(0).clone();
        style.bold = true;
        let id = wb.styles.intern(style);
        wb.sheets[0].set_style(0, 0, id);
        let receipt = save_atomic_with_receipt(&file, &wb).unwrap();
        assert!(receipt.used_cell_delta);
        let (back, diag) = load_diag(&file).unwrap();
        assert!(diag.is_complete());
        assert!(back.styles.get(back.sheets[0].style_of(0, 0)).bold);
        assert_eq!(back.sheets[0].value(0, 0), Value::Number(13.0));
    }

    #[test]
    fn generated_xf_is_reused_only_on_its_verified_source() {
        let rows = r#"<row r="1"><c r="A1"><v>1</v></c><c r="B1" s="1"><v>2</v></c></row>"#;
        let (dir, file, _) = fixture(rows, "");
        let raw = r#"<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:e="urn:test">
<fonts count="1"><font><sz val="11"/><name val="Calibri"/></font></fonts>
<cellStyleXfs count="1"><xf fontId="0"/></cellStyleXfs>
<cellXfs count="2"><xf fontId="0"/><xf fontId="0" e:keep="other"/></cellXfs></styleSheet>"#;
        crate::fixups::write_parts(&file, &[("xl/styles.xml".into(), raw.as_bytes().into())]).unwrap();
        let (mut wb, d) = load_diag(&file).unwrap();
        assert!(d.is_complete());
        let mut style = wb.styles.get(wb.sheets[0].style_of(0, 0)).clone();
        style.bold = true;
        let id = wb.styles.intern(style);
        wb.sheets[0].set_style(0, 0, id);
        let receipt = save_atomic_with_receipt(&file, &wb).unwrap();
        assert!(receipt.used_cell_delta);
        receipt.adopt_document(&mut wb, &file, true);
        wb.sheets[0].set_style(0, 1, id);
        let target = dir.path().join("different-source.xlsx");
        assert!(!write_if_supported(&file, &target, &wb).unwrap());
        assert!(!target.exists());
    }

    #[test]
    fn new_font_xf_rejects_ambiguous_raw_sources() {
        let rows = r#"<row r="1"><c r="A1" s="1"><v>1</v></c><c r="B1" s="2"><v>2</v></c></row>"#;
        let (dir, file, _) = fixture(rows, "");
        let style = r#"<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:e="urn:test">
<fonts count="1"><font><name val="Calibri"/><sz val="11"/></font></fonts>
<fills count="1"><fill><patternFill patternType="none"/></fill></fills>
<borders count="1"><border/></borders>
<cellStyleXfs count="1"><xf fontId="0"/></cellStyleXfs>
<cellXfs count="3"><xf fontId="0"/><xf fontId="0" e:tag="left"/><xf fontId="0" e:tag="right"/></cellXfs></styleSheet>"#;
        crate::fixups::write_parts(&file, &[("xl/styles.xml".into(), style.as_bytes().into())]).unwrap();
        let (mut wb, d) = load_diag(&file).unwrap();
        assert!(d.is_complete());
        let source = wb.sheets[0].style_of(0, 0);
        assert_eq!(wb.sheets[0].style_of(0, 1), source);
        let mut style = wb.styles.get(source).clone();
        style.italic = true;
        let id = wb.styles.intern(style);
        wb.sheets[0].set_style(0, 0, id);
        let target = dir.path().join("ambiguous.xlsx");
        assert!(!write_if_supported(&file, &target, &wb).unwrap());
        assert!(!target.exists());

        // With no cell style attribute the model uses its own default. A
        // workbook can assign different font properties to raw XF zero.
        let (dir, file, _) = fixture(r#"<row r="1"><c r="A1"><v>3</v></c></row>"#, "");
        let raw = r#"<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
<fonts count="1"><font><sz val="25"/><i/></font></fonts>
<cellStyleXfs count="1"><xf fontId="0"/></cellStyleXfs>
<cellXfs count="1"><xf fontId="0"/></cellXfs></styleSheet>"#;
        crate::fixups::write_parts(&file, &[("xl/styles.xml".into(), raw.as_bytes().into())]).unwrap();
        let (mut wb, d) = load_diag(&file).unwrap();
        assert!(d.is_complete());
        let mut style = wb.styles.get(wb.sheets[0].style_of(0, 0)).clone();
        assert_ne!(style.size, 25.0);
        style.bold = true;
        let id = wb.styles.intern(style);
        wb.sheets[0].set_style(0, 0, id);
        let target = dir.path().join("wrong-default.xlsx");
        assert!(!write_if_supported(&file, &target, &wb).unwrap());
        assert!(!target.exists());
    }

    #[test]
    fn array_and_spill_regions_protect_only_affected_coordinates_and_shape_changes() {
        let rows = r#"<row r="1"><c r="A1"><f t="array" ref="A1:A2">ROW(A1:A2)</f><v>1</v></c></row><row r="2"><c r="A2"><v>2</v></c></row>"#;
        let (dir, path, part) = fixture(rows, "");
        let (mut wb, _) = load_diag(&path).unwrap();
        assert_eq!(wb.sheets[0].arrays.len(), 1);
        let out = dir.path().join("array.xlsx");
        wb.sheets[0].set(25, 2, num(23.0));
        assert!(write_if_supported(&path, &out, &wb).unwrap());
        let xml = String::from_utf8(read_part(&out, &part).unwrap()).unwrap();
        assert!(xml.contains(rows));
        wb.sheets[0].merge_range(1, 0, 1, 1);
        assert!(!write_if_supported(&path, &out, &wb).unwrap());
        wb.sheets[0].unmerge_range(1, 0, 1, 1);
        wb.sheets[0].set(1, 0, num(8.0));
        assert!(!write_if_supported(&path, &out, &wb).unwrap());
        wb.sheets[0].set(1, 0, num(2.0));
        wb.sheets[0].spill_shapes.insert(CellRef::new(8, 3), (2, 1));
        wb.sheets[0].spilled.insert(CellRef::new(9, 3));
        assert!(!write_if_supported(&path, &out, &wb).unwrap());
        let mut zip = ZipArchive::new(File::open(&path).unwrap()).unwrap();
        wb.sheets[0].source_cells = SourceCells::capture(&mut zip, &part, &wb.sheets[0]);
        wb.sheets[0].set(26, 2, num(24.0));
        assert!(write_if_supported(&path, &out, &wb).unwrap());
        wb.sheets[0].set(9, 3, num(8.0));
        assert!(!write_if_supported(&path, &out, &wb).unwrap());
    }

    #[test]
    fn large_edit_above_old_cell_limit_uses_delta() {
        let (dir, path, _) = fixture("", "");
        let (mut wb, _) = load_diag(&path).unwrap();
        for r in 0..20_000 {
            wb.sheets[0].set(r, 1, num(r as f64));
        }
        let out = dir.path().join("many.xlsx");
        assert!(write_if_supported(&path, &out, &wb).unwrap());
        let (wb, d) = load_diag(&out).unwrap();
        assert!(d.is_complete());
        assert_eq!(wb.sheets[0].len(), 20_000);
        assert_eq!(wb.sheets[0].value(19_999, 1), Value::Number(19_999.0));
    }

    #[test]
    fn formula_edit_drops_actual_calc_chain_and_preserves_unrelated_relationships() {
        let (dir, path, part) = fixture(r#"<row r="1"><c r="A1"><f>1+1</f><v>2</v></c></row>"#, "");
        let rels_name = "xl/_rels/workbook.xml.rels";
        let ct = "[Content_Types].xml";
        let chain = "xl/calculation/custom.xml";
        let rels=String::from_utf8(read_part(&path,rels_name).unwrap()).unwrap().replace("</Relationships>",
            r#"<Relationship Id="rIdCalc" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/calcChain" Target="calculation/custom.xml"/></Relationships>"#);
        let types=String::from_utf8(read_part(&path,ct).unwrap()).unwrap().replace("</Types>",
            r#"<Override PartName="/xl/calculation/custom.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.calcChain+xml"/></Types>"#);
        crate::fixups::write_parts(&path,&[(rels_name.into(),rels.into_bytes()),(ct.into(),types.into_bytes()),(chain.into(),br#"<calcChain xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><c r="A1" i="1"/></calcChain>"#.to_vec())]).unwrap();
        let (mut wb, d) = load_diag(&path).unwrap();
        assert!(d.is_complete());
        let out = dir.path().join("edited.xlsx");
        wb.sheets[0].get_mut(0, 0).unwrap().value = Value::Number(3.0);
        assert!(write_if_supported(&path, &out, &wb).unwrap());
        assert_eq!(
            read_part(&path, chain).unwrap(),
            read_part(&out, chain).unwrap()
        );
        wb.sheets[0].set(
            0,
            0,
            Cell {
                formula: Some("=1+3".into()),
                value: Value::Number(4.0),
            },
        );
        assert!(write_if_supported(&path, &out, &wb).unwrap());
        save_atomic_with_receipt(&out, &wb).unwrap();
        let norel = String::from_utf8(read_part(&out, rels_name).unwrap()).unwrap();
        let noct = String::from_utf8(read_part(&out, ct).unwrap()).unwrap();
        assert!(
            read_part(&out, chain).is_none()
                && !norel.contains("/calcChain")
                && !noct.contains("calcChain+xml")
        );
        assert!(norel.contains("/worksheet") && noct.contains("/xl/workbook.xml"));
        let workbook = String::from_utf8(read_part(&out, "xl/workbook.xml").unwrap()).unwrap();
        assert!(
            workbook.contains("calcId=\"0\"") && workbook.contains("fullCalcOnLoad=\"1\""),
            "{workbook}"
        );
        let (wb, _) = load_diag(&out).unwrap();
        assert_eq!(
            wb.sheets[0].get(0, 0).unwrap().formula.as_deref(),
            Some("=1+3")
        );
        assert!(
            String::from_utf8(read_part(&out, &part).unwrap())
                .unwrap()
                .contains("<f>1+3</f>")
        );
    }

    #[test]
    fn modern_formula_prefix_is_not_an_encoded_character() {
        assert!(xml_text("_x000D_").is_none());
        let (dir, path, part) = fixture("", "");
        let (mut wb, _) = load_diag(&path).unwrap();
        wb.sheets[0].set(
            0,
            0,
            Cell {
                formula: Some("=XLOOKUP(1,B1:B3,C1:C3)".into()),
                value: Value::Number(4.0),
            },
        );
        let out = dir.path().join("modern.xlsx");
        assert!(write_if_supported(&path, &out, &wb).unwrap());
        let xml = String::from_utf8(read_part(&out, &part).unwrap()).unwrap();
        assert!(xml.contains("<f>_xlfn.XLOOKUP(1,B1:B3,C1:C3)</f>"), "{xml}");
    }

    #[test]
    fn prefixed_worksheet_cell_changes_use_original_namespace_and_keep_extension_bytes() {
        let (dir,path,part) = fixture("", "");
        let extension = r#"<x:extLst><x:ext uri="test"><setting xmlns="urn:vendor"><x:label>xx</x:label></setting></x:ext></x:extLst>"#;
        let xml = format!(r#"<x:worksheet xmlns:x="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns="urn:default"><x:dimension ref="A1"/><x:sheetData><x:row r='1'><x:c r="A1"><x:v>2</x:v></x:c></x:row></x:sheetData>{extension}</x:worksheet>"#);
        crate::patch::append_entry(&path,&part,xml.as_bytes()).unwrap();
        let (mut wb,diag) = load_diag(&path).unwrap();
        assert!(diag.is_complete());
        wb.sheets[0].set(0,0,num(5.));
        wb.sheets[0].set(2,1,Cell { value: Value::Text("x:c".into()), formula:None });
        let out = dir.path().join("prefixed.xlsx");
        let receipt = save_atomic_with_receipt(&out,&wb).unwrap();
        assert!(receipt.used_cell_delta);
        let saved = String::from_utf8(read_part(&out,&part).unwrap()).unwrap();
        assert!(saved.contains(extension));
        let doc = roxmltree::Document::parse(&saved).unwrap();
        let cells: Vec<_> = doc.descendants().filter(|e| e.tag_name().name()=="c").collect();
        assert_eq!(cells.len(),2);
        assert!(cells.iter().flat_map(|c| c.descendants()).filter(|n| n.is_element())
            .all(|n| n.tag_name().namespace()==Some("http://schemas.openxmlformats.org/spreadsheetml/2006/main")));
        let back = load_diag(&out).unwrap().0;
        assert_eq!(back.sheets[0].value(0,0),Value::Number(5.));
        assert_eq!(back.sheets[0].value(2,1),Value::Text("x:c".into()));
    }

    #[test]
    fn fixed_array_cache_edits_preserve_definition_and_decline_tail_input() {
        let rows = r#"<row r="1"><c r="A1"><f t="array" ref="A1:A2" aca="1">B1:B2*2</f><v>4</v></c><c r="B1"><v>2</v></c></row><row r="2"><c r="A2"><v>6</v></c><c r="B2"><v>3</v></c></row>"#;
        let (dir,path,part) = fixture(rows,"");
        let (mut wb,diag) = load_diag(&path).unwrap();
        assert!(diag.is_complete());
        wb.sheets[0].set(0,1,num(5.));
        wb.sheets[0].set(1,1,num(7.));
        crate::formula::recalc_all(&mut wb);
        assert_eq!(wb.sheets[0].value(1,0),Value::Number(14.));
        let out = dir.path().join("fixed-cache.xlsx");
        let receipt = save_atomic_with_receipt(&out,&wb).unwrap();
        assert!(receipt.used_cell_delta);
        let xml = String::from_utf8(read_part(&out,&part).unwrap()).unwrap();
        assert!(xml.contains(r#"<f t="array" ref="A1:A2" aca="1">B1:B2*2</f><v>10</v>"#));
        let reopened = load_diag(&out).unwrap().0;
        assert_eq!(reopened.sheets[0].value(1,0),Value::Number(14.));
        assert_eq!(reopened.sheets[0].arrays,wb.sheets[0].arrays);
        receipt.adopt_document(&mut wb,&out,true);
        wb.sheets[0].set(1,0,num(99.));
        assert!(!write_if_supported(&out,&dir.path().join("override.xlsx"),&wb).unwrap());
    }

    #[test]
    fn changing_a_shared_formula_master_or_child_uses_delta_and_promotes_next_child() {
        let rows = r#"<row r="1"><c r="A1"><f t="shared" si="0" ref="A1:A3">B1+1</f><v>2</v></c></row><row r="2"><c r="A2"><f t="shared" si="0"/><v>3</v></c></row><row r="3"><c r="A3"><f t="shared" si="0"/><v>4</v></c></row>"#;
        let (dir, path, part) = fixture(rows, "");
        let (mut wb, diag) = load_diag(&path).unwrap();
        assert!(diag.is_complete());
        wb.sheets[0].get_mut(0, 0).unwrap().value = Value::Number(11.0);
        let target = dir.path().join("cache.xlsx");
        assert!(write_if_supported(&path, &target, &wb).unwrap());
        let xml = String::from_utf8(read_part(&target, &part).unwrap()).unwrap();
        assert!(xml.contains(r#"<f t="shared" si="0" ref="A1:A3">B1+1</f><v>11</v>"#));
        wb.sheets[0].get_mut(1, 0).unwrap().value = Value::Number(18.0);
        assert!(write_if_supported(&path, &target, &wb).unwrap());
        let xml = String::from_utf8(read_part(&target, &part).unwrap()).unwrap();
        assert!(xml.contains(r#"<f t="shared" si="0"/><v>18</v>"#));

        // Overwrite child A2 with a literal -> A2 becomes normal cell via delta!
        wb.sheets[0].set(1, 0, num(99.0));
        let child_out = dir.path().join("child-edit.xlsx");
        assert!(write_if_supported(&path, &child_out, &wb).unwrap());
        let back_child = load_diag(&child_out).unwrap().0;
        assert_eq!(back_child.sheets[0].value(1, 0), Value::Number(99.0));
        assert_eq!(back_child.sheets[0].get(2, 0).unwrap().formula.as_deref(), Some("=B3+1"));

        // Overwrite master A1 while A2 is literal -> A3 is promoted to master!
        wb.sheets[0].set(0, 0, num(4.0));
        assert!(write_if_supported(&path, &target, &wb).unwrap());
        let xml = String::from_utf8(read_part(&target, &part).unwrap()).unwrap();
        assert!(xml.contains(r#"<f t="shared" ref="A3" si="0">B3+1</f>"#), "{xml}");
        let reopened = load_diag(&target).unwrap().0;
        assert_eq!(reopened.sheets[0].value(0, 0), Value::Number(4.0));
        assert_eq!(reopened.sheets[0].value(1, 0), Value::Number(99.0));
        assert_eq!(reopened.sheets[0].get(2, 0).unwrap().formula.as_deref(), Some("=B3+1"));
    }

    #[test]
    fn cell_protection_new_dynamic_spill_and_layout_aux_edits_use_delta() {
        let (dir, path, _) = fixture("<row r='1'><c r='A1'><v>10</v></c></row>", "");
        let (mut wb, _) = load_diag(&path).unwrap();
        // 1. Cell protection (locked=false, hidden=true) uses font_delta + xlsx_delta
        let mut st = wb.styles.get(wb.sheets[0].style_of(0, 0)).clone();
        st.locked = false;
        st.hidden = true;
        let sid = wb.styles.intern(st);
        wb.sheets[0].set_style(0, 0, sid);
        // 2. Brand-new dynamic array formula injects xl/metadata.xml in delta path
        wb.sheets[0].set(
            0,
            1,
            Cell {
                formula: Some("=SEQUENCE(3)".into()),
                value: Value::Empty,
            },
        );
        crate::formula::recalc_all(&mut wb);
        // 3. Layout, freeze, filter, and page setup dirty flags stream in delta path
        wb.sheets[0].rows.set(0, 40.0);
        wb.sheets[0].cols.set(1, 120.0);
        wb.sheets[0].layout_dirty = true;
        wb.sheets[0].freeze = Some((1, 1));
        wb.sheets[0].freeze_dirty = true;
        wb.sheets[0].filter = Some(crate::sort_filter::Filter::new(0, 0, 2, 1));
        wb.sheets[0].filter_dirty = true;
        wb.sheets[0].page.landscape = true;
        wb.sheets[0].page.print_area = Some((0, 0, 2, 1));
        wb.sheets[0].page_dirty = true;

        let out = dir.path().join("all-delta.xlsx");
        let receipt = save_atomic_with_receipt(&out, &wb).unwrap();
        assert!(receipt.used_cell_delta);
        let (back, diag) = load_diag(&out).unwrap();
        assert!(diag.is_complete());
        let s0 = &back.sheets[0];
        let back_st = back.styles.get(s0.style_of(0, 0));
        assert!(!back_st.locked && back_st.hidden);
        assert_eq!(s0.spill_shapes.get(&CellRef::new(0, 1)), Some(&(3, 1)));
        assert_eq!(s0.value(2, 1), Value::Number(3.0));
        assert_eq!(s0.rows.size(0), 40.0);
        assert!((s0.cols.size(1) - 120.0).abs() < 1.0);
        assert_eq!(s0.freeze, Some((1, 1)));
        assert_eq!(s0.filter.as_ref().unwrap().range, (0, 0, 2, 1));
        assert!(s0.page.landscape);
        assert_eq!(s0.page.print_area, Some((0, 0, 2, 1)));
    }

    #[test]
    fn high_dirty_parallel_rewrite_sst_header_precheck_and_trusted_parts_roundtrip() {
        // 1. SST root count attribute header precheck
        assert_eq!(
            sst_root_has_count_attrs(
                b"<?xml version=\"1.0\"?><sst xmlns=\"u\" count=\"5\" uniqueCount=\"3\"><si><t>a</t></si></sst>"
                    .as_slice()
            )
            .unwrap(),
            Some(true)
        );
        assert_eq!(
            sst_root_has_count_attrs(
                b"<?xml version=\"1.0\"?><sst xmlns=\"u\"><si><t>a</t></si></sst>".as_slice()
            )
            .unwrap(),
            Some(false)
        );

        // 2. High-dirty sheet (>= 16,384 dirty cells) triggers parallel rewrite inside xlsx_delta
        // while preserving `<extLst>` outside `<sheetData>` and raw-copying untouched Sheet2.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("high-dirty-chain.xlsx");
        let mut wb = Workbook::new_blank();
        wb.sheets[0].set(0, 0, num(1.0));
        for r in 1..20_000u32 {
            wb.sheets[0].set(
                r,
                0,
                Cell {
                    value: Value::Number(f64::from(r + 1)),
                    formula: Some(format!("=A{r}+1").into()),
                },
            );
        }
        let mut sh2 = Sheet::new("Sheet2");
        sh2.set(
            0,
            0,
            Cell {
                value: Value::Text("保留不变".into()),
                formula: None,
            },
        );
        wb.sheets.push(sh2);
        save(&path, &wb).unwrap();

        // Inject an unmodeled `<extLst>` footer into sheet1.xml to prove shell preservation.
        let s1_part = crate::patch::sheet_targets(&path).unwrap()["Sheet1"].clone();
        let mut raw_s1 = String::from_utf8(read_part(&path, &s1_part).unwrap()).unwrap();
        let close = raw_s1.rfind("</worksheet>").unwrap();
        raw_s1.insert_str(close, "<extLst><ext uri=\"{12345678-1234-1234-1234-1234567890AB}\"/></extLst>");
        crate::fixups::write_parts(&path, &[(s1_part.clone(), raw_s1.into_bytes())]).unwrap();

        let (mut loaded, _) = load_diag(&path).unwrap();
        loaded.sheets[0].set(0, 0, num(100.0));
        crate::formula::recalc_all(&mut loaded);

        let out = dir.path().join("high-dirty-saved.xlsx");
        let receipt = save_atomic_with_receipt(&out, &loaded).unwrap();
        assert!(receipt.used_cell_delta);

        let saved_s1 = String::from_utf8(read_part(&out, &s1_part).unwrap()).unwrap();
        assert!(saved_s1.contains("{12345678-1234-1234-1234-1234567890AB}"));

        let (back, diag) = load_diag(&out).unwrap();
        assert!(diag.is_complete());
        assert_eq!(back.sheets[0].value(0, 0), Value::Number(100.0));
        assert_eq!(back.sheets[0].value(19_999, 0), Value::Number(20_099.0));
        assert_eq!(back.sheets[1].value(0, 0), Value::Text("保留不变".into()));
    }
}
