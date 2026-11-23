//! Patch ordinary cell edits against the exact worksheet captured during load
//! or save. Unsupported cell representations use the full serializer.
use crate::model::{Cell, CellRef, Sheet, Value, Workbook};
use crate::store::{CellStyles, Store};
use crate::style::StyleTable;
use anyhow::{Context, Result};
use quick_xml::{
    Reader,
    events::{BytesStart, Event},
};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fs::File,
    io::Write,
    path::Path,
    sync::Arc,
};
use zip::{ZipArchive, ZipWriter, write::SimpleFileOptions};

#[derive(Clone, Debug)]
pub(crate) struct SourceCells {
    store: Arc<Store>,
    signature: (u32, u64),
    strings: Option<(u32, u64)>,
    style_part: Option<(u32, u64)>,
    style_map: Option<Arc<StyleMap>>,
    notes: Option<NoteSource>,
    styles: CellStyles,
    arrays: Arc<Vec<(CellRef, Rect)>>,
    spill_shapes: Arc<HashMap<CellRef, (u32, u32)>>,
    spilled: Arc<HashSet<CellRef>>,
}

type Rect = (u32, u32, u32, u32);
type StyleChanges = HashMap<CellRef, Option<u16>>;

#[derive(Clone, Debug)]
pub(super) struct StyleMap {
    // Only a unique original cellXf can be reused: distinct raw XFs may
    // collapse to one modeled style (e.g. unmodeled alignment properties).
    xfs: HashMap<u16, Option<u16>>,
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
            original: styles.clone(),
        })
    }

    fn xf(&self, id: u16, current: &StyleTable) -> Option<u16> {
        let old = &self.original;
        (usize::from(id) < current.len()
            && old.get(id) == current.get(id)
            && old.theme_of(id) == current.theme_of(id)
            && old.pattern_of(id) == current.pattern_of(id)
            && old.diag_of(id) == current.diag_of(id)
            && old.quote_of(id) == current.quote_of(id))
        .then(|| self.xfs.get(&id).copied().flatten())
        .flatten()
    }
}

fn signature(zip: &mut ZipArchive<File>, part: &str) -> Option<(u32, u64)> {
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
    fn matches_legacy_notes(&self, zip: &mut ZipArchive<File>, sheet: &Sheet) -> bool {
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

    fn capture(zip: &mut ZipArchive<File>, part: &str, prior: Option<&Self>) -> Result<Self> {
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
    pub(super) fn set_style_map(&mut self, styles: Arc<StyleMap>) {
        self.style_map = Some(styles);
    }

    pub(super) fn capture(zip: &mut ZipArchive<File>, part: &str, sheet: &Sheet) -> Option<Self> {
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
        Some(Self {
            store,
            notes,
            signature: signature(zip, part)?,
            strings: signature(zip, "xl/sharedStrings.xml"),
            style_part,
            style_map,
            styles: sheet.styles.clone(),
            arrays: Arc::new(sheet.arrays.clone()),
            spill_shapes: Arc::new(sheet.spill_shapes.clone()),
            spilled: Arc::new(sheet.spilled.clone()),
        })
    }
}

struct Changes {
    keys: BTreeSet<CellRef>,
    styles: StyleChanges,
    formula_changed: bool,
}

fn intersects(keys: &BTreeSet<CellRef>, (top, left, bottom, right): Rect) -> bool {
    if top > bottom || left > right {
        return true;
    }
    keys.range(CellRef::new(top, 0)..=CellRef::new(bottom, u32::MAX))
        .any(|k| (left..=right).contains(&k.col))
}

fn coordinates(sheet: &Sheet, source: &SourceCells, styles: &StyleTable) -> Option<Changes> {
    let base = &source.store;
    let mut keys = BTreeSet::new();
    let mut formula_changed = false;
    for key in sheet
        .cells()
        .changed_keys(base)
        .chain(base.changed_keys(sheet.cells()))
    {
        if !keys.insert(key) {
            continue;
        }
        if sheet.rich.contains_key(&(key.row, key.col)) {
            return None;
        }
        formula_changed |= base
            .get(key.row, key.col)
            .and_then(|c| c.formula.as_deref())
            != sheet
                .get(key.row, key.col)
                .and_then(|c| c.formula.as_deref());
    }
    // The fallback owns changes to array formulas, cached spill results and their footprints.
    // An unrelated value edit in the same sheet never needs to materialize these regions.
    if sheet.arrays != *source.arrays
        || sheet.spill_shapes != *source.spill_shapes
        || sheet.spilled != *source.spilled
        || sheet
            .arrays
            .iter()
            .any(|(_, rect)| intersects(&keys, *rect))
        || keys
            .iter()
            .any(|k| sheet.spilled.contains(k) || sheet.spill_shapes.contains_key(k))
    {
        return None;
    }
    let mut changed_styles = StyleChanges::new();
    if sheet.styles_dirty || !sheet.style_edits.is_empty() {
        for key in sheet
            .styles
            .changed_keys(&source.styles)
            .chain(source.styles.changed_keys(&sheet.styles))
        {
            let value = match sheet.explicit_style_of(key.row, key.col) {
                Some(id) => Some(source.style_map.as_ref()?.xf(id, styles)?),
                None => None,
            };
            changed_styles.insert(key, value);
            keys.insert(key);
        }
    }
    Some(Changes {
        keys,
        styles: changed_styles,
        formula_changed,
    })
}

fn attr<'a>(e: &'a BytesStart<'a>, key: &str) -> Option<String> {
    e.attributes()
        .flatten()
        .find(|a| a.key.as_ref() == key)
        .map(|a| a.value.into_owned())
}

fn xml_text(text: &str) -> Option<String> {
    if text.chars().any(|c| c < ' ' && c != '\t' && c != '\n')
        // A literal _xHHHH_ requires SpreadsheetML-specific escaping; ordinary
        // identifiers such as _xlfn.XLOOKUP contain no encoded character.
        || text.as_bytes().windows(7).any(|w| w.starts_with(b"_x") && w[6] == b'_' && w[2..6].iter().all(u8::is_ascii_hexdigit))
        || text.contains('\u{fffe}')
        || text.contains('\u{ffff}')
    {
        return None;
    }
    Some(quick_xml::escape::escape(text).into_owned())
}

/// Leave Excel's formula definition intact on cache-only writes, including
/// shared formula anchors and derived cells. Preserve the explicit style ID.
fn replacement(
    key: CellRef,
    start: &BytesStart<'_>,
    body: &str,
    old: Option<&Cell>,
    new: Option<&Cell>,
    styles: &StyleChanges,
) -> Option<String> {
    let mut style = None;
    for a in start.attributes() {
        let a = a.ok()?;
        match a.key.as_ref() {
            "r" | "t" => {}
            "s" => style = Some(a.value.parse::<u32>().ok()?),
            _ => return None,
        }
    }
    if let Some(&xf) = styles.get(&key) {
        style = xf.map(u32::from);
    }
    let mut xml = Reader::from_str(body);
    let mut definition = "";
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
                    // A what-if table anchor, for example, has an OOXML formula
                    // definition even though the model only knows its cached number.
                    if old.and_then(|c| c.formula.as_ref()).is_none() {
                        return None;
                    }
                    if old.and_then(|c| c.formula.as_ref()) != new.and_then(|c| c.formula.as_ref())
                        && e.attributes().next().is_some()
                    {
                        return None;
                    }
                    definition = &body[offset..xml.buffer_position() as usize];
                }
            }
            Event::Text(t) if t.as_ref().trim().is_empty() => {}
            Event::Eof => break,
            _ => return None,
        }
    }
    let reference = crate::model::cell_name(key.row, key.col);
    let mut out = format!("<c r=\"{reference}\"");
    if let Some(style) = style {
        out.push_str(&format!(" s=\"{style}\""));
    }
    let same_formula = old.and_then(|c| c.formula.as_ref()) == new.and_then(|c| c.formula.as_ref());

    let Some(cell) = new else {
        return Some(if style.is_some() {
            format!("{out}/>")
        } else {
            String::new()
        });
    };
    let (kind, value) = match &cell.value {
        Value::Empty => ("", String::new()),
        Value::Number(n) if n.is_finite() => ("", format!("<v>{n}</v>")),
        Value::Number(_) => return None,
        Value::Bool(b) => ("b", format!("<v>{}</v>", u8::from(*b))),
        Value::Error(crate::formula::ErrKind::Spill) => return None,
        Value::Error(e) => ("e", format!("<v>{}</v>", e.text())),
        Value::Text(s) if cell.formula.is_some() => ("str", format!("<v>{}</v>", xml_text(s)?)),
        Value::Text(s) => (
            "inlineStr",
            format!("<is><t xml:space=\"preserve\">{}</t></is>", xml_text(s)?),
        ),
    };
    if !kind.is_empty() {
        out.push_str(&format!(" t=\"{kind}\""));
    }
    out.push('>');
    if let Some(f) = &cell.formula {
        if same_formula {
            if definition.is_empty() {
                return None;
            }
            out.push_str(definition);
        } else {
            let f = crate::add_fn_prefixes(f.strip_prefix('=').unwrap_or(f));
            out.push_str(&format!("<f>{}</f>", xml_text(&f)?));
        }
    }
    out.push_str(&value);
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
    replacement(
        key,
        &BytesStart::new("c"),
        "",
        None,
        sheet.get(key.row, key.col),
        styles,
    )
}

fn insert_missing_rows(
    before: u32,
    sheet: &Sheet,
    base: &Store,
    keys: &mut BTreeSet<CellRef>,
    styles: &StyleChanges,
) -> Option<String> {
    let mut xml = String::new();
    while let Some(&key) = keys.first().filter(|key| key.row < before) {
        xml.push_str(&format!("<row r=\"{}\">", key.row + 1));
        while let Some(cell) = keys.first().copied().filter(|cell| cell.row == key.row) {
            xml.push_str(&new_cell(cell, sheet, base, styles)?);
            keys.pop_first();
        }
        xml.push_str("</row>");
    }
    Some(xml)
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

fn sheet_edits(
    xml: &str,
    sheet: &Sheet,
    base: &Store,
    mut keys: BTreeSet<CellRef>,
    styles: &StyleChanges,
    changed_sst: &mut bool,
) -> Option<Vec<Edit>> {
    let mut reader = Reader::from_str(xml);
    let mut edits = Vec::new();
    let mut in_data = false;
    let mut row = None;
    let mut last_row = None;
    let mut additions: Vec<_> = keys
        .iter()
        .filter(|k| base.get(k.row, k.col).is_none())
        .copied()
        .collect();
    if sheet.merges_dirty {
        additions.extend(
            sheet
                .merges
                .iter()
                .flat_map(|&(r, c, z, d)| [CellRef::new(r, c), CellRef::new(z, d)]),
        );
    }
    let mut passed_dimension = additions.is_empty();
    if additions
        .iter()
        .any(|k| k.row >= 1_048_576 || k.col >= 16_384)
    {
        return None;
    }
    loop {
        let from = reader.buffer_position() as usize;
        let event = reader.read_event().ok()?;
        let to = reader.buffer_position() as usize;
        match event {
            Event::Empty(ref e) | Event::Start(ref e)
                if e.local_name().as_ref() == "dimension" && !additions.is_empty() =>
            {
                passed_dimension = true;
                let bounds = crate::arr::parse_a1_range(&attr(&e, "ref")?)?;
                let expanded = additions.iter().fold(bounds, |(r1, c1, r2, c2), k| {
                    (r1.min(k.row), c1.min(k.col), r2.max(k.row), c2.max(k.col))
                });
                if expanded != bounds {
                    let mut text = expanded_tag(e, "ref")?;
                    text.pop();
                    let end = if matches!(event, Event::Empty(_)) {
                        "/>"
                    } else {
                        ">"
                    };
                    text.push_str(&format!(" ref=\"{}\"{end}", crate::arr::a1_range(expanded)));
                    edits.push(Edit { from, to, text });
                }
            }
            Event::Start(e) if e.name().as_ref() == "sheetData" => {
                in_data = true;
                passed_dimension = true;
            }
            Event::Empty(e) if e.name().as_ref() == "sheetData" => {
                let mut text = expanded_tag(&e, "")?;
                text.push_str(&insert_missing_rows(
                    u32::MAX,
                    sheet,
                    base,
                    &mut keys,
                    styles,
                )?);
                text.push_str("</sheetData>");
                edits.push(Edit { from, to, text });
                break;
            }
            Event::End(e) if e.name().as_ref() == "sheetData" => {
                let text = insert_missing_rows(u32::MAX, sheet, base, &mut keys, styles)?;
                if !text.is_empty() {
                    edits.push(Edit {
                        from,
                        to: from,
                        text,
                    });
                }
                break;
            }
            Event::Start(ref e) | Event::Empty(ref e) if in_data && e.name().as_ref() == "row" => {
                let r = attr(e, "r")?.parse::<u32>().ok()?.checked_sub(1)?;
                if last_row.is_some_and(|prev| prev >= r) {
                    return None;
                }
                last_row = Some(r);
                let text = insert_missing_rows(r, sheet, base, &mut keys, styles)?;
                if !text.is_empty() {
                    edits.push(Edit {
                        from,
                        to: from,
                        text,
                    });
                }
                if keys.first().is_none_or(|key| key.row != r) {
                    if matches!(event, Event::Start(_)) {
                        reader.read_to_end(e.name()).ok()?;
                    }
                    continue;
                }
                if matches!(event, Event::Empty(_)) {
                    let mut text = expanded_tag(e, "spans")?;
                    while let Some(k) = keys.first().copied().filter(|k| k.row == r) {
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
                    let text = if let Some(xf) = styles.get(&key).filter(|_| old == new) {
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
                        replacement(key, e, body, old, new, styles)?
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
            Event::Start(_) | Event::Empty(_) if in_data => return None,
            Event::Eof => break,
            _ => {}
        }
        if keys.is_empty() && passed_dimension {
            break;
        }
    }
    keys.is_empty().then_some(edits)
}

fn write_edits(out: &mut ZipWriter<File>, part: &str, xml: &str, edits: &[Edit]) -> Result<()> {
    let opts = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .compression_level(Some(1));
    out.start_file(part, opts)?;
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

fn invalidate_chain(
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
    replacements.insert(
        BOOK.into(),
        replace_spans(
            &xml,
            [Edit {
                from,
                to,
                text: String::from_utf8(w.into_inner())?,
            }],
        ),
    );
    Ok(Some((replacements, deleted)))
}

pub(super) fn write_if_supported(base: &Path, target: &Path, wb: &Workbook) -> Result<bool> {
    let mut zip = ZipArchive::new(File::open(base)?)?;
    check_note_sources(&mut zip, wb)?;
    let mut sheets = HashMap::new();
    let mut parts = PackageParts::new();
    let mut formula_changed = false;
    let sst = signature(&mut zip, "xl/sharedStrings.xml");
    let style_part = signature(&mut zip, "xl/styles.xml");
    for sheet in &wb.sheets {
        if sheet.row_styles_dirty || sheet.col_styles_dirty {
            return Ok(false);
        }
        if !sheet.cell_data_needs_save()
            && !sheet.styles_dirty
            && sheet.style_edits.is_empty()
            && !sheet.merges_dirty
            && !sheet.notes_dirty
        {
            continue;
        }
        let (Some(part), Some(source)) = (&sheet.source_part, &sheet.source_cells) else {
            return Ok(false);
        };
        if sheet.rich_dirty
            || signature(&mut zip, part) != Some(source.signature)
            || source.strings != sst
            || ((sheet.styles_dirty || !sheet.style_edits.is_empty())
                && source.style_part != style_part)
        {
            return Ok(false);
        }
        let Some(changes) = coordinates(sheet, source, &wb.styles) else {
            return Ok(false);
        };
        if sheet.notes_dirty {
            let Some(notes) = crate::notes::edited_parts(&mut zip, part, sheet)? else {
                return Ok(false);
            };
            for (name, data) in notes {
                if parts.insert(name, data).is_some() {
                    return Ok(false);
                }
            }
        }
        formula_changed |= changes.formula_changed;
        if !changes.keys.is_empty() || sheet.merges_dirty {
            if sheets
                .insert(part.clone(), (sheet, &source.store, changes))
                .is_some()
            {
                return Ok(false);
            }
        }
    }
    let mut deleted = HashSet::new();
    if formula_changed {
        let Some((chain, remove)) = invalidate_chain(&mut zip, wb.calc_manual)? else {
            return Ok(false);
        };
        deleted = remove;
        for (name, data) in chain {
            if parts.insert(name, data).is_some() {
                return Ok(false);
            }
        }
    }
    if parts
        .keys()
        .any(|name| sheets.contains_key(name) || deleted.contains(name))
    {
        return Ok(false);
    }
    let tmp = tempfile::NamedTempFile::new_in(
        target
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new(".")),
    )?;
    let (file, tmp) = tmp.into_parts();
    let mut out = ZipWriter::new(file);
    let mut changed_sst = false;
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
        } else if let Some((sheet, store, changes)) = sheets.remove(&part) {
            let xml = super::read_zip_text(&mut zip, &part)?;
            let Some(mut edits) = sheet_edits(
                &xml,
                sheet,
                store,
                changes.keys,
                &changes.styles,
                &mut changed_sst,
            ) else {
                return Ok(false);
            };
            if sheet.merges_dirty {
                let Some(merge) = merge_edit(&xml, sheet) else {
                    return Ok(false);
                };
                edits.push(merge);
                edits.sort_by_key(|e| e.from);
            }
            write_edits(&mut out, &part, &xml, &edits)?;
        } else {
            out.raw_copy_file(zip.by_index_raw(index)?)?;
        }
    }
    if !sheets.is_empty() || !parts.is_empty() {
        return Ok(false);
    }
    if let Some(i) = zip.index_for_name("xl/sharedStrings.xml") {
        if changed_sst {
            // Some indexed references became inline strings. Both counters are
            // optional; preserve indices, rich runs and every original <si>.
            let xml = super::read_zip_text(&mut zip, "xl/sharedStrings.xml")?;
            let mut reader = Reader::from_str(&xml);
            let edit = loop {
                let from = reader.buffer_position() as usize;
                match reader.read_event()? {
                    Event::Start(e) if e.local_name().as_ref() == "sst" => {
                        let mut root = BytesStart::new(e.name().as_ref().to_owned());
                        for a in e.attributes() {
                            let a = a?;
                            if !matches!(a.key.as_ref(), "count" | "uniqueCount") {
                                root.push_attribute(a);
                            }
                        }
                        let mut w = quick_xml::Writer::new(Vec::new());
                        w.write_event(Event::Start(root))?;
                        break Edit {
                            from,
                            to: reader.buffer_position() as usize,
                            text: String::from_utf8(w.into_inner())?,
                        };
                    }
                    Event::Eof => return Ok(false),
                    _ => {}
                }
            };
            write_edits(&mut out, "xl/sharedStrings.xml", &xml, &[edit])?;
        } else {
            out.raw_copy_file(zip.by_index_raw(i)?)?;
        }
    }
    out.finish()?;
    drop(zip);
    tmp.persist(target)?;
    Ok(true)
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
    fn changing_a_shared_formula_master_uses_the_full_serializer() {
        let rows = r#"<row r="1"><c r="A1"><f t="shared" si="0" ref="A1:A2">B1+1</f><v>2</v></c></row><row r="2"><c r="A2"><f t="shared" si="0"/><v>3</v></c></row>"#;
        let (dir, path, part) = fixture(rows, "");
        let (mut wb, diag) = load_diag(&path).unwrap();
        assert!(diag.is_complete());
        wb.sheets[0].get_mut(0, 0).unwrap().value = Value::Number(11.0);
        let target = dir.path().join("cache.xlsx");
        assert!(write_if_supported(&path, &target, &wb).unwrap());
        let xml = String::from_utf8(read_part(&target, &part).unwrap()).unwrap();
        assert!(xml.contains(r#"<f t="shared" si="0" ref="A1:A2">B1+1</f><v>11</v>"#));
        wb.sheets[0].set(0, 0, num(4.0));
        assert!(!write_if_supported(&path, &target, &wb).unwrap());
        save(&target, &wb).unwrap();
        assert_eq!(
            load_diag(&target).unwrap().0.sheets[0].value(0, 0),
            Value::Number(4.0)
        );
    }
}
