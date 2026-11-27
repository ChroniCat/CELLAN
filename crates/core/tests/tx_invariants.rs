//! Phase 6 P1: 5-Invariant Deterministic Stateful Sequence Suite for CELLAN
//! workbook transactions (`WorkbookTx`), `UndoStack`, `CoordTransform`,
//! `CoordParticipant`, and `TxOverlay` (`CellShiftOverlay`).
//!
//! Zero external crates: uses a deterministic xorshift64 PRNG across multiple
//! seeds and prints the exact seed, step index, field-level `BusinessState`
//! diff, and complete replayable operation history on any failure.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;

use sheetcore::cells::Shift as CellShiftDir;
use sheetcore::cf::{CfBlock, CfKind, CfOp, CfRule, CfSheet, Cfvo, CfvoKind, Dxf};
use sheetcore::charts::{Chart, ChartKind, Series};
use sheetcore::dv::{DataValidation, DvKind, DvOp, DvStyle};
use sheetcore::external::{Kind as SourceKind, Source};
use sheetcore::hyperlinks::Hyperlink;
use sheetcore::model::{Cell, CellRef, Sheet, Value, Workbook};
use sheetcore::notes::{Note, ThreadEntry, ThreadedComment};
use sheetcore::page::PageSetup;
use sheetcore::participant::{CellShiftMode, CoordParticipant};
use sheetcore::pic::Picture;
use sheetcore::pivot::{Agg, GroupBy, PivotResult, PivotSpec};
use sheetcore::pivot_engine::{PivotSlicerSpec, PivotTableModel};
use sheetcore::pivot_multi::{AxisField, PivotGrid, PivotLayout, PivotSpec2, ShowAs, ValueField};
use sheetcore::protection::ProtectedRange;
use sheetcore::refs::Axis;
use sheetcore::richtext::RichRun;
use sheetcore::scenarios::{Scenario, ScenarioSettings};
use sheetcore::sort_filter::{CmpOp, ColorFilter, Criterion, Filter};
use sheetcore::sparkline::{EmptyAs, SparkKind, Sparkline};
use sheetcore::structural::{CellShift, CellShiftConflict, Structural};
use sheetcore::style::CellStyle;
use sheetcore::style_ext::FillPattern;
use sheetcore::style_theme::{ThemeRef, XfTheme};
use sheetcore::sync::{MAX_COLS, MAX_ROWS};
use sheetcore::table::Table;
use sheetcore::textbox::{ShapeKind, TextBox};
use sheetcore::transform::CoordTransform;
use sheetcore::tx::{
    CellWrite, DefinedNameIntent, SheetTabIntent, TxError, TxIntent, TxOverlay, WorkbookTx,
};
use sheetcore::undo::UndoStack;

// ============================================================================
// Deterministic xorshift64 PRNG & Failure Diagnostics
// ============================================================================

#[derive(Clone, Copy, Debug)]
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(if seed == 0 { 0x9e37_79b9_7f4a_7c15 } else { seed })
    }

    #[inline]
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    #[inline]
    fn range_u32(&mut self, lo: u32, hi_excl: u32) -> u32 {
        debug_assert!(lo < hi_excl);
        let span = u64::from(hi_excl - lo);
        lo + (self.next_u64() % span) as u32
    }

    #[inline]
    fn range_usize(&mut self, lo: usize, hi_excl: usize) -> usize {
        debug_assert!(lo < hi_excl);
        let span = (hi_excl - lo) as u64;
        lo + (self.next_u64() % span) as usize
    }

    #[inline]
    fn bool(&mut self) -> bool {
        (self.next_u64() & 1) != 0
    }

    #[inline]
    fn chance(&mut self, num: u64, den: u64) -> bool {
        (self.next_u64() % den) < num
    }
}

struct Trace {
    invariant: &'static str,
    seed: u64,
    ops: Vec<String>,
}

impl Trace {
    fn new(invariant: &'static str, seed: u64) -> Self {
        Self {
            invariant,
            seed,
            ops: Vec::new(),
        }
    }

    fn record(&mut self, step: usize, desc: impl Into<String>) {
        self.ops.push(format!("  step {step:02}: {}", desc.into()));
    }

    fn dump(&self, step: usize, detail: &str) -> String {
        let history = if self.ops.is_empty() {
            "  <none>".to_string()
        } else {
            self.ops.join("\n")
        };
        format!(
            "[{}] seed={:#x} ({}) step={}\n{}\nOp history:\n{}",
            self.invariant, self.seed, self.seed, step, detail, history
        )
    }
}

fn num(v: f64) -> Cell {
    Cell {
        value: Value::Number(v),
        formula: None,
    }
}

fn txt(s: &str) -> Cell {
    Cell {
        value: Value::Text(s.into()),
        formula: None,
    }
}

fn bool_cell(b: bool) -> Cell {
    Cell {
        value: Value::Bool(b),
        formula: None,
    }
}

fn fml(s: &str) -> Cell {
    Cell {
        value: Value::Empty,
        formula: Some(s.into()),
    }
}

fn allow_all(_: usize, _: u32, _: u32, _: &Cell) -> bool {
    true
}

fn make_rich_runs(prefix: &str, bold_part: &str, color: [u8; 3]) -> (String, Vec<RichRun>) {
    let mut r2 = RichRun::plain(bold_part);
    r2.bold = true;
    r2.color = Some(color);
    let full = format!("{prefix}{bold_part}");
    (full, vec![RichRun::plain(prefix), r2])
}

// ============================================================================
// Complete Workbook BusinessState Snapshot & Diff Comparator
// ============================================================================

#[derive(Clone, Debug, PartialEq, Eq)]
enum CanonicalValue {
    Empty,
    Number(u64),
    Text(String),
    Bool(bool),
    Error(String),
}

impl CanonicalValue {
    fn from_value(v: &Value) -> Self {
        match v {
            Value::Empty => Self::Empty,
            Value::Number(n) => Self::Number(n.to_bits()),
            Value::Text(s) => Self::Text(s.to_string()),
            Value::Bool(b) => Self::Bool(*b),
            Value::Error(e) => Self::Error(e.text().to_string()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum CellInput {
    Value(CanonicalValue),
    Formula(String),
}

#[derive(Clone, Debug, PartialEq)]
struct ResolvedStyle {
    style: CellStyle,
    theme: XfTheme,
    pattern: Option<FillPattern>,
    quote: bool,
}

impl ResolvedStyle {
    fn read(wb: &Workbook, id: u16) -> Self {
        Self {
            style: wb.styles.get(id).clone(),
            theme: wb.styles.theme_of(id),
            pattern: wb.styles.pattern_of(id).cloned(),
            quote: wb.styles.quote_of(id),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct ChartSnapshot {
    part: String,
    kind: ChartKind,
    title: String,
    anchor: (u32, u32, u32, u32),
    source_range: String,
    series_refs: Vec<(String, String, String, String)>,
}

impl ChartSnapshot {
    fn read(ch: &Chart) -> Self {
        Self {
            part: ch.part.clone(),
            kind: ch.kind,
            title: ch.title.clone(),
            anchor: ch.anchor,
            source_range: ch.source_range.clone(),
            series_refs: ch
                .series
                .iter()
                .map(|s| {
                    (
                        s.name_ref.clone(),
                        s.cat_ref.clone(),
                        s.val_ref.clone(),
                        s.size_ref.clone(),
                    )
                })
                .collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct AxisSnapshot {
    default_bits: u32,
    spans: Vec<(u32, u32, u32)>,
}

impl AxisSnapshot {
    fn read(ax: &sheetcore::layout::Axis) -> Self {
        Self {
            default_bits: ax.default_size().to_bits(),
            spans: ax
                .spans()
                .iter()
                .map(|s| (s.start, s.end, s.size.to_bits()))
                .collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PictureGeom {
    part: String,
    name: String,
    rect: (u32, u32, u32, u32),
    off: [i64; 4],
    ext: String,
    rot_deg_bits: u32,
    crop_bits: [u32; 4],
}

impl PictureGeom {
    fn read(p: &Picture) -> Self {
        Self {
            part: p.part.clone(),
            name: p.name.clone(),
            rect: p.rect,
            off: p.off,
            ext: p.ext.clone(),
            rot_deg_bits: p.rot_deg.to_bits(),
            crop_bits: p.crop.map(f32::to_bits),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct TextBoxGeom {
    id: u32,
    name: String,
    kind: ShapeKind,
    from: (u32, i64, u32, i64),
    to: (u32, i64, u32, i64),
    text: String,
}

impl TextBoxGeom {
    fn read(tb: &TextBox) -> Self {
        Self {
            id: tb.id,
            name: tb.name.clone(),
            kind: tb.kind,
            from: tb.from,
            to: tb.to,
            text: tb.text.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct PivotModelSnapshot {
    name: String,
    source_sheet: String,
    target_sheet: usize,
    anchor: (u32, u32),
    rect: (u32, u32, u32, u32),
    last_meta_anchor: (u32, u32),
    last_meta_rect: (u32, u32, u32, u32),
    spec: PivotSpec2,
    slicer_cols: Vec<u32>,
    imported_part: Option<String>,
}

impl PivotModelSnapshot {
    fn read(p: &PivotTableModel) -> Self {
        Self {
            name: p.name.clone(),
            source_sheet: p.source_sheet.clone(),
            target_sheet: p.target_sheet,
            anchor: p.anchor,
            rect: p.rect,
            last_meta_anchor: p.last_meta.anchor,
            last_meta_rect: p.last_meta.rect,
            spec: p.spec.clone(),
            slicer_cols: p.options.slicers.iter().map(|s| s.col).collect(),
            imported_part: p.imported_part.clone(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct SheetBusinessState {
    name: String,
    cells: BTreeMap<(u32, u32), CellInput>,
    row_styles: BTreeMap<u32, ResolvedStyle>,
    col_styles: BTreeMap<u32, ResolvedStyle>,
    explicit_styles: BTreeMap<(u32, u32), ResolvedStyle>,
    rich: BTreeMap<(u32, u32), Vec<RichRun>>,
    notes: BTreeMap<(u32, u32), Note>,
    threads: BTreeMap<(u32, u32), ThreadedComment>,
    hyperlinks: BTreeMap<(u32, u32), Hyperlink>,
    merges: Vec<(u32, u32, u32, u32)>,
    cf: CfSheet,
    dv: Vec<DataValidation>,
    charts: Vec<ChartSnapshot>,
    sparklines: Vec<Sparkline>,
    tables: Vec<Table>,
    filter: Option<Filter>,
    arrays: Vec<(CellRef, (u32, u32, u32, u32))>,
    spilled: BTreeSet<CellRef>,
    spill_shapes: BTreeMap<CellRef, (u32, u32)>,
    scenarios: Vec<Scenario>,
    scenario_settings: ScenarioSettings,
    protected_ranges: Vec<ProtectedRange>,
    ignored_errors: BTreeSet<(u32, u32)>,
    rows: AxisSnapshot,
    cols: AxisSnapshot,
    hidden_rows: Vec<(u32, u32)>,
    hidden_rows_user: Vec<(u32, u32)>,
    hidden_cols: Vec<(u32, u32)>,
    row_levels: BTreeMap<u32, u8>,
    col_levels: BTreeMap<u32, u8>,
    freeze: Option<(u32, u32)>,
    page: PageSetup,
    pictures: Vec<PictureGeom>,
    textboxes: Vec<TextBoxGeom>,
}

#[derive(Clone, Debug, PartialEq)]
struct BusinessState {
    sheets: Vec<SheetBusinessState>,
    shared_names: Vec<(String, Option<usize>, String)>,
    private_names: Vec<(String, Option<usize>, String)>,
    sources: Vec<Source>,
    pivots: Vec<PivotModelSnapshot>,
    pivots_pending: Vec<(usize, PivotSpec, (u32, u32, u32, u32), String)>,
    pivots_pending2: Vec<(usize, PivotSpec2, (u32, u32, u32, u32))>,
    pivot_shifts: Vec<(String, u8, u32, u32, bool)>,
}

impl BusinessState {
    fn capture(wb: &Workbook) -> Self {
        let default_style = ResolvedStyle::read(wb, 0);
        let sheets = wb
            .sheets
            .iter()
            .map(|sh| {
                let mut cells = BTreeMap::new();
                for (k, view) in sh.iter_views() {
                    if let Some(f) = view.formula() {
                        cells.insert((k.row, k.col), CellInput::Formula(f.to_string()));
                    } else {
                        let v = view.value();
                        if !v.is_empty() {
                            cells.insert(
                                (k.row, k.col),
                                CellInput::Value(CanonicalValue::from_value(v.as_ref())),
                            );
                        }
                    }
                }

                let row_styles: BTreeMap<u32, ResolvedStyle> = sh
                    .row_styles
                    .iter()
                    .map(|(&r, &id)| (r, ResolvedStyle::read(wb, id)))
                    .filter(|(_, st)| st != &default_style)
                    .collect();
                let col_styles: BTreeMap<u32, ResolvedStyle> = sh
                    .col_styles
                    .iter()
                    .map(|(&c, &id)| (c, ResolvedStyle::read(wb, id)))
                    .filter(|(_, st)| st != &default_style)
                    .collect();
                let mut explicit_styles = BTreeMap::new();
                for (k, &id) in sh.styles.iter() {
                    let rs = ResolvedStyle::read(wb, id);
                    let inherited = row_styles
                        .get(&k.row)
                        .or_else(|| col_styles.get(&k.col))
                        .unwrap_or(&default_style);
                    if &rs != inherited {
                        explicit_styles.insert((k.row, k.col), rs);
                    }
                }

                let rich: BTreeMap<(u32, u32), Vec<RichRun>> =
                    sh.rich.iter().map(|(&k, v)| (k, v.clone())).collect();
                let notes: BTreeMap<(u32, u32), Note> = sh
                    .notes
                    .iter()
                    .map(|(k, v)| ((k.row, k.col), v.clone()))
                    .collect();
                let threads: BTreeMap<(u32, u32), ThreadedComment> = sh
                    .threads
                    .iter()
                    .map(|(k, v)| ((k.row, k.col), v.clone()))
                    .collect();
                let hyperlinks: BTreeMap<(u32, u32), Hyperlink> =
                    sh.hyperlinks.iter().map(|(&k, v)| (k, v.clone())).collect();

                let mut merges: Vec<(u32, u32, u32, u32)> = sh
                    .merges
                    .iter()
                    .map(|&(r1, c1, r2, c2)| (r1.min(r2), c1.min(c2), r1.max(r2), c1.max(c2)))
                    .collect();
                merges.sort_unstable();

                let charts: Vec<ChartSnapshot> = sh.charts.iter().map(ChartSnapshot::read).collect();

                let mut sparklines = sh.sparklines.clone();
                sparklines.sort_by(|a, b| {
                    (a.r, a.c, a.rect, &a.source_sheet).cmp(&(b.r, b.c, b.rect, &b.source_sheet))
                });

                let mut arrays = sh.arrays.clone();
                arrays.sort_unstable();

                let spilled: BTreeSet<CellRef> = sh.spilled.iter().copied().collect();
                let spill_shapes: BTreeMap<CellRef, (u32, u32)> =
                    sh.spill_shapes.iter().map(|(&k, &v)| (k, v)).collect();
                let ignored_errors: BTreeSet<(u32, u32)> =
                    sh.ignored_errors.iter().copied().collect();

                let row_levels: BTreeMap<u32, u8> =
                    sh.row_levels.iter().map(|(&k, &v)| (k, v)).collect();
                let col_levels: BTreeMap<u32, u8> =
                    sh.col_levels.iter().map(|(&k, &v)| (k, v)).collect();

                let mut page = sh.page.clone();
                page.row_breaks.sort_unstable();
                page.col_breaks.sort_unstable();

                SheetBusinessState {
                    name: sh.name.clone(),
                    cells,
                    row_styles,
                    col_styles,
                    explicit_styles,
                    rich,
                    notes,
                    threads,
                    hyperlinks,
                    merges,
                    cf: sh.cf.clone(),
                    dv: sh.dv.clone(),
                    charts,
                    sparklines,
                    tables: sh.tables.clone(),
                    filter: sh.filter.clone(),
                    arrays,
                    spilled,
                    spill_shapes,
                    scenarios: sh.scenarios.clone(),
                    scenario_settings: sh.scenario_settings.clone(),
                    protected_ranges: sh.protected_ranges.clone(),
                    ignored_errors,
                    rows: AxisSnapshot::read(&sh.rows),
                    cols: AxisSnapshot::read(&sh.cols),
                    hidden_rows: sh.hidden_rows.intervals().to_vec(),
                    hidden_rows_user: sh.hidden_rows_user.intervals().to_vec(),
                    hidden_cols: sh.hidden_cols.intervals().to_vec(),
                    row_levels,
                    col_levels,
                    freeze: sh.freeze,
                    page,
                    pictures: sh.pictures.iter().map(PictureGeom::read).collect(),
                    textboxes: sh.textboxes.iter().map(TextBoxGeom::read).collect(),
                }
            })
            .collect();

        let shared_names = wb
            .names
            .iter()
            .filter(|(n, _, _)| !n.starts_with("_sc_src_"))
            .cloned()
            .collect();
        let private_names = wb
            .names
            .iter()
            .filter(|(n, _, _)| n.starts_with("_sc_src_"))
            .cloned()
            .collect();

        Self {
            sheets,
            shared_names,
            private_names,
            sources: wb.sources.clone(),
            pivots: wb.pivots.iter().map(PivotModelSnapshot::read).collect(),
            pivots_pending: wb
                .pivots_pending
                .iter()
                .map(|(si, spec, _, out, val)| (*si, spec.clone(), *out, val.clone()))
                .collect(),
            pivots_pending2: wb
                .pivots_pending2
                .iter()
                .map(|(si, spec, _, out)| (*si, spec.clone(), *out))
                .collect(),
            pivot_shifts: wb.pivot_shifts.clone(),
        }
    }

    fn diff(&self, other: &Self) -> Vec<String> {
        let mut diffs = Vec::new();
        if self.sheets.len() != other.sheets.len() {
            diffs.push(format!(
                "wb.sheets.len: expected {}, got {}",
                self.sheets.len(),
                other.sheets.len()
            ));
        }
        for (i, (a, b)) in self.sheets.iter().zip(&other.sheets).enumerate() {
            let pfx = format!("sheet[{i}:{}]", a.name);
            macro_rules! cmp_field {
                ($field:ident) => {
                    if a.$field != b.$field {
                        diffs.push(format!(
                            "{pfx}.{}: expected {:?}\n    got {:?}",
                            stringify!($field),
                            a.$field,
                            b.$field
                        ));
                    }
                };
            }
            cmp_field!(name);
            cmp_field!(cells);
            cmp_field!(row_styles);
            cmp_field!(col_styles);
            cmp_field!(explicit_styles);
            cmp_field!(rich);
            cmp_field!(notes);
            cmp_field!(threads);
            cmp_field!(hyperlinks);
            cmp_field!(merges);
            cmp_field!(cf);
            cmp_field!(dv);
            cmp_field!(charts);
            cmp_field!(sparklines);
            cmp_field!(tables);
            cmp_field!(filter);
            cmp_field!(arrays);
            cmp_field!(spilled);
            cmp_field!(spill_shapes);
            cmp_field!(scenarios);
            cmp_field!(scenario_settings);
            cmp_field!(protected_ranges);
            cmp_field!(ignored_errors);
            cmp_field!(rows);
            cmp_field!(cols);
            cmp_field!(hidden_rows);
            cmp_field!(hidden_rows_user);
            cmp_field!(hidden_cols);
            cmp_field!(row_levels);
            cmp_field!(col_levels);
            cmp_field!(freeze);
            cmp_field!(page);
            cmp_field!(pictures);
            cmp_field!(textboxes);
        }
        macro_rules! cmp_wb {
            ($field:ident) => {
                if self.$field != other.$field {
                    diffs.push(format!(
                        "wb.{}: expected {:?}\n    got {:?}",
                        stringify!($field),
                        self.$field,
                        other.$field
                    ));
                }
            };
        }
        cmp_wb!(shared_names);
        cmp_wb!(private_names);
        cmp_wb!(sources);
        cmp_wb!(pivots);
        cmp_wb!(pivots_pending);
        cmp_wb!(pivots_pending2);
        cmp_wb!(pivot_shifts);
        diffs
    }
}

fn assert_business_state_eq(
    expected: &BusinessState,
    actual: &BusinessState,
    trace: &Trace,
    step: usize,
    label: &str,
) {
    if expected != actual {
        let diffs = expected.diff(actual);
        let preview = if diffs.len() > 8 {
            format!(
                "{}\n  ... (+{} more field diffs)",
                diffs[..8].join("\n"),
                diffs.len() - 8
            )
        } else {
            diffs.join("\n")
        };
        panic!(
            "{}",
            trace.dump(
                step,
                &format!("BusinessState mismatch ({label}):\n{preview}")
            )
        );
    }
}

fn capture_raw_styles(wb: &Workbook) -> Vec<(usize, Vec<(CellRef, u16)>, Vec<(u32, u16)>, Vec<(u32, u16)>)> {
    wb.sheets
        .iter()
        .enumerate()
        .map(|(i, sh)| {
            let mut cells: Vec<_> = sh.styles.iter().map(|(k, &v)| (k, v)).collect();
            cells.sort_unstable();
            let mut rows: Vec<_> = sh.row_styles.iter().map(|(&k, &v)| (k, v)).collect();
            rows.sort_unstable();
            let mut cols: Vec<_> = sh.col_styles.iter().map(|(&k, &v)| (k, v)).collect();
            cols.sort_unstable();
            (i, cells, rows, cols)
        })
        .collect()
}

// ============================================================================
// Invariant 4 Helper: Physical Coordinate Bounds Checker
// ============================================================================

fn assert_physical_bounds(wb: &Workbook, trace: &Trace, step: usize, label: &str) {
    let check_point = |r: u32, c: u32, what: &str| {
        if r >= MAX_ROWS || c >= MAX_COLS {
            panic!(
                "{}",
                trace.dump(
                    step,
                    &format!(
                        "Physical bound violation ({label}): {what} has out-of-grid coord ({r}, {c}) >= ({MAX_ROWS}, {MAX_COLS})"
                    )
                )
            );
        }
    };
    let check_rect = |(r1, c1, r2, c2): (u32, u32, u32, u32), what: &str| {
        let (ra, rb) = (r1.min(r2), r1.max(r2));
        let (ca, cb) = (c1.min(c2), c1.max(c2));
        if rb >= MAX_ROWS || cb >= MAX_COLS {
            panic!(
                "{}",
                trace.dump(
                    step,
                    &format!(
                        "Physical bound violation ({label}): {what} has out-of-grid rect ({ra},{ca},{rb},{cb}) >= ({MAX_ROWS}, {MAX_COLS})"
                    )
                )
            );
        }
    };

    for (si, sh) in wb.sheets.iter().enumerate() {
        let p = format!("sheet[{si}:{}]", sh.name);
        for (k, _) in sh.iter_views() {
            check_point(k.row, k.col, &format!("{p}.cells"));
        }
        for (k, _) in sh.styles.iter() {
            check_point(k.row, k.col, &format!("{p}.styles"));
        }
        for &r in sh.row_styles.keys() {
            check_point(r, 0, &format!("{p}.row_styles"));
        }
        for &c in sh.col_styles.keys() {
            check_point(0, c, &format!("{p}.col_styles"));
        }
        for &(r, c) in sh.rich.keys() {
            check_point(r, c, &format!("{p}.rich"));
        }
        for (k, n) in sh.notes.iter() {
            check_point(k.row, k.col, &format!("{p}.notes.key"));
            check_point(n.r, n.c, &format!("{p}.notes.val"));
        }
        for (k, t) in sh.threads.iter() {
            check_point(k.row, k.col, &format!("{p}.threads.key"));
            check_point(t.r, t.c, &format!("{p}.threads.val"));
        }
        for (&(r, c), link) in sh.hyperlinks.iter() {
            check_point(r, c, &format!("{p}.hyperlinks.anchor"));
            let r2 = r.saturating_add(link.span.0);
            let c2 = c.saturating_add(link.span.1);
            check_point(r2, c2, &format!("{p}.hyperlinks.span_end"));
        }
        for &m in sh.merges.iter() {
            check_rect(m, &format!("{p}.merges"));
        }
        for blk in &sh.cf.blocks {
            for &rg in &blk.ranges {
                check_rect(rg, &format!("{p}.cf"));
            }
        }
        for dv in &sh.dv {
            for &rg in &dv.ranges {
                check_rect(rg, &format!("{p}.dv"));
            }
        }
        for ch in sh.charts.iter() {
            check_rect(ch.anchor, &format!("{p}.charts.anchor"));
        }
        for sp in &sh.sparklines {
            check_point(sp.r, sp.c, &format!("{p}.sparklines.anchor"));
            check_rect(sp.rect, &format!("{p}.sparklines.rect"));
        }
        for t in &sh.tables {
            check_rect(t.range, &format!("{p}.tables"));
        }
        if let Some(f) = &sh.filter {
            check_rect(f.range, &format!("{p}.filter.range"));
            for &c in f
                .allows
                .keys()
                .chain(f.criteria.keys())
                .chain(f.date_groups.keys())
                .chain(f.color_filters.keys())
                .chain(f.top_filters.keys())
                .chain(f.dynamic_filters.keys())
            {
                check_point(0, c, &format!("{p}.filter.col_key"));
            }
        }
        for &(anchor, rect) in &sh.arrays {
            check_point(anchor.row, anchor.col, &format!("{p}.arrays.anchor"));
            check_rect(rect, &format!("{p}.arrays.rect"));
        }
        for k in sh.spilled.iter() {
            check_point(k.row, k.col, &format!("{p}.spilled"));
        }
        for (&anchor, _) in sh.spill_shapes.iter() {
            check_point(anchor.row, anchor.col, &format!("{p}.spill_shapes"));
        }
        for sc in &sh.scenarios {
            for &((r, c), _) in &sc.cells {
                check_point(r, c, &format!("{p}.scenarios"));
            }
        }
        for pr in &sh.protected_ranges {
            for &rg in &pr.sqref {
                check_rect(rg, &format!("{p}.protected_ranges"));
            }
        }
        for &(r, c) in sh.ignored_errors.iter() {
            check_point(r, c, &format!("{p}.ignored_errors"));
        }
        for sp in sh.rows.spans() {
            check_point(sp.end, 0, &format!("{p}.rows.span"));
        }
        for sp in sh.cols.spans() {
            check_point(0, sp.end, &format!("{p}.cols.span"));
        }
        for &(_, hi) in sh.hidden_rows.intervals() {
            check_point(hi, 0, &format!("{p}.hidden_rows"));
        }
        for &(_, hi) in sh.hidden_rows_user.intervals() {
            check_point(hi, 0, &format!("{p}.hidden_rows_user"));
        }
        for &(_, hi) in sh.hidden_cols.intervals() {
            check_point(0, hi, &format!("{p}.hidden_cols"));
        }
        for &r in sh.row_levels.keys() {
            check_point(r, 0, &format!("{p}.row_levels"));
        }
        for &c in sh.col_levels.keys() {
            check_point(0, c, &format!("{p}.col_levels"));
        }
        if let Some((fr, fc)) = sh.freeze {
            if fr > MAX_ROWS || fc > MAX_COLS {
                panic!(
                    "{}",
                    trace.dump(
                        step,
                        &format!("Physical bound violation ({label}): {p}.freeze=({fr},{fc})")
                    )
                );
            }
        }
        if let Some(pa) = sh.page.print_area {
            check_rect(pa, &format!("{p}.page.print_area"));
        }
        if let Some((r1, r2)) = sh.page.title_rows {
            check_point(r1.max(r2), 0, &format!("{p}.page.title_rows"));
        }
        if let Some((c1, c2)) = sh.page.title_cols {
            check_point(0, c1.max(c2), &format!("{p}.page.title_cols"));
        }
        for &rb in &sh.page.row_breaks {
            check_point(rb, 0, &format!("{p}.page.row_breaks"));
        }
        for &cb in &sh.page.col_breaks {
            check_point(0, cb, &format!("{p}.page.col_breaks"));
        }
        for pic in &sh.pictures {
            check_rect(pic.rect, &format!("{p}.pictures"));
        }
        for tb in &sh.textboxes {
            check_point(tb.from.2, tb.from.0, &format!("{p}.textboxes.from"));
            check_point(tb.to.2, tb.to.0, &format!("{p}.textboxes.to"));
        }
    }

    for src in &wb.sources {
        check_point(src.at.0, src.at.1, "wb.sources.at");
        if let SourceKind::Internal { rect, .. } = &src.kind {
            check_rect(*rect, "wb.sources.internal");
        }
    }
    for piv in &wb.pivots {
        check_point(piv.anchor.0, piv.anchor.1, "wb.pivots.anchor");
        check_rect(piv.rect, "wb.pivots.rect");
        check_point(piv.last_meta.anchor.0, piv.last_meta.anchor.1, "wb.pivots.last_meta.anchor");
        check_rect(piv.last_meta.rect, "wb.pivots.last_meta.rect");
        check_rect(piv.spec.source, "wb.pivots.spec.source");
    }
    for (_, spec, _, out, _) in &wb.pivots_pending {
        check_rect(spec.source, "wb.pivots_pending.source");
        check_rect(*out, "wb.pivots_pending.out");
    }
    for (_, spec, _, out) in &wb.pivots_pending2 {
        check_rect(spec.source, "wb.pivots_pending2.source");
        check_rect(*out, "wb.pivots_pending2.out");
    }
}

// ============================================================================
// Rich Baseline Workbook Builder & Late-Join Helper
// ============================================================================

struct RichFixtures {
    wb: Workbook,
    style_ids: Vec<u16>,
}

/// Build a genuinely rich multi-sheet baseline workbook exercising ALL 16
/// `CoordParticipant` subsystems cleanly separated so partial `CellShift`,
/// whole-axis `Structural`, `AxisPermute`, `DefinedNames`, `CellBatch`, and
/// `SheetTab` operations all interact with real business state.
fn build_rich_workbook(seed: u64) -> RichFixtures {
    let mut rng = Rng::new(seed ^ 0xA5A5_5A5A_1234_5678);
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Data".into();
    let data_id = wb.sheets[0].identity;

    let mut report = Sheet::new("Report");
    let mut aux = Sheet::new("Aux");

    // Intern a rich style palette (including theme, fill pattern, and quotePrefix).
    let st_bold = wb.styles.intern(CellStyle {
        bold: true,
        fill: Some([0xEE, 0xF4, 0xFF]),
        ..CellStyle::default_dark()
    });
    let st_num = wb.styles.intern_with_ext(
        CellStyle {
            format: "#,##0.00".into(),
            italic: true,
            ..CellStyle::default_dark()
        },
        XfTheme {
            font: Some(ThemeRef { idx: 4, tint: -0.25 }),
            fill: None,
        },
        None,
        false,
    );
    let st_pat = wb.styles.intern_with_ext(
        CellStyle {
            underline: true,
            ..CellStyle::default_dark()
        },
        XfTheme::default(),
        Some(FillPattern {
            kind: "lightGrid".into(),
            fg: Some([0x20, 0x40, 0x80]),
            bg: Some([0xFF, 0xFF, 0xEE]),
            fg_theme: None,
            bg_theme: None,
            fg_indexed: None,
            bg_indexed: None,
        }),
        true,
    );
    let style_ids = vec![0, st_bold, st_num, st_pat];

    // 1. Cells + Styles (explicit, row, col) + Rich text on Data (rows 0..10, cols 0..6)
    {
        let sh = &mut wb.sheets[0];
        for r in 0..8u32 {
            sh.set(r, 0, txt(&format!("Item_{r}_{}", rng.range_u32(10, 99))));
            sh.set(r, 1, num((r + 1) as f64 * 10.0 + rng.range_u32(0, 9) as f64));
            sh.set(r, 2, num((r + 1) as f64 * 2.5));
        }
        sh.set(0, 3, fml("=SUM(B1:B8)"));
        sh.set(1, 3, fml("=B2+C2"));
        sh.set(2, 3, bool_cell(true));

        // Rich text cells on (0, 4), (2, 4), (4, 0), and (2, 3) (safe col-D CellShift lane)
        for &(rr, cc, pfx, bld) in &[
            (0u32, 4u32, "Revenue ", "total"),
            (2u32, 4u32, "Quarter ", "Q4"),
            (4u32, 0u32, "Dept ", "Alpha"),
            (2u32, 3u32, "Lane ", "D3"),
        ] {
            let (full, runs) = make_rich_runs(pfx, bld, [0xCC, 0x22, 0x11]);
            sh.set(rr, cc, txt(&full));
            sh.rich.insert((rr, cc), runs);
        }

        sh.set_style(0, 0, st_bold);
        sh.set_style(1, 1, st_num);
        sh.set_style(2, 2, st_pat);
        sh.set_style(1, 3, st_bold);
        sh.set_row_style(3, st_bold);
        sh.set_col_style(5, st_num);

        // 2. Notes & Threads (also on safe col-D lane so vertical CellShift moves them!)
        sh.notes.insert(
            CellRef::new(1, 0),
            Note {
                r: 1,
                c: 0,
                author: "Alice".into(),
                text: "Verify Q1 figure".into(),
            },
        );
        sh.notes.insert(
            CellRef::new(1, 3),
            Note {
                r: 1,
                c: 3,
                author: "LaneD".into(),
                text: "Col D memo".into(),
            },
        );
        sh.threads.insert(
            CellRef::new(2, 1),
            ThreadedComment {
                r: 2,
                c: 1,
                resolved: false,
                entries: vec![
                    ThreadEntry {
                        id: "e1".into(),
                        author: "Bob".into(),
                        timestamp: "2026-11-27T01:00:00Z".into(),
                        text: "Looks good".into(),
                    },
                    ThreadEntry {
                        id: "e2".into(),
                        author: "Alice".into(),
                        timestamp: "2026-11-27T01:05:00Z".into(),
                        text: "Confirmed".into(),
                    },
                ],
            },
        );

        // 3. Hyperlinks (single-cell internal + multi-cell external)
        sh.hyperlinks.insert(
            (1, 4),
            Hyperlink {
                location: "Data!B3".into(),
                tooltip: "Jump B3".into(),
                ..Default::default()
            },
        );
        sh.hyperlinks.insert(
            (6, 0),
            Hyperlink {
                target: "https://example.invalid/spec".into(),
                external: true,
                span: (0, 1), // covers (6,0)..=(6,1)
                ..Default::default()
            },
        );

        // 4. Merges (two disjoint merges)
        sh.merge_range(8, 0, 9, 1);
        sh.merge_range(5, 4, 5, 5);

        // 5. Conditional Formatting & Data Validation (including a full-column CF rule!)
        sh.cf.dxfs.push(Dxf {
            bold: true,
            fill: Some([0xFF, 0xEE, 0xCC]),
            ..Dxf::default()
        });
        sh.cf.blocks.push(CfBlock {
            ranges: vec![(0, 1, 7, 1)],
            rules: vec![CfRule {
                kind: CfKind::CellIs {
                    op: CfOp::Greater,
                    a: "25".into(),
                    b: String::new(),
                    dxf: 0,
                },
                stop: false,
                priority: 1,
            }],
        });
        sh.cf.blocks.push(CfBlock {
            ranges: vec![(0, 2, MAX_ROWS - 1, 2)], // full-column rule: must NOT falsely reject inserts!
            rules: vec![CfRule {
                kind: CfKind::Bar {
                    color: [0x33, 0x66, 0xCC],
                    neg_color: None,
                    min: Cfvo {
                        kind: CfvoKind::Formula,
                        val: 0.0,
                        formula: "=Data!$B$1".into(),
                    },
                    max: Cfvo {
                        kind: CfvoKind::Formula,
                        val: 0.0,
                        formula: "=Data!$B$5".into(),
                    },
                    show_value: true,
                },
                stop: false,
                priority: 2,
            }],
        });
        sh.dv.push(DataValidation {
            ranges: vec![(0, 1, 7, 1)],
            kind: DvKind::Whole,
            op: DvOp::Between,
            f1: "0".into(),
            f2: "10000".into(),
            allow_blank: true,
            show_dropdown: true,
            error_style: DvStyle::Stop,
            show_error: true,
            show_prompt: false,
            error_title: String::new(),
            error_msg: String::new(),
            prompt_title: String::new(),
            prompt_msg: String::new(),
        });

        // 6. IgnoredErrors
        sh.ignored_errors.insert((1, 3));
        sh.ignored_errors.insert((3, 1));

        // 7. Chart & Sparkline on Data
        sh.charts.push(Chart {
            kind: ChartKind::Column,
            title: "Sales".into(),
            part: "xl/charts/chart1.xml".into(),
            anchor: (1, 7, 6, 11),
            source_range: "Data!$B$1:$B$5".into(),
            series: vec![Series {
                name_ref: "Data!$A$1".into(),
                cat_ref: "Data!$A$1:$A$5".into(),
                val_ref: "Data!$B$1:$B$5".into(),
                size_ref: String::new(),
                ..Series::default()
            }],
            ..Chart::default()
        });
        sh.sparklines.push(Sparkline {
            r: 0,
            c: 6,
            source_sheet: None,
            rect: (0, 1, 4, 1),
            kind: SparkKind::Line,
            empty: EmptyAs::Zero,
            ..Sparkline::default()
        });

        // 8. Guarded subsystems placed in rows 12..=24 / cols 0..=10
        // Table1 at rows 12..=15, cols 0..=2
        for r in 12..=15u32 {
            sh.set(r, 0, txt(if r == 12 { "ColA" } else { "Row" }));
            sh.set(r, 1, num((r - 11) as f64 * 100.0));
            sh.set(r, 2, num((r - 11) as f64 * 5.0));
        }
        sh.tables.push(Table {
            name: "Table1".into(),
            display: "Table1".into(),
            range: (12, 0, 15, 2),
            columns: vec!["ColA".into(), "Amount".into(), "Tax".into()],
            column_ids: vec![1, 2, 3],
            header_row: true,
            totals_row: false,
            style_name: "TableStyleMedium2".into(),
            totals_fns: vec!["none".into(), "sum".into(), "none".into()],
        });

        // AutoFilter at rows 17..=20, cols 0..=2
        let mut flt = Filter::new(17, 0, 20, 2);
        flt.allows
            .insert(0, ["A".to_string(), "B".to_string()].into_iter().collect());
        flt.criteria.insert(1, Criterion::new(CmpOp::Gt, "10"));
        flt.color_filters
            .insert(2, ColorFilter::Fill(Some([0xEE, 0xF4, 0xFF])));
        sh.filter = Some(flt);

        // Arrays & Spills at rows 12..=14, cols 4..=5
        sh.set(12, 4, fml("=B1:B3*2"));
        sh.arrays.push((CellRef::new(12, 4), (12, 4, 14, 4)));
        sh.set(12, 5, fml("=SEQUENCE(2,1)"));
        sh.set(13, 5, num(2.0));
        sh.spilled.insert(CellRef::new(13, 5));
        sh.spill_shapes.insert(CellRef::new(12, 5), (2, 1));

        // Scenarios & ProtectedRanges at rows 17..=19, cols 4..=6
        let mut sc = Scenario::new("BaseCase");
        sc.comment = "Default assumptions".into();
        sc.cells = vec![((17, 4), "100".into()), ((18, 4), "0.15".into())];
        sh.scenarios.push(sc);
        sh.scenario_settings = ScenarioSettings::default();
        sh.scenario_settings.current = Some(0);
        sh.scenario_settings.show = Some(0);
        sh.scenario_settings.sqref = Some("E18:E19".into());
        sh.protected_ranges
            .push(ProtectedRange::new("EditArea", vec![(17, 5, 19, 6)], ""));

        // Layout & Page (custom row heights, col widths, hidden rows/cols, outline levels, freeze, page)
        sh.rows.set(0, 42.0);
        sh.rows.set(2, 30.0);
        sh.rows.set(12, 28.0);
        sh.cols.set(0, 120.0);
        sh.cols.set(1, 95.0);
        sh.hidden_rows.insert(18);
        sh.hidden_rows_user.insert(7);
        sh.hidden_cols.insert(6);
        sh.row_levels.insert(1, 1);
        sh.row_levels.insert(2, 2);
        sh.col_levels.insert(1, 1);
        sh.freeze = Some((1, 1));
        sh.page.print_area = Some((0, 0, 20, 6));
        sh.page.title_rows = Some((0, 0));
        sh.page.title_cols = Some((0, 0));
        sh.page.row_breaks = vec![10];
        sh.page.col_breaks = vec![4];

        // Pictures (with dummy image bytes) & TextBoxes
        sh.pictures.push(Picture {
            rect: (2, 7, 5, 9),
            off: [100, 200, 300, 400],
            part: "xl/media/image1.png".into(),
            data: Some(Arc::new(vec![0x89, b'P', b'N', b'G', 1, 2, 3, 4])),
            ext: "png".into(),
            name: "Logo".into(),
            rot_deg: 15.0,
            crop: [0.0, 0.0, 0.0, 0.0],
        });
        sh.textboxes.push(TextBox {
            id: 7,
            name: "Callout1".into(),
            kind: ShapeKind::RoundedRect,
            from: (7, 1000, 8, 2000),
            to: (10, 3000, 11, 4000),
            text: "Summary note".into(),
            ..TextBox::default()
        });
    }

    // 9. Cross-sheet references on Report
    report.set(0, 0, fml("=Data!B2+Data!B4"));
    report.set(1, 0, fml("=SUM(Data!B1:B5)"));
    report.set(2, 0, num(42.0));
    report.cf.blocks.push(CfBlock {
        ranges: vec![(0, 0, 2, 0)],
        rules: vec![CfRule {
            kind: CfKind::Bar {
                color: [0x22, 0x88, 0x33],
                neg_color: None,
                min: Cfvo {
                    kind: CfvoKind::Formula,
                    val: 0.0,
                    formula: "=Data!$B$1".into(),
                },
                max: Cfvo {
                    kind: CfvoKind::Formula,
                    val: 0.0,
                    formula: "=Data!$B$4".into(),
                },
                show_value: true,
            },
            stop: false,
            priority: 1,
        }],
    });
    report.dv.push(DataValidation {
        ranges: vec![(0, 1, 2, 1)],
        kind: DvKind::List,
        op: DvOp::Between,
        f1: "=Data!$A$1:$A$4".into(),
        f2: String::new(),
        allow_blank: true,
        show_dropdown: true,
        error_style: DvStyle::Warning,
        show_error: true,
        show_prompt: false,
        error_title: String::new(),
        error_msg: String::new(),
        prompt_title: String::new(),
        prompt_msg: String::new(),
    });
    report.charts.push(Chart {
        kind: ChartKind::Line,
        title: "CrossSheet".into(),
        part: "xl/charts/chart2.xml".into(),
        anchor: (1, 3, 8, 9),
        source_range: "Data!$B$1:$B$5".into(),
        series: vec![Series {
            name_ref: "Data!$A$1".into(),
            cat_ref: "Data!$A$1:$A$5".into(),
            val_ref: "Data!$B$1:$B$5".into(),
            size_ref: String::new(),
            ..Series::default()
        }],
        ..Chart::default()
    });
    report.sparklines.push(Sparkline {
        r: 0,
        c: 2,
        source_sheet: Some("Data".into()),
        rect: (0, 1, 4, 1),
        kind: SparkKind::Column,
        ..Sparkline::default()
    });

    // Aux sheet
    aux.set(0, 0, txt("AuxHeader"));
    aux.set(1, 0, fml("=Data!C1*2"));

    wb.sheets.push(report);
    wb.sheets.push(aux);

    // 10. Workbook-level registries: names (private + global + scoped), sources, pivots
    wb.names
        .push(("_sc_src_local".into(), None, "\"C:\\local\\private.csv\"".into()));
    wb.names
        .push(("TotalRev".into(), None, "Data!$B$1:$B$5".into()));
    wb.names
        .push(("LocalRate".into(), Some(1), "Data!$C$2".into()));

    wb.sources.push(Source {
        name: "InternalFeed".into(),
        kind: SourceKind::Internal {
            sheet: "Data".into(),
            rect: (0, 0, 3, 1),
        },
        at: (5, 0),
        target_sheet: Some("Aux".into()),
        rows: 4,
        cols: 2,
        csv_opts: None,
    });

    // Pivots referencing Data rows 21..=23, cols 0..=2 (disjoint from rows 0..10 safe CellShift band)
    let spec2 = PivotSpec2 {
        source: (21, 0, 23, 2),
        rows: vec![AxisField::plain(0)],
        cols: Vec::new(),
        values: vec![ValueField {
            col: 1,
            agg: Agg::Sum,
            label: None,
            show_as: ShowAs::Normal,
            base: None,
        }],
        filters: Vec::new(),
        layout: PivotLayout::default(),
    };
    let mut piv = PivotTableModel::new(
        "Pivot1".into(),
        "Data".into(),
        Some(data_id),
        1,
        (10, 0),
        spec2.clone(),
    );
    piv.rect = (10, 0, 14, 2);
    piv.last_meta.anchor = (10, 0);
    piv.last_meta.rect = (10, 0, 14, 2);
    piv.options.slicers.push(PivotSlicerSpec {
        id: 101,
        col: 2,
        title: "Region".into(),
        pos: [24.0, 48.0],
        width: 140.0,
        columns: 1,
    });
    wb.pivots.push(piv);

    wb.pivots_pending.push((
        0,
        PivotSpec {
            source: (21, 0, 23, 2),
            row_field: 0,
            col_field: None,
            value_field: 1,
            agg: Agg::Sum,
            group: GroupBy::None,
            exclude: Vec::new(),
        },
        PivotResult::default(),
        (21, 8, 24, 10),
        "SumVal".into(),
    ));
    wb.pivots_pending2
        .push((0, spec2, PivotGrid::default(), (21, 12, 24, 14)));

    wb.dirty = false;
    RichFixtures { wb, style_ids }
}

/// Mirror collaborative late-join cell snapshot installation: overwrite cell
/// values/formulas on `dst` to match `src` editable inputs (leaving all styles
/// and metadata on `dst` untouched until `TxOverlay::apply_to` runs).
fn sync_authoritative_cells(src: &Workbook, dst: &mut Workbook) {
    assert_eq!(src.sheets.len(), dst.sheets.len());
    for (s_src, s_dst) in src.sheets.iter().zip(&mut dst.sheets) {
        s_dst.clear();
        for (k, view) in s_src.iter_views() {
            let cell = if let Some(f) = view.formula() {
                Cell {
                    value: Value::Empty,
                    formula: Some(f.into()),
                }
            } else {
                Cell {
                    value: view.value().into_owned(),
                    formula: None,
                }
            };
            s_dst.set(k.row, k.col, cell);
        }
    }
}

// ============================================================================
// Invariant 1 — Transaction Failure Atomicity
// ============================================================================

#[test]
fn invariant_1_transaction_failure_atomicity() {
    const SEEDS: u64 = 16;
    const STEPS: usize = 36;

    let mut err_overflow = 0usize;
    let mut err_partial_merge = 0usize;
    let mut err_table = 0usize;
    let mut err_pivot = 0usize;
    let mut err_array_spill = 0usize;
    let mut err_autofilter = 0usize;
    let mut err_sparkline = 0usize;
    let mut err_protected_or_scenario = 0usize;
    let mut err_partial_hyperlink = 0usize;
    let mut err_defined_name = 0usize;
    let mut err_sheet_tab = 0usize;
    let mut err_invalid_coord = 0usize;
    let mut err_noop = 0usize;
    let mut ok_count = 0usize;

    for seed_idx in 0..SEEDS {
        let seed = 0x1001_0000 + seed_idx * 0x9E37 + 17;
        let mut rng = Rng::new(seed);
        let mut trace = Trace::new("Invariant1:FailureAtomicity", seed);
        let fix = build_rich_workbook(seed);
        let mut wb = fix.wb;
        let mut undo = UndoStack::new();

        // Populate tail-boundary sentinels on every seed so overflow failures hit
        // both cells and metadata at MAX_ROWS - 1 / MAX_COLS - 1.
        let tail_mode = seed_idx % 4;
        match tail_mode {
            0 => {
                wb.sheets[0].set(MAX_ROWS - 1, 0, num(999.0));
                wb.sheets[0].set(0, MAX_COLS - 1, txt("EndCol"));
            }
            1 => {
                wb.sheets[0].set_style(MAX_ROWS - 1, 1, fix.style_ids[1]);
                wb.sheets[0].set_style(1, MAX_COLS - 1, fix.style_ids[2]);
            }
            2 => {
                wb.sheets[0].notes.insert(
                    CellRef::new(MAX_ROWS - 1, 2),
                    Note {
                        r: MAX_ROWS - 1,
                        c: 2,
                        author: "Edge".into(),
                        text: "Bottom".into(),
                    },
                );
                wb.sheets[0].merge_range(2, MAX_COLS - 2, 2, MAX_COLS - 1);
            }
            _ => {
                wb.sheets[0].sparklines.push(Sparkline {
                    r: MAX_ROWS - 1,
                    c: 0,
                    source_sheet: None,
                    rect: (0, 1, 2, 1),
                    ..Sparkline::default()
                });
                wb.sheets[0].set(3, MAX_COLS - 1, fml("=A1+1"));
            }
        }
        // Add a straddling sparkline on Report referencing Data!(1,15,1,17) and a multi-cell
        // hyperlink at (6,18)..=(6,19) in clear lanes so Sparkline & PartialHyperlink conflicts
        // fire unshadowed by Merge/Table/Pivot/Filter.
        wb.sheets[1].sparklines.push(Sparkline {
            r: 5,
            c: 5,
            source_sheet: Some("Data".into()),
            rect: (1, 15, 1, 17),
            ..Sparkline::default()
        });
        wb.sheets[0].hyperlinks.insert(
            (6, 18),
            Hyperlink {
                target: "https://example.invalid/clearlane".into(),
                external: true,
                span: (0, 1),
                ..Default::default()
            },
        );

        let mut next_name_id = 0usize;
        let mut next_sheet_id = 0usize;

        for step in 0..STEPS {
            // 65% deliberately failing ops, 35% valid ops.
            let (desc, intent): (String, TxIntent) = if rng.chance(65, 100) {
                match rng.range_u32(0, 17) {
                    // Tail overflow: whole-axis InsertRows
                    0 => (
                        "FAIL InsertRows overflow".into(),
                        TxIntent::from_structural(0, &Structural::InsertRows { at: 0, n: 1 }),
                    ),
                    // Tail overflow: whole-axis InsertCols
                    1 => (
                        "FAIL InsertCols overflow".into(),
                        TxIntent::from_structural(0, &Structural::InsertCols { at: 0, n: 1 }),
                    ),
                    // Tail overflow: MultiInsert rows/cols
                    2 => {
                        let ax = if rng.bool() { Axis::Row } else { Axis::Col };
                        (
                            format!("FAIL MultiInsert({ax:?}) overflow"),
                            TxIntent::from_multi_insert(0, ax, vec![1, 3]),
                        )
                    }
                    // Tail overflow: CellShift insert in occupied tail band
                    3 => {
                        let op = CellShift::new((0, 0, 0, 2), CellShiftDir::Vertical, true);
                        ("FAIL CellShift overflow".into(), TxIntent::from_cell_shift(0, &op))
                    }
                    // Partial merge: sheet[0] has merge (8,0,9,1)
                    4 => {
                        let op = CellShift::new((8, 0, 8, 0), CellShiftDir::Vertical, true);
                        ("FAIL CellShift PartialMerge".into(), TxIntent::from_cell_shift(0, &op))
                    }
                    // Table overlap: Table1 at (12,0,15,2)
                    5 => {
                        let op = CellShift::new((12, 0, 12, 0), CellShiftDir::Vertical, false);
                        ("FAIL CellShift Table".into(), TxIntent::from_cell_shift(0, &op))
                    }
                    // Pivot overlap: Pivot1 source at (21,0,23,2)
                    6 => {
                        let op = CellShift::new((21, 0, 21, 0), CellShiftDir::Vertical, false);
                        ("FAIL CellShift Pivot".into(), TxIntent::from_cell_shift(0, &op))
                    }
                    // Array / spill overlap: arrays/spills at (12..=14, 4..=5)
                    7 => {
                        let op = CellShift::new((12, 4, 12, 4), CellShiftDir::Vertical, false);
                        ("FAIL CellShift ArrayOrSpill".into(), TxIntent::from_cell_shift(0, &op))
                    }
                    // AutoFilter overlap: filter at (17..=20, 0..=2); use horizontal shift so Pivot at rows 21..=23 doesn't shadow it!
                    8 => {
                        let op = CellShift::new((17, 1, 17, 1), CellShiftDir::Horizontal, false);
                        ("FAIL CellShift AutoFilter".into(), TxIntent::from_cell_shift(0, &op))
                    }
                    // Straddling sparkline: Report sparkline reads Data!(1,15,1,17); col 16 is clear of other objects.
                    9 => {
                        let op = CellShift::new((1, 16, 1, 16), CellShiftDir::Vertical, false);
                        ("FAIL CellShift Sparkline".into(), TxIntent::from_cell_shift(0, &op))
                    }
                    // ProtectedRange or Scenario overlap at (17..=19, 4..=6)
                    10 => {
                        let col = if rng.bool() { 5 } else { 4 };
                        let op = CellShift::new((17, col, 17, col), CellShiftDir::Vertical, false);
                        (
                            format!("FAIL CellShift Prot/Scenario col={col}"),
                            TxIntent::from_cell_shift(0, &op),
                        )
                    }
                    // Partial multi-cell hyperlink at (6,18)..=(6,19) in clear lane
                    11 => {
                        let op = CellShift::new((6, 18, 6, 18), CellShiftDir::Vertical, false);
                        (
                            "FAIL CellShift PartialHyperlink".into(),
                            TxIntent::from_cell_shift(0, &op),
                        )
                    }
                    // Invalid / duplicate DefinedName
                    12 => match rng.range_u32(0, 4) {
                        0 => (
                            "FAIL Define empty name".into(),
                            TxIntent::from_defined_names(DefinedNameIntent::Define {
                                name: "   ".into(),
                                scope: None,
                                formula: "1".into(),
                            }),
                        ),
                        1 => (
                            "FAIL Define duplicate name".into(),
                            TxIntent::from_defined_names(DefinedNameIntent::Define {
                                name: "totalrev".into(),
                                scope: None,
                                formula: "99".into(),
                            }),
                        ),
                        2 => (
                            "FAIL Remove OOB name".into(),
                            TxIntent::from_defined_names(DefinedNameIntent::Remove {
                                index: wb.names.len() + 5,
                            }),
                        ),
                        _ => (
                            "FAIL Define invalid scope".into(),
                            TxIntent::from_defined_names(DefinedNameIntent::Define {
                                name: "BadScope".into(),
                                scope: Some(wb.sheets.len() + 3),
                                formula: "1".into(),
                            }),
                        ),
                    },
                    // Invalid SheetTab (bad name / duplicate name / OOB index)
                    13 => match rng.range_u32(0, 4) {
                        0 => (
                            "FAIL Append duplicate sheet".into(),
                            TxIntent::from_sheet_tab(SheetTabIntent::Append {
                                sheet: Sheet::new("data"),
                            }),
                        ),
                        1 => (
                            "FAIL Append invalid chars".into(),
                            TxIntent::from_sheet_tab(SheetTabIntent::Append {
                                sheet: Sheet::new("Bad/Sheet*"),
                            }),
                        ),
                        2 => (
                            "FAIL Rename OOB sheet".into(),
                            TxIntent::from_sheet_tab(SheetTabIntent::Rename {
                                index: wb.sheets.len() + 2,
                                name: "Ghost".into(),
                            }),
                        ),
                        _ => (
                            "FAIL Delete OOB sheet".into(),
                            TxIntent::from_sheet_tab(SheetTabIntent::Delete {
                                index: wb.sheets.len() + 1,
                            }),
                        ),
                    },
                    // Invalid CellBatch coord
                    14 => {
                        let bad = if rng.bool() {
                            CellWrite::cell(MAX_ROWS, 0, num(1.0))
                        } else {
                            CellWrite::cell(0, MAX_COLS, num(1.0))
                        };
                        (
                            "FAIL CellBatch OOB coord".into(),
                            TxIntent::from_cell_batch(0, vec![CellWrite::cell(0, 0, num(77.0)), bad]),
                        )
                    }
                    // NoOp transactions
                    15 => match rng.range_u32(0, 3) {
                        0 => (
                            "FAIL Structural NoOp".into(),
                            TxIntent::from_structural(0, &Structural::InsertRows { at: 0, n: 0 }),
                        ),
                        1 => (
                            "FAIL MultiInsert empty NoOp".into(),
                            TxIntent::from_multi_insert(0, Axis::Row, vec![]),
                        ),
                        _ => (
                            "FAIL SheetTab Move NoOp".into(),
                            TxIntent::from_sheet_tab(SheetTabIntent::Move { from: 0, to: 0 }),
                        ),
                    },
                    // Out-of-bounds target sheet for Transform
                    _ => (
                        "FAIL Transform InvalidSheet".into(),
                        TxIntent::from_structural(99, &Structural::DeleteRows { at: 0, n: 1 }),
                    ),
                }
            } else {
                // Valid mutating ops (keeping tail edge intact so future overflow probes still fire!)
                match rng.range_u32(0, 5) {
                    0 => {
                        let r = rng.range_u32(0, 6);
                        let c = rng.range_u32(0, 4);
                        let v = rng.range_u32(1, 500) as f64;
                        let st = fix.style_ids[rng.range_usize(0, fix.style_ids.len())];
                        (
                            format!("OK CellBatch ({r},{c})={v} st={st}"),
                            TxIntent::from_cell_batch(0, vec![CellWrite::cell_and_style(r, c, num(v), st)]),
                        )
                    }
                    1 => {
                        next_name_id += 1;
                        let nm = format!("N_{seed_idx}_{next_name_id}");
                        (
                            format!("OK Define {nm}"),
                            TxIntent::from_defined_names(DefinedNameIntent::Define {
                                name: nm,
                                scope: None,
                                formula: "Data!$B$1".into(),
                            }),
                        )
                    }
                    2 => {
                        // Safe AxisPermute in rows 0..6 (does not touch tail MAX_ROWS-1)
                        let from = rng.range_u32(0, 4);
                        let at = if from < 2 { from + 2 } else { 0 };
                        (
                            format!("OK AxisPermute Row {from}..+1 -> {at}"),
                            TxIntent::from_axis_permute(0, Axis::Row, from, 1, at).unwrap(),
                        )
                    }
                    3 => {
                        // Safe CellShift in col 3 (empty at tail, no table/pivot/array/filter/merge)
                        let insert = rng.bool();
                        let op = CellShift::new((0, 3, 0, 3), CellShiftDir::Vertical, insert);
                        (
                            format!("OK CellShift col 3 insert={insert}"),
                            TxIntent::from_cell_shift(0, &op),
                        )
                    }
                    _ => {
                        next_sheet_id += 1;
                        let sname = format!("Extra_{seed_idx}_{next_sheet_id}");
                        (
                            format!("OK Append sheet {sname}"),
                            TxIntent::from_sheet_tab(SheetTabIntent::Append {
                                sheet: Sheet::new(sname),
                            }),
                        )
                    }
                }
            };

            let before_state = BusinessState::capture(&wb);
            let before_raw_styles = capture_raw_styles(&wb);
            let before_epoch = wb.coord_epoch();
            let before_depth = undo.depth();
            let before_dirty = wb.dirty;

            match WorkbookTx::execute(&mut wb, intent, (0, 0, 0)) {
                Ok(commit) => {
                    trace.record(step, format!("{desc} -> Ok"));
                    ok_count += 1;
                    let _ = undo.push_tx(commit);
                    assert_physical_bounds(&wb, &trace, step, "after Ok");
                }
                Err(err) => {
                    trace.record(step, format!("{desc} -> Err({err:?})"));
                    match err {
                        TxError::Conflict(CellShiftConflict::Overflow) => err_overflow += 1,
                        TxError::Conflict(CellShiftConflict::PartialMerge) => err_partial_merge += 1,
                        TxError::Conflict(CellShiftConflict::Table) => err_table += 1,
                        TxError::Conflict(CellShiftConflict::Pivot) => err_pivot += 1,
                        TxError::Conflict(CellShiftConflict::ArrayOrSpill) => err_array_spill += 1,
                        TxError::Conflict(CellShiftConflict::AutoFilter) => err_autofilter += 1,
                        TxError::Conflict(CellShiftConflict::Sparkline) => err_sparkline += 1,
                        TxError::Conflict(CellShiftConflict::ProtectedRange)
                        | TxError::Conflict(CellShiftConflict::Scenario) => {
                            err_protected_or_scenario += 1
                        }
                        TxError::Conflict(CellShiftConflict::PartialHyperlink) => {
                            err_partial_hyperlink += 1
                        }
                        TxError::InvalidDefinedName | TxError::DuplicateDefinedName => {
                            err_defined_name += 1
                        }
                        TxError::InvalidSheet | TxError::InvalidSheetName => err_sheet_tab += 1,
                        TxError::InvalidCellCoord => err_invalid_coord += 1,
                        TxError::NoOp => err_noop += 1,
                        _ => {}
                    }

                    let after_state = BusinessState::capture(&wb);
                    assert_business_state_eq(
                        &before_state,
                        &after_state,
                        &trace,
                        step,
                        &format!("Err({err:?}) must leave BusinessState identical"),
                    );
                    assert_eq!(
                        capture_raw_styles(&wb),
                        before_raw_styles,
                        "{}",
                        trace.dump(step, &format!("Err({err:?}) mutated raw styles"))
                    );
                    assert_eq!(
                        wb.coord_epoch(),
                        before_epoch,
                        "{}",
                        trace.dump(step, &format!("Err({err:?}) mutated coord_epoch"))
                    );
                    assert_eq!(
                        undo.depth(),
                        before_depth,
                        "{}",
                        trace.dump(step, &format!("Err({err:?}) mutated UndoStack::depth"))
                    );
                    assert_eq!(
                        wb.dirty,
                        before_dirty,
                        "{}",
                        trace.dump(step, &format!("Err({err:?}) mutated wb.dirty"))
                    );
                }
            }
        }
    }

    // Explicitly verify CannotDeleteLastSheet atomicity on a 1-sheet workbook.
    {
        let mut wb = Workbook::new_blank();
        wb.sheets[0].set(0, 0, num(42.0));
        wb.dirty = false;
        let undo = UndoStack::new();
        let before = BusinessState::capture(&wb);
        let epoch = wb.coord_epoch();
        let err = WorkbookTx::execute(
            &mut wb,
            TxIntent::from_sheet_tab(SheetTabIntent::Delete { index: 0 }),
            (0, 0, 0),
        )
        .unwrap_err();
        assert_eq!(err, TxError::CannotDeleteLastSheet);
        assert_eq!(BusinessState::capture(&wb), before);
        assert_eq!(wb.coord_epoch(), epoch);
        assert_eq!(undo.depth(), (0, 0));
        assert!(!wb.dirty);
    }

    // Prove non-vacuous coverage across ALL expected error categories.
    assert!(ok_count > 50, "expected valid ops mixed in, got {ok_count}");
    assert!(err_overflow > 10, "overflow errors under-exercised: {err_overflow}");
    assert!(err_partial_merge > 0, "PartialMerge never triggered");
    assert!(err_table > 0, "Table conflict never triggered");
    assert!(err_pivot > 0, "Pivot conflict never triggered");
    assert!(err_array_spill > 0, "ArrayOrSpill conflict never triggered");
    assert!(err_autofilter > 0, "AutoFilter conflict never triggered");
    assert!(err_sparkline > 0, "Sparkline conflict never triggered");
    assert!(
        err_protected_or_scenario > 0,
        "ProtectedRange/Scenario conflict never triggered"
    );
    assert!(err_partial_hyperlink > 0, "PartialHyperlink never triggered");
    assert!(err_defined_name > 0, "DefinedName error never triggered");
    assert!(err_sheet_tab > 0, "SheetTab error never triggered");
    assert!(err_invalid_coord > 0, "InvalidCellCoord never triggered");
    assert!(err_noop > 0, "NoOp never triggered");
}

// ============================================================================
// Invariant 2 — Apply -> Undo -> Redo -> Undo Business-State Equivalence
// ============================================================================

#[test]
fn invariant_2_apply_undo_redo_undo_business_state_equivalence() {
    const SEEDS: u64 = 16;
    const STEPS: usize = 28;

    let mut family_counts = [0usize; 6]; // CellBatch, Structural, CellShift, AxisPermute, DefinedNames, SheetTab

    for seed_idx in 0..SEEDS {
        let seed = 0x2002_0000 + seed_idx * 0x517C + 31;
        let mut rng = Rng::new(seed);
        let mut trace = Trace::new("Invariant2:UndoRedoEquivalence", seed);
        let fix = build_rich_workbook(seed);
        let mut wb = fix.wb;
        let mut undo = UndoStack::new();

        let initial_state = BusinessState::capture(&wb);
        let mut pre_step_states: Vec<BusinessState> = Vec::new();
        let mut post_step_states: Vec<BusinessState> = Vec::new();

        let mut serial = 0usize;

        for step in 0..STEPS {
            serial += 1;
            let family = if step < 6 {
                // Guarantee every seed exercises all 6 transaction families at least once.
                step
            } else {
                rng.range_usize(0, 6)
            };

            let (desc, intent): (String, TxIntent) = match family {
                // 0: CellBatch including ALL rich-text overwrite modes
                0 => {
                    let rich_targets = [(0u32, 4u32), (2, 4), (4, 0)];
                    let &(rr, cc) = &rich_targets[rng.range_usize(0, rich_targets.len())];
                    let st = fix.style_ids[rng.range_usize(1, fix.style_ids.len())];
                    let (mode_name, w1) = match rng.range_u32(0, 6) {
                        0 => ("rich->num", CellWrite::cell(rr, cc, num(serial as f64))),
                        1 => ("rich->formula", CellWrite::cell(rr, cc, fml("=B1+1"))),
                        2 => ("rich->newtext", CellWrite::cell(rr, cc, txt(&format!("Plain_{serial}")))),
                        3 => {
                            let cur = wb.sheets[0].value(rr, cc).display();
                            ("rich->sametext", CellWrite::cell(rr, cc, txt(&cur)))
                        }
                        4 => ("rich->clear", CellWrite::cell(rr, cc, Cell::default())),
                        _ => ("rich->style_only", CellWrite::style(rr, cc, st)),
                    };
                    let extra_r = rng.range_u32(0, 6);
                    let extra_c = rng.range_u32(0, 4);
                    let w2 = CellWrite::cell_and_style(extra_r, extra_c, num((serial * 3) as f64), st);
                    (
                        format!("CellBatch[{mode_name} ({rr},{cc}) + ({extra_r},{extra_c})]"),
                        TxIntent::from_cell_batch(0, vec![w1, w2]),
                    )
                }
                // 1: Whole-axis Structural (Insert/Delete Rows/Cols or MultiInsert)
                1 => {
                    let s_idx = 0usize;
                    match rng.range_u32(0, 5) {
                        0 => {
                            let at = rng.range_u32(0, 15);
                            let n = rng.range_u32(1, 3);
                            (
                                format!("Structural InsertRows at={at} n={n}"),
                                TxIntent::from_structural(s_idx, &Structural::InsertRows { at, n }),
                            )
                        }
                        1 => {
                            let at = rng.range_u32(1, 14);
                            let n = rng.range_u32(1, 3);
                            (
                                format!("Structural DeleteRows at={at} n={n}"),
                                TxIntent::from_structural(s_idx, &Structural::DeleteRows { at, n }),
                            )
                        }
                        2 => {
                            let at = rng.range_u32(0, 8);
                            let n = rng.range_u32(1, 3);
                            (
                                format!("Structural InsertCols at={at} n={n}"),
                                TxIntent::from_structural(s_idx, &Structural::InsertCols { at, n }),
                            )
                        }
                        3 => {
                            let at = rng.range_u32(1, 7);
                            let n = rng.range_u32(1, 2);
                            (
                                format!("Structural DeleteCols at={at} n={n}"),
                                TxIntent::from_structural(s_idx, &Structural::DeleteCols { at, n }),
                            )
                        }
                        _ => {
                            let ax = if rng.bool() { Axis::Row } else { Axis::Col };
                            let p1 = rng.range_u32(0, 4);
                            let p2 = p1 + rng.range_u32(1, 4);
                            (
                                format!("MultiInsert({ax:?}) [{p1},{p2}]"),
                                TxIntent::from_multi_insert(s_idx, ax, vec![p2, p1]),
                            )
                        }
                    }
                }
                // 2: Partial-band CellShift (vertical in safe col-3 lane or horizontal in safe rows 0..5)
                2 => {
                    let vert = rng.bool();
                    let insert = rng.bool();
                    let (rect, dir) = if vert {
                        let r1 = rng.range_u32(0, 5);
                        let c1 = if rng.chance(75, 100) { 3 } else { rng.range_u32(1, 4) };
                        ((r1, c1, r1, c1), CellShiftDir::Vertical)
                    } else {
                        let r1 = rng.range_u32(1, 5);
                        let c1 = rng.range_u32(2, 4);
                        ((r1, c1, r1, c1), CellShiftDir::Horizontal)
                    };
                    let op = CellShift::new(rect, dir, insert);
                    (
                        format!("CellShift rect={rect:?} dir={dir:?} ins={insert}"),
                        TxIntent::from_cell_shift(0, &op),
                    )
                }
                // 3: AxisPermute (row or col drag move across data + metadata regions)
                3 => {
                    let ax = if rng.bool() { Axis::Row } else { Axis::Col };
                    let (from, n, at) = if ax == Axis::Row {
                        let f = rng.range_u32(0, 16);
                        let n = rng.range_u32(1, 3);
                        let mut a = rng.range_u32(0, 18);
                        if a == f {
                            a = (f + 3) % 18;
                        }
                        (f, n, a)
                    } else {
                        let f = rng.range_u32(0, 7);
                        let n = rng.range_u32(1, 3);
                        let mut a = rng.range_u32(0, 8);
                        if a == f {
                            a = (f + 2) % 8;
                        }
                        (f, n, a)
                    };
                    (
                        format!("AxisPermute({ax:?}) {from}..+{n} -> {at}"),
                        TxIntent::from_axis_permute(0, ax, from, n, at).unwrap(),
                    )
                }
                // 4: DefinedNames (Define / Update / Remove / ReplaceAll)
                4 => {
                    let shared_indices: Vec<usize> = wb
                        .names
                        .iter()
                        .enumerate()
                        .filter(|(_, (n, _, _))| !n.starts_with("_sc_src_"))
                        .map(|(i, _)| i)
                        .collect();
                    match rng.range_u32(0, 4) {
                        0 => {
                            let nm = format!("Dyn_{seed_idx}_{serial}");
                            let scope = if rng.bool() {
                                None
                            } else {
                                Some(rng.range_usize(0, wb.sheets.len()))
                            };
                            (
                                format!("DefinedNames::Define {nm}"),
                                TxIntent::from_defined_names(DefinedNameIntent::Define {
                                    name: nm,
                                    scope,
                                    formula: "Data!$B$2".into(),
                                }),
                            )
                        }
                        1 if !shared_indices.is_empty() => {
                            let idx = shared_indices[rng.range_usize(0, shared_indices.len())];
                            let (old_n, old_sc, _) = wb.names[idx].clone();
                            (
                                format!("DefinedNames::Update[{idx}] {old_n}"),
                                TxIntent::from_defined_names(DefinedNameIntent::Update {
                                    index: idx,
                                    name: old_n,
                                    scope: old_sc,
                                    formula: format!("Data!$B${}", rng.range_u32(1, 7)),
                                }),
                            )
                        }
                        2 if shared_indices.len() > 1 => {
                            let idx = shared_indices[rng.range_usize(0, shared_indices.len())];
                            (
                                format!("DefinedNames::Remove[{idx}]"),
                                TxIntent::from_defined_names(DefinedNameIntent::Remove { index: idx }),
                            )
                        }
                        _ => {
                            let nm = format!("Rep_{seed_idx}_{serial}");
                            (
                                format!("DefinedNames::Define(fallback) {nm}"),
                                TxIntent::from_defined_names(DefinedNameIntent::Define {
                                    name: nm,
                                    scope: None,
                                    formula: "123".into(),
                                }),
                            )
                        }
                    }
                }
                // 5: SheetTab (Append / Insert / Rename / Move / Delete)
                _ => match rng.range_u32(0, 5) {
                    0 => {
                        let name = format!("Tab_{seed_idx}_{serial}");
                        let mut sh = Sheet::new(name.clone());
                        sh.set(0, 0, num(serial as f64));
                        (
                            format!("SheetTab::Append {name}"),
                            TxIntent::from_sheet_tab(SheetTabIntent::Append { sheet: sh }),
                        )
                    }
                    1 => {
                        let name = format!("Ins {serial}");
                        let idx = rng.range_usize(1, wb.sheets.len() + 1);
                        (
                            format!("SheetTab::Insert[{idx}] '{name}'"),
                            TxIntent::from_sheet_tab(SheetTabIntent::Insert {
                                index: idx,
                                sheet: Sheet::new(name),
                            }),
                        )
                    }
                    2 => {
                        // Rename a non-primary sheet (index >= 1) with occasional space in name
                        let idx = rng.range_usize(1, wb.sheets.len());
                        let new_name = if rng.bool() {
                            format!("Renamed {serial}")
                        } else {
                            format!("Sh_{serial}")
                        };
                        (
                            format!("SheetTab::Rename[{idx}] -> '{new_name}'"),
                            TxIntent::from_sheet_tab(SheetTabIntent::Rename {
                                index: idx,
                                name: new_name,
                            }),
                        )
                    }
                    3 if wb.sheets.len() >= 3 => {
                        // Move among sheets[1..] so sheet 0 ("Data") stays at index 0 for subsequent ops
                        let from = rng.range_usize(1, wb.sheets.len());
                        let to = if from + 1 < wb.sheets.len() {
                            from + 1
                        } else {
                            1
                        };
                        (
                            format!("SheetTab::Move {from}->{to}"),
                            TxIntent::from_sheet_tab(SheetTabIntent::Move { from, to }),
                        )
                    }
                    4 if wb.sheets.len() > 3 => {
                        let idx = wb.sheets.len() - 1;
                        (
                            format!("SheetTab::Delete[{idx}]"),
                            TxIntent::from_sheet_tab(SheetTabIntent::Delete { index: idx }),
                        )
                    }
                    _ => {
                        let name = format!("ExtraTab_{serial}");
                        (
                            format!("SheetTab::Append(fallback) {name}"),
                            TxIntent::from_sheet_tab(SheetTabIntent::Append {
                                sheet: Sheet::new(name),
                            }),
                        )
                    }
                },
            };

            let before_step = BusinessState::capture(&wb);
            let commit = match WorkbookTx::execute(&mut wb, intent, (0, 0, 0)) {
                Ok(c) => c,
                Err(_) => {
                    // Valid randomized conflict (e.g. CellShift hit a shifted object); state must be untouched.
                    assert_business_state_eq(
                        &before_step,
                        &BusinessState::capture(&wb),
                        &trace,
                        step,
                        "rejected randomized step mutated state",
                    );
                    continue;
                }
            };

            let entry_id = undo.push_tx(commit);
            if entry_id == 0 {
                // Empty delta (e.g., same-text CellBatch or empty CellShift band): state must be unchanged.
                assert_business_state_eq(
                    &before_step,
                    &BusinessState::capture(&wb),
                    &trace,
                    step,
                    "zero-delta commit mutated state",
                );
                continue;
            }

            family_counts[family] += 1;
            trace.record(step, desc.clone());
            assert_physical_bounds(&wb, &trace, step, "forward");

            let after_step = BusinessState::capture(&wb);

            // Step-by-step immediate Apply -> Undo -> Redo equivalence!
            let undone = undo
                .undo_workbook(&mut wb, allow_all)
                .unwrap_or_else(|| panic!("{}", trace.dump(step, "immediate undo returned None")));
            assert_eq!(
                undone.skipped,
                0,
                "{}",
                trace.dump(step, "immediate undo unexpectedly skipped cells")
            );
            assert_business_state_eq(
                &before_step,
                &BusinessState::capture(&wb),
                &trace,
                step,
                &format!("immediate Undo after {desc}"),
            );

            let redone = undo
                .redo_workbook(&mut wb, allow_all)
                .unwrap_or_else(|| panic!("{}", trace.dump(step, "immediate redo returned None")));
            assert_eq!(
                redone.skipped,
                0,
                "{}",
                trace.dump(step, "immediate redo unexpectedly skipped cells")
            );
            assert_business_state_eq(
                &after_step,
                &BusinessState::capture(&wb),
                &trace,
                step,
                &format!("immediate Redo after {desc}"),
            );

            pre_step_states.push(before_step);
            post_step_states.push(after_step);
        }

        let n = pre_step_states.len();
        assert!(n >= 10, "seed {seed:#x} executed too few ops: {n}");
        assert_eq!(undo.depth(), (n, 0));

        // Multi-step deep unwind: N steps Undo -> back to exact initial_state!
        for k in (0..n).rev() {
            let undone = undo
                .undo_workbook(&mut wb, allow_all)
                .unwrap_or_else(|| panic!("{}", trace.dump(k, "deep undo #1 returned None")));
            assert_eq!(undone.skipped, 0);
            assert_business_state_eq(
                &pre_step_states[k],
                &BusinessState::capture(&wb),
                &trace,
                k,
                &format!("deep Undo #1 at k={k}"),
            );
        }
        assert_eq!(undo.depth(), (0, n));
        assert_business_state_eq(
            &initial_state,
            &BusinessState::capture(&wb),
            &trace,
            0,
            "deep Undo #1 reached initial_state",
        );

        // Multi-step deep replay: N steps Redo -> matches every recorded forward state!
        for k in 0..n {
            let redone = undo
                .redo_workbook(&mut wb, allow_all)
                .unwrap_or_else(|| panic!("{}", trace.dump(k, "deep redo returned None")));
            assert_eq!(redone.skipped, 0);
            assert_business_state_eq(
                &post_step_states[k],
                &BusinessState::capture(&wb),
                &trace,
                k,
                &format!("deep Redo at k={k}"),
            );
        }
        assert_eq!(undo.depth(), (n, 0));

        // Second full unwind: N steps Undo -> exact initial_state again!
        for k in (0..n).rev() {
            let undone = undo
                .undo_workbook(&mut wb, allow_all)
                .unwrap_or_else(|| panic!("{}", trace.dump(k, "deep undo #2 returned None")));
            assert_eq!(undone.skipped, 0);
            assert_business_state_eq(
                &pre_step_states[k],
                &BusinessState::capture(&wb),
                &trace,
                k,
                &format!("deep Undo #2 at k={k}"),
            );
        }
        assert_eq!(undo.depth(), (0, n));
        assert_business_state_eq(
            &initial_state,
            &BusinessState::capture(&wb),
            &trace,
            0,
            "deep Undo #2 reached initial_state",
        );
    }

    for (idx, &cnt) in family_counts.iter().enumerate() {
        assert!(cnt >= 16, "tx family {idx} under-exercised: count={cnt}");
    }
}

#[test]
fn invariant_2_cell_batch_rich_text_all_transitions_and_conflict_guard() {
    let mut wb = Workbook::new_blank();
    let st_bold = wb.styles.intern(CellStyle {
        bold: true,
        ..CellStyle::default_dark()
    });

    // Populate 6 distinct rich text cells to test every transition cleanly.
    let mut orig_runs: Vec<((u32, u32), Vec<RichRun>)> = Vec::new();
    for r in 0..6u32 {
        let (full, runs) = make_rich_runs("Revenue ", &format!("part{r}"), [0xAA, 0x11, r as u8 * 30]);
        wb.sheets[0].set(r, 0, txt(&full));
        wb.sheets[0].rich.insert((r, 0), runs.clone());
        orig_runs.push(((r, 0), runs));
    }

    let initial = BusinessState::capture(&wb);
    let mut undo = UndoStack::new();
    let trace = Trace::new("Invariant2:RichBatchCases", 0);

    // Single atomic CellBatch covering:
    // row 0: text -> number
    // row 1: text -> formula
    // row 2: text -> different text
    // row 3: text -> clear (blank)
    // row 4: text -> same text (rich runs MUST be preserved!)
    // row 5: style-only write (rich runs MUST be preserved!)
    let writes = vec![
        CellWrite::cell(0, 0, num(99.0)),
        CellWrite::cell(1, 0, fml("=10+20")),
        CellWrite::cell(2, 0, txt("Replaced plain text")),
        CellWrite::cell(3, 0, Cell::default()),
        CellWrite::cell(4, 0, txt("Revenue part4")),
        CellWrite::style(5, 0, st_bold),
    ];
    let commit = WorkbookTx::execute(&mut wb, TxIntent::from_cell_batch(0, writes), (0, 0, 0)).unwrap();
    assert!(undo.push_tx(commit) > 0);

    // Forward checks
    assert!(!wb.sheets[0].rich.contains_key(&(0, 0)), "text->num must clear rich");
    assert!(!wb.sheets[0].rich.contains_key(&(1, 0)), "text->formula must clear rich");
    assert!(!wb.sheets[0].rich.contains_key(&(2, 0)), "text->newtext must clear rich");
    assert!(!wb.sheets[0].rich.contains_key(&(3, 0)), "text->clear must clear rich");
    assert_eq!(wb.sheets[0].rich.get(&(4, 0)), Some(&orig_runs[4].1), "same text must keep rich");
    assert_eq!(wb.sheets[0].rich.get(&(5, 0)), Some(&orig_runs[5].1), "style-only must keep rich");
    let after = BusinessState::capture(&wb);

    // Undo -> exact initial state (all 6 RichRuns restored!)
    undo.undo_workbook(&mut wb, allow_all).unwrap();
    assert_business_state_eq(&initial, &BusinessState::capture(&wb), &trace, 1, "rich batch undo");

    // Redo -> exact after state (rows 0..=3 cleared again, rows 4..=5 preserved!)
    undo.redo_workbook(&mut wb, allow_all).unwrap();
    assert_business_state_eq(&after, &BusinessState::capture(&wb), &trace, 2, "rich batch redo");

    // Conflict guard: peer concurrently overwrites row 0 with NEW rich text;
    // Undo MUST skip row 0 (preserving peer's text & RichRun!) while still restoring rows 1..=3!
    let (peer_text, peer_runs) = make_rich_runs("Peer ", "LiveEdit", [0x11, 0x88, 0x22]);
    wb.sheets[0].set(0, 0, txt(&peer_text));
    wb.sheets[0].rich.insert((0, 0), peer_runs.clone());

    let undone = undo.undo_workbook(&mut wb, allow_all).unwrap();
    assert_eq!(undone.skipped, 1, "conflicting row 0 must be skipped");
    assert_eq!(wb.sheets[0].value(0, 0), Value::Text(peer_text.into()));
    assert_eq!(wb.sheets[0].rich.get(&(0, 0)), Some(&peer_runs), "skipped cell live rich must be untouched");
    for r in 1..6u32 {
        assert_eq!(
            wb.sheets[0].rich.get(&(r, 0)),
            Some(&orig_runs[r as usize].1),
            "non-conflicting row {r} rich must be restored"
        );
    }
}

// ============================================================================
// Invariant 3 — Online State vs Late-Join `TxOverlay` Equivalence
// ============================================================================

#[test]
fn invariant_3_online_vs_late_join_tx_overlay_equivalence() {
    const SEEDS: u64 = 14;
    const STEPS: usize = 24;

    let mut total_cell_shifts = 0usize;
    let mut total_axis_permutes = 0usize;
    let mut total_defined_names = 0usize;
    let mut total_undos = 0usize;
    let mut total_redos = 0usize;

    for seed_idx in 0..SEEDS {
        let seed = 0x3003_0000 + seed_idx * 0x7F4A + 53;
        let mut rng = Rng::new(seed);
        let mut trace = Trace::new("Invariant3:LateJoinTxOverlay", seed);
        let fix = build_rich_workbook(seed);
        let baseline = fix.wb;
        let mut live = baseline.clone();
        let mut undo = UndoStack::new();
        let mut serial = 0usize;

        for step in 0..STEPS {
            serial += 1;
            let (undo_depth, redo_depth) = undo.depth();

            // Choose among: AxisPermute, CellShift, DefinedNames, Undo, Redo.
            let choice = if step == 0 {
                // Always start with an AxisPermute across tables/layout/pivots/etc.
                0
            } else if step == 1 {
                1
            } else if step == 2 {
                2
            } else if undo_depth > 0 && rng.chance(22, 100) {
                3
            } else if redo_depth > 0 && rng.chance(20, 100) {
                4
            } else {
                rng.range_u32(0, 3)
            };

            let mut acted = false;
            match choice {
                // 0: AxisPermute (rows & cols across ALL subsystems including tables/filter/pivots/layout/drawings!)
                0 => {
                    let ax = if rng.bool() { Axis::Row } else { Axis::Col };
                    let (from, n, at) = if ax == Axis::Row {
                        let f = rng.range_u32(0, 22);
                        let n = rng.range_u32(1, 3);
                        let mut a = rng.range_u32(0, 23);
                        if a == f {
                            a = (f + 4) % 23;
                        }
                        (f, n, a)
                    } else {
                        let f = rng.range_u32(0, 9);
                        let n = rng.range_u32(1, 3);
                        let mut a = rng.range_u32(0, 10);
                        if a == f {
                            a = (f + 3) % 10;
                        }
                        (f, n, a)
                    };
                    let sheet_idx = if rng.chance(85, 100) { 0 } else { 1 };
                    let intent = TxIntent::from_axis_permute(sheet_idx, ax, from, n, at).unwrap();
                    if let Ok(commit) = WorkbookTx::execute(&mut live, intent, (sheet_idx, 0, 0)) {
                        if undo.push_tx(commit) > 0 {
                            total_axis_permutes += 1;
                            acted = true;
                            trace.record(
                                step,
                                format!("AxisPermute sheet={sheet_idx} {ax:?} {from}..+{n} -> {at}"),
                            );
                        }
                    }
                }
                // 1: CellShift (vertical in safe col-3 lane or horizontal in safe rows 1..5)
                1 => {
                    let vert = rng.bool();
                    let insert = rng.bool();
                    let (rect, dir) = if vert {
                        let r1 = rng.range_u32(0, 5);
                        let c1 = if rng.chance(80, 100) { 3 } else { rng.range_u32(1, 4) };
                        ((r1, c1, r1, c1), CellShiftDir::Vertical)
                    } else {
                        let r1 = rng.range_u32(1, 5);
                        let c1 = rng.range_u32(2, 4);
                        ((r1, c1, r1, c1), CellShiftDir::Horizontal)
                    };
                    let op = CellShift::new(rect, dir, insert);
                    if let Ok(commit) =
                        WorkbookTx::execute(&mut live, TxIntent::from_cell_shift(0, &op), (0, 0, 0))
                    {
                        if undo.push_tx(commit) > 0 {
                            total_cell_shifts += 1;
                            acted = true;
                            trace.record(
                                step,
                                format!("CellShift rect={rect:?} dir={dir:?} ins={insert}"),
                            );
                        }
                    }
                }
                // 2: DefinedNames (Define / Update / Remove)
                2 => {
                    let shared: Vec<usize> = live
                        .names
                        .iter()
                        .enumerate()
                        .filter(|(_, (n, _, _))| !n.starts_with("_sc_src_"))
                        .map(|(i, _)| i)
                        .collect();
                    let (desc, intent) = match rng.range_u32(0, 3) {
                        0 => {
                            let nm = format!("RoomN_{seed_idx}_{serial}");
                            (
                                format!("DefinedNames::Define {nm}"),
                                TxIntent::from_defined_names(DefinedNameIntent::Define {
                                    name: nm,
                                    scope: if rng.bool() { None } else { Some(0) },
                                    formula: "Data!$B$1:$B$4".into(),
                                }),
                            )
                        }
                        1 if !shared.is_empty() => {
                            let idx = shared[rng.range_usize(0, shared.len())];
                            let (n, sc, _) = live.names[idx].clone();
                            (
                                format!("DefinedNames::Update[{idx}] {n}"),
                                TxIntent::from_defined_names(DefinedNameIntent::Update {
                                    index: idx,
                                    name: n,
                                    scope: sc,
                                    formula: format!("Data!$C${}", rng.range_u32(1, 8)),
                                }),
                            )
                        }
                        _ if shared.len() > 1 => {
                            let idx = shared[rng.range_usize(0, shared.len())];
                            (
                                format!("DefinedNames::Remove[{idx}]"),
                                TxIntent::from_defined_names(DefinedNameIntent::Remove { index: idx }),
                            )
                        }
                        _ => {
                            let nm = format!("RoomN2_{seed_idx}_{serial}");
                            (
                                format!("DefinedNames::Define(fb) {nm}"),
                                TxIntent::from_defined_names(DefinedNameIntent::Define {
                                    name: nm,
                                    scope: None,
                                    formula: "42".into(),
                                }),
                            )
                        }
                    };
                    if let Ok(commit) = WorkbookTx::execute(&mut live, intent, (0, 0, 0)) {
                        if undo.push_tx(commit) > 0 {
                            total_defined_names += 1;
                            acted = true;
                            trace.record(step, desc);
                        }
                    }
                }
                // 3: Undo
                3 => {
                    if let Some(u) = undo.undo_workbook(&mut live, allow_all) {
                        assert_eq!(u.skipped, 0);
                        total_undos += 1;
                        acted = true;
                        trace.record(step, format!("Undo({})", u.label));
                    }
                }
                // 4: Redo
                _ => {
                    if let Some(r) = undo.redo_workbook(&mut live, allow_all) {
                        assert_eq!(r.skipped, 0);
                        total_redos += 1;
                        acted = true;
                        trace.record(step, format!("Redo({})", r.label));
                    }
                }
            }

            if !acted {
                continue;
            }
            assert_physical_bounds(&live, &trace, step, "live after step");

            // Capture TxOverlay, verify wire_valid + JSON wire round-trip!
            let overlay = TxOverlay::capture(&live);
            assert!(
                overlay.wire_valid(),
                "{}",
                trace.dump(step, "TxOverlay::capture produced !wire_valid() overlay")
            );
            let wire_json = serde_json::to_string(&overlay)
                .unwrap_or_else(|e| panic!("{}", trace.dump(step, &format!("overlay serialize: {e}"))));
            let overlay_rt: TxOverlay = serde_json::from_str(&wire_json)
                .unwrap_or_else(|e| panic!("{}", trace.dump(step, &format!("overlay deserialize: {e}"))));
            assert!(
                overlay_rt.wire_valid(),
                "{}",
                trace.dump(step, "deserialized TxOverlay failed wire_valid()")
            );

            // Late joiner starts from ORIGINAL untouched baseline (before any AxisPermute/CellShift),
            // installs authoritative cell values/formulas, and applies TxOverlay!
            let mut late_joiner = baseline.clone();
            sync_authoritative_cells(&live, &mut late_joiner);
            assert!(
                overlay_rt.apply_to(&mut late_joiner),
                "{}",
                trace.dump(step, "TxOverlay::apply_to returned false on cloned baseline")
            );

            // Verify image bytes on late_joiner were NOT dropped or corrupted by geometry-only sync!
            assert_eq!(
                late_joiner.sheets[0].pictures[0].data.as_deref(),
                baseline.sheets[0].pictures[0].data.as_deref(),
                "{}",
                trace.dump(step, "TxOverlay clobbered Picture::data bytes on late joiner")
            );

            // Full BusinessState comparison between online live workbook and late joiner!
            assert_business_state_eq(
                &BusinessState::capture(&live),
                &BusinessState::capture(&late_joiner),
                &trace,
                step,
                "online live vs late_joiner after TxOverlay::apply_to",
            );
        }
    }

    assert!(total_axis_permutes >= 40, "AxisPermute under-exercised: {total_axis_permutes}");
    assert!(total_cell_shifts >= 10, "CellShift under-exercised: {total_cell_shifts}");
    assert!(total_defined_names >= 20, "DefinedNames under-exercised: {total_defined_names}");
    assert!(total_undos >= 15, "Undo under-exercised: {total_undos}");
    assert!(total_redos >= 3, "Redo under-exercised: {total_redos}");
}

#[test]
fn invariant_3_tx_overlay_legacy_json_backwards_compatibility() {
    // A legacy peer JSON overlay containing ONLY the original pre-Phase-6 fields
    // MUST still deserialize cleanly, pass wire_valid(), and apply cleanly.
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "S1".into();
    wb.sheets[0].set(0, 0, num(10.0));
    let overlay = TxOverlay::capture(&wb);
    let mut val = serde_json::to_value(&overlay).unwrap();

    // Strip all optional Phase-5/Phase-6 fields from top-level and sheet objects.
    let top = val.as_object_mut().unwrap();
    top.retain(|k, _| matches!(k.as_str(), "sheets" | "palette" | "sources" | "names"));
    for sh in top["sheets"].as_array_mut().unwrap() {
        sh.as_object_mut().unwrap().retain(|k, _| {
            matches!(
                k.as_str(),
                "name" | "styles" | "notes" | "threads" | "rich" | "links" | "merges" | "cf" | "dv"
            )
        });
    }

    let decoded: TxOverlay = serde_json::from_value(val)
        .expect("legacy TxOverlay JSON without Phase 6 fields must deserialize");
    assert!(decoded.wire_valid(), "legacy TxOverlay must pass wire_valid()");
    let mut target = wb.clone();
    assert!(decoded.apply_to(&mut target), "legacy TxOverlay must apply cleanly");
}

// ============================================================================
// Invariant 4 — Physical vs Reference Coordinate Boundary Consistency
// ============================================================================

#[test]
fn invariant_4_physical_vs_reference_coordinate_boundary_consistency() {
    const SEEDS: u64 = 16;
    const STEPS: usize = 30;

    let mut overflow_rejects = 0usize;
    let mut valid_transforms = 0usize;

    for seed_idx in 0..SEEDS {
        let seed = 0x4004_0000 + seed_idx * 0x61C8 + 71;
        let mut rng = Rng::new(seed);
        let mut trace = Trace::new("Invariant4:BoundaryConsistency", seed);

        let mut wb = Workbook::new_blank();
        wb.sheets[0].name = "Edge".into();
        let mut other = Sheet::new("Watcher");
        other.set(0, 0, fml("=Edge!A1048576"));
        other.set(1, 0, fml("=Edge!XFD1"));
        wb.sheets.push(other);

        let st_hi = wb.styles.intern(CellStyle {
            bold: true,
            fill: Some([0xFF, 0xDD, 0xCC]),
            ..CellStyle::default_dark()
        });

        // Full-axis CF and DV rules MUST NOT falsely block inserts when no tail cells overflow!
        wb.sheets[0].cf.blocks.push(CfBlock {
            ranges: vec![(0, 0, MAX_ROWS - 1, 0), (0, 0, 0, MAX_COLS - 1)],
            rules: vec![CfRule {
                kind: CfKind::CellIs {
                    op: CfOp::Greater,
                    a: "0".into(),
                    b: String::new(),
                    dxf: 0,
                },
                stop: false,
                priority: 1,
            }],
        });
        wb.sheets[0].dv.push(DataValidation {
            ranges: vec![(0, 1, MAX_ROWS - 1, 1)],
            kind: DvKind::Whole,
            op: DvOp::GreaterEq,
            f1: "0".into(),
            f2: String::new(),
            allow_blank: true,
            show_dropdown: true,
            error_style: DvStyle::Stop,
            show_error: true,
            show_prompt: false,
            error_title: String::new(),
            error_msg: String::new(),
            prompt_title: String::new(),
            prompt_msg: String::new(),
        });

        // Verify full-axis CF/DV alone does NOT falsely reject whole-row / whole-col insert!
        {
            let before_cf_dv = BusinessState::capture(&wb);
            let mut undo_probe = UndoStack::new();
            let c1 = WorkbookTx::execute(
                &mut wb,
                TxIntent::from_structural(0, &Structural::InsertRows { at: 0, n: 1 }),
                (0, 0, 0),
            )
            .expect("full-axis CF/DV must NOT falsely reject whole-row insert");
            assert!(undo_probe.push_tx(c1) > 0);
            assert_physical_bounds(&wb, &trace, 0, "after full-axis CF/DV row insert");
            let c2 = WorkbookTx::execute(
                &mut wb,
                TxIntent::from_structural(0, &Structural::InsertCols { at: 0, n: 1 }),
                (0, 0, 0),
            )
            .expect("full-axis CF/DV must NOT falsely reject whole-col insert");
            assert!(undo_probe.push_tx(c2) > 0);
            assert_physical_bounds(&wb, &trace, 0, "after full-axis CF/DV col insert");
            undo_probe.undo_workbook(&mut wb, allow_all).unwrap();
            undo_probe.undo_workbook(&mut wb, allow_all).unwrap();
            assert_business_state_eq(&before_cf_dv, &BusinessState::capture(&wb), &trace, 0, "cf/dv probe undo");
        }

        // Populate diverse tail-edge items near MAX_ROWS - 1 and MAX_COLS - 1.
        let r_near = MAX_ROWS - 5;
        let c_near = MAX_COLS - 5;
        match seed_idx % 5 {
            0 => {
                wb.sheets[0].set(MAX_ROWS - 1, 0, num(101.0));
                wb.sheets[0].set(0, MAX_COLS - 1, num(202.0));
                wb.sheets[0].set(r_near, 2, txt("NearBottom"));
                wb.sheets[0].set(2, c_near, txt("NearRight"));
            }
            1 => {
                wb.sheets[0].set_style(MAX_ROWS - 1, 0, st_hi);
                wb.sheets[0].set_style(0, MAX_COLS - 1, st_hi);
                wb.sheets[0].set_row_style(MAX_ROWS - 2, st_hi);
                wb.sheets[0].set_col_style(MAX_COLS - 2, st_hi);
            }
            2 => {
                wb.sheets[0].tables.push(Table {
                    name: "TailTable".into(),
                    display: "TailTable".into(),
                    range: (MAX_ROWS - 3, 2, MAX_ROWS - 1, 4),
                    columns: vec!["A".into(), "B".into(), "C".into()],
                    column_ids: vec![1, 2, 3],
                    ..Table::default()
                });
                wb.sheets[0].merge_range(4, MAX_COLS - 2, 5, MAX_COLS - 1);
            }
            3 => {
                wb.sheets[0].notes.insert(
                    CellRef::new(MAX_ROWS - 1, 3),
                    Note {
                        r: MAX_ROWS - 1,
                        c: 3,
                        author: "Edge".into(),
                        text: "TailNote".into(),
                    },
                );
                wb.sheets[0].sparklines.push(Sparkline {
                    r: 3,
                    c: MAX_COLS - 1,
                    source_sheet: None,
                    rect: (0, 0, 2, 0),
                    ..Sparkline::default()
                });
            }
            _ => {
                wb.sheets[0].merge_range(MAX_ROWS - 2, 0, MAX_ROWS - 1, 1);
                wb.sheets[0].notes.insert(
                    CellRef::new(1, MAX_COLS - 1),
                    Note {
                        r: 1,
                        c: MAX_COLS - 1,
                        author: "Edge".into(),
                        text: "RightNote".into(),
                    },
                );
                wb.sheets[0].rows.set(MAX_ROWS - 2, 35.0);
                wb.sheets[0].cols.set(MAX_COLS - 2, 110.0);
            }
        }

        // Also place tracked sentinel cells near both top-left and tail so we verify
        // `CoordTransform::map_index` agrees with physical persistence on EVERY step!
        wb.sheets[0].set(10, 5, txt("SENTINEL_MID"));
        wb.sheets[0].set(MAX_ROWS - 6, 5, txt("SENTINEL_BOT"));
        wb.sheets[0].set(5, MAX_COLS - 6, txt("SENTINEL_RIGHT"));

        let mut undo = UndoStack::new();

        for step in 0..STEPS {
            let (desc, tx_opt, intent): (String, Option<CoordTransform>, TxIntent) =
                match rng.range_u32(0, 10) {
                    // Whole-axis InsertRows near top or near bottom
                    0 => {
                        let at = if rng.bool() {
                            rng.range_u32(0, 5)
                        } else {
                            MAX_ROWS - rng.range_u32(2, 10)
                        };
                        let n = rng.range_u32(1, 5);
                        let tx = CoordTransform::from_structural(0, &Structural::InsertRows { at, n });
                        (format!("InsertRows at={at} n={n}"), Some(tx.clone()), TxIntent::Transform(tx))
                    }
                    // Whole-axis InsertCols near left or near right
                    1 => {
                        let at = if rng.bool() {
                            rng.range_u32(0, 5)
                        } else {
                            MAX_COLS - rng.range_u32(2, 10)
                        };
                        let n = rng.range_u32(1, 5);
                        let tx = CoordTransform::from_structural(0, &Structural::InsertCols { at, n });
                        (format!("InsertCols at={at} n={n}"), Some(tx.clone()), TxIntent::Transform(tx))
                    }
                    // Whole-axis DeleteRows near bottom (creates room so later inserts can succeed!)
                    2 => {
                        let at = MAX_ROWS - rng.range_u32(1, 10);
                        let n = rng.range_u32(1, 3).min(MAX_ROWS - at);
                        let tx = CoordTransform::from_structural(0, &Structural::DeleteRows { at, n });
                        (format!("DeleteRows at={at} n={n}"), Some(tx.clone()), TxIntent::Transform(tx))
                    }
                    // Whole-axis DeleteCols near right
                    3 => {
                        let at = MAX_COLS - rng.range_u32(1, 10);
                        let n = rng.range_u32(1, 3).min(MAX_COLS - at);
                        let tx = CoordTransform::from_structural(0, &Structural::DeleteCols { at, n });
                        (format!("DeleteCols at={at} n={n}"), Some(tx.clone()), TxIntent::Transform(tx))
                    }
                    // MultiInsert near bottom or right
                    4 => {
                        let ax = if rng.bool() { Axis::Row } else { Axis::Col };
                        let lim = if ax == Axis::Row { MAX_ROWS } else { MAX_COLS };
                        let p1 = lim - rng.range_u32(6, 12);
                        let p2 = lim - rng.range_u32(1, 6);
                        let tx = CoordTransform::from_multi_insert(0, ax, vec![p1, p2]);
                        (format!("MultiInsert({ax:?}) [{p1},{p2}]"), Some(tx.clone()), TxIntent::Transform(tx))
                    }
                    // Partial CellShift near tail
                    5 => {
                        let vert = rng.bool();
                        let insert = rng.bool();
                        let (rect, dir) = if vert {
                            let r = MAX_ROWS - rng.range_u32(2, 8);
                            let c = rng.range_u32(0, 6);
                            ((r, c, r, c), CellShiftDir::Vertical)
                        } else {
                            let r = rng.range_u32(0, 6);
                            let c = MAX_COLS - rng.range_u32(2, 8);
                            ((r, c, r, c), CellShiftDir::Horizontal)
                        };
                        let op = CellShift::new(rect, dir, insert);
                        let tx = CoordTransform::from_cell_shift(0, &op);
                        (
                            format!("CellShift rect={rect:?} dir={dir:?} ins={insert}"),
                            Some(tx.clone()),
                            TxIntent::Transform(tx),
                        )
                    }
                    // AxisPermute near tail
                    6 => {
                        let ax = if rng.bool() { Axis::Row } else { Axis::Col };
                        let lim = if ax == Axis::Row { MAX_ROWS } else { MAX_COLS };
                        let from = lim - rng.range_u32(3, 8);
                        let n = rng.range_u32(1, 3);
                        let mut at = lim - rng.range_u32(2, 8);
                        if at == from || at + n > lim {
                            at = lim - n;
                        }
                        if from == at {
                            at = from - 1;
                        }
                        let tx = CoordTransform::from_axis_permute(0, ax, from, n, at).unwrap();
                        (
                            format!("AxisPermute({ax:?}) {from}..+{n} -> {at}"),
                            Some(tx.clone()),
                            TxIntent::Transform(tx),
                        )
                    }
                    // Write a fresh cell right at MAX_ROWS - 1 or MAX_COLS - 1
                    7 => {
                        let w = if rng.bool() {
                            CellWrite::cell(MAX_ROWS - 1, rng.range_u32(0, 6), num(step as f64))
                        } else {
                            CellWrite::cell(rng.range_u32(0, 6), MAX_COLS - 1, num(step as f64))
                        };
                        (
                            format!("CellBatch tail ({},{})", w.row, w.col),
                            None,
                            TxIntent::from_cell_batch(0, vec![w]),
                        )
                    }
                    // Undo / Redo
                    8 if undo.depth().0 > 0 => {
                        let u = undo.undo_workbook(&mut wb, allow_all).unwrap();
                        trace.record(step, format!("Undo({})", u.label));
                        assert_physical_bounds(&wb, &trace, step, "after undo");
                        continue;
                    }
                    _ if undo.depth().1 > 0 => {
                        let r = undo.redo_workbook(&mut wb, allow_all).unwrap();
                        trace.record(step, format!("Redo({})", r.label));
                        assert_physical_bounds(&wb, &trace, step, "after redo");
                        continue;
                    }
                    _ => {
                        let tx = CoordTransform::from_structural(0, &Structural::InsertRows { at: 0, n: 1 });
                        ("InsertRows(fallback) at=0 n=1".into(), Some(tx.clone()), TxIntent::Transform(tx))
                    }
                };

            // Snapshot exact pre-step cell map on target sheet so we can cross-check
            // every single cell's physical movement against `CoordTransform::map_cell` / `map_index`!
            let before_cells: BTreeMap<(u32, u32), CellInput> = wb.sheets[0]
                .iter_views()
                .filter_map(|(k, v)| {
                    if let Some(f) = v.formula() {
                        Some(((k.row, k.col), CellInput::Formula(f.to_string())))
                    } else if !v.value().is_empty() {
                        Some((
                            (k.row, k.col),
                            CellInput::Value(CanonicalValue::from_value(v.value().as_ref())),
                        ))
                    } else {
                        None
                    }
                })
                .collect();
            let before_state = BusinessState::capture(&wb);
            let before_epoch = wb.coord_epoch();

            match WorkbookTx::execute(&mut wb, intent, (0, 0, 0)) {
                Ok(commit) => {
                    trace.record(step, format!("{desc} -> Ok"));
                    let _ = undo.push_tx(commit);
                    assert_physical_bounds(&wb, &trace, step, &format!("after {desc}"));

                    // Verify `CoordTransform::map_index` / `map_cell` agrees with physical cell persistence!
                    if let Some(tx) = tx_opt {
                        valid_transforms += 1;
                        let is_insert = matches!(
                            tx,
                            CoordTransform::AxisShift { delete: false, .. }
                                | CoordTransform::MultiInsert { .. }
                        );
                        for (&(r, c), input) in &before_cells {
                            let mapped = tx.map_cell(r, c);
                            // Also cross-check map_cell against map_index along tx.axis()!
                            let in_band = match tx.band() {
                                None => true,
                                Some((lo, hi)) => {
                                    let cross = match tx.axis() {
                                        Axis::Row => c,
                                        Axis::Col => r,
                                    };
                                    cross >= lo && cross <= hi
                                }
                            };
                            let via_index = if !in_band {
                                Some((r, c))
                            } else {
                                match tx.axis() {
                                    Axis::Row => tx.map_index(r).map(|nr| (nr, c)),
                                    Axis::Col => tx.map_index(c).map(|nc| (r, nc)),
                                }
                            };
                            assert_eq!(
                                mapped, via_index,
                                "{}",
                                trace.dump(step, &format!("map_cell vs map_index mismatch at ({r},{c})"))
                            );

                            match mapped {
                                Some((nr, nc)) => {
                                    assert!(nr < MAX_ROWS && nc < MAX_COLS);
                                    let after_view = wb.sheets[0].get_view(nr, nc);
                                    assert!(
                                        after_view.is_some(),
                                        "{}",
                                        trace.dump(
                                            step,
                                            &format!(
                                                "Physical persistence mismatch: ({r},{c})={input:?} mapped via map_index to ({nr},{nc}), but cell is MISSING after {desc}"
                                            )
                                        )
                                    );
                                    if let CellInput::Value(ref expected_v) = *input {
                                        let actual_v =
                                            CanonicalValue::from_value(after_view.unwrap().value().as_ref());
                                        assert_eq!(
                                            &actual_v,
                                            expected_v,
                                            "{}",
                                            trace.dump(
                                                step,
                                                &format!(
                                                    "Physical value mismatch at mapped ({nr},{nc}) from ({r},{c}) after {desc}"
                                                )
                                            )
                                        );
                                    }
                                }
                                None => {
                                    // An accepted INSERT must NEVER drop a live cell past MAX_ROWS/MAX_COLS!
                                    assert!(
                                        !is_insert,
                                        "{}",
                                        trace.dump(
                                            step,
                                            &format!(
                                                "Accepted insert {desc} silently dropped live cell at ({r},{c}) past grid boundary!"
                                            )
                                        )
                                    );
                                }
                            }
                        }
                    }
                }
                Err(err) => {
                    trace.record(step, format!("{desc} -> Err({err:?})"));
                    if matches!(err, TxError::Conflict(CellShiftConflict::Overflow)) {
                        overflow_rejects += 1;
                    }
                    assert_business_state_eq(
                        &before_state,
                        &BusinessState::capture(&wb),
                        &trace,
                        step,
                        &format!("rejected boundary op {desc} mutated state"),
                    );
                    assert_eq!(wb.coord_epoch(), before_epoch);
                    assert_physical_bounds(&wb, &trace, step, &format!("after rejected {desc}"));
                }
            }
        }
    }

    assert!(
        overflow_rejects >= 20,
        "boundary test did not exercise enough overflow rejections: {overflow_rejects}"
    );
    assert!(
        valid_transforms >= 20,
        "boundary test did not exercise enough accepted transforms: {valid_transforms}"
    );
}

// ============================================================================
// Invariant 5 — `CoordParticipant` Lifecycle Matrix Completeness
// ============================================================================

#[test]
fn invariant_5_coord_participant_lifecycle_matrix_completeness() {
    assert_eq!(CoordParticipant::ALL.len(), 16);
    let unique: HashSet<CoordParticipant> = CoordParticipant::ALL.iter().copied().collect();
    assert_eq!(unique.len(), 16, "CoordParticipant::ALL must contain 16 unique variants");

    // 1. Static lifecycle matrix contract check over EVERY participant.
    for p in CoordParticipant::ALL {
        assert_eq!(
            p.mutated_by_cell_shift(),
            p.cell_shift_mode() == CellShiftMode::Transform,
            "mutated_by_cell_shift() must match CellShiftMode::Transform for {p:?}"
        );
        if p.mutated_by_axis_permute() || p.mutated_by_cell_shift() {
            assert!(
                p.captured_in_tx_overlay(),
                "CoordParticipant::{p:?} is mutated by AxisPermute/CellShift but captured_in_tx_overlay() == false!"
            );
        }
    }

    // 2. Dynamic end-to-end TxOverlay round-trip verification for EVERY SINGLE participant!
    // Exhaustive match (no `_` wildcard) ensures adding a new participant forces updating this suite.
    for participant in CoordParticipant::ALL {
        let mut baseline = Workbook::new_blank();
        baseline.sheets[0].name = "Data".into();
        let data_id = baseline.sheets[0].identity;
        baseline.sheets.push(Sheet::new("Report"));

        let st1 = baseline.styles.intern(CellStyle {
            bold: true,
            fill: Some([0xDD, 0xEE, 0xFF]),
            ..CellStyle::default_dark()
        });

        // Build targeted fixture + mutation specifically exercising `participant`.
        let (intent, expects_diff_without_overlay): (TxIntent, bool) = match participant {
            CoordParticipant::CellsAndStyles => {
                baseline.sheets[0].set(0, 0, num(10.0));
                baseline.sheets[0].set_style(0, 0, st1);
                baseline.sheets[0].set_row_style(0, st1);
                (TxIntent::from_axis_permute(0, Axis::Row, 0, 1, 2).unwrap(), true)
            }
            CoordParticipant::CellNotesAndThreads => {
                baseline.sheets[0].notes.insert(
                    CellRef::new(0, 0),
                    Note {
                        r: 0,
                        c: 0,
                        author: "A".into(),
                        text: "N1".into(),
                    },
                );
                baseline.sheets[0].threads.insert(
                    CellRef::new(1, 0),
                    ThreadedComment {
                        r: 1,
                        c: 0,
                        resolved: false,
                        entries: vec![ThreadEntry {
                            id: "t1".into(),
                            author: "A".into(),
                            timestamp: "2026-11-27".into(),
                            text: "Hi".into(),
                        }],
                    },
                );
                (TxIntent::from_axis_permute(0, Axis::Row, 0, 2, 3).unwrap(), true)
            }
            CoordParticipant::RichAndHyperlinks => {
                let (full, runs) = make_rich_runs("Hi ", "bold", [0xAA, 0x00, 0x00]);
                baseline.sheets[0].set(0, 0, txt(&full));
                baseline.sheets[0].rich.insert((0, 0), runs);
                baseline.sheets[0].hyperlinks.insert(
                    (0, 1),
                    Hyperlink {
                        location: "Data!A1".into(),
                        ..Default::default()
                    },
                );
                (TxIntent::from_axis_permute(0, Axis::Row, 0, 1, 3).unwrap(), true)
            }
            CoordParticipant::Merges => {
                baseline.sheets[0].merge_range(0, 0, 1, 1);
                (TxIntent::from_axis_permute(0, Axis::Row, 0, 2, 3).unwrap(), true)
            }
            CoordParticipant::ConditionalAndValidation => {
                baseline.sheets[0].cf.blocks.push(CfBlock {
                    ranges: vec![(0, 0, 1, 0)],
                    rules: vec![CfRule {
                        kind: CfKind::CellIs {
                            op: CfOp::Greater,
                            a: "5".into(),
                            b: String::new(),
                            dxf: 0,
                        },
                        stop: false,
                        priority: 1,
                    }],
                });
                baseline.sheets[0].dv.push(DataValidation {
                    ranges: vec![(0, 0, 1, 0)],
                    kind: DvKind::Whole,
                    op: DvOp::Greater,
                    f1: "0".into(),
                    f2: String::new(),
                    allow_blank: true,
                    show_dropdown: true,
                    error_style: DvStyle::Stop,
                    show_error: true,
                    show_prompt: false,
                    error_title: String::new(),
                    error_msg: String::new(),
                    prompt_title: String::new(),
                    prompt_msg: String::new(),
                });
                (TxIntent::from_axis_permute(0, Axis::Row, 0, 2, 3).unwrap(), true)
            }
            CoordParticipant::Charts => {
                baseline.sheets[0].charts.push(Chart {
                    kind: ChartKind::Column,
                    part: "xl/charts/chart1.xml".into(),
                    anchor: (0, 3, 1, 6),
                    source_range: "Data!$A$1:$A$2".into(),
                    series: vec![Series {
                        val_ref: "Data!$A$1:$A$2".into(),
                        ..Series::default()
                    }],
                    ..Chart::default()
                });
                (TxIntent::from_axis_permute(0, Axis::Row, 0, 2, 4).unwrap(), true)
            }
            CoordParticipant::Sparklines => {
                baseline.sheets[0].sparklines.push(Sparkline {
                    r: 0,
                    c: 2,
                    source_sheet: None,
                    rect: (0, 0, 1, 0),
                    ..Sparkline::default()
                });
                (TxIntent::from_axis_permute(0, Axis::Row, 0, 2, 4).unwrap(), true)
            }
            CoordParticipant::DefinedNames => {
                baseline
                    .names
                    .push(("Metric".into(), None, "Data!$A$1:$A$2".into()));
                (TxIntent::from_axis_permute(0, Axis::Row, 0, 2, 4).unwrap(), true)
            }
            CoordParticipant::ExternalSources => {
                baseline.sources.push(Source {
                    name: "Src".into(),
                    kind: SourceKind::Internal {
                        sheet: "Data".into(),
                        rect: (0, 0, 1, 1),
                    },
                    at: (0, 0),
                    target_sheet: Some("Report".into()),
                    rows: 2,
                    cols: 2,
                    csv_opts: None,
                });
                (TxIntent::from_axis_permute(0, Axis::Row, 0, 2, 4).unwrap(), true)
            }
            CoordParticipant::IgnoredErrors => {
                baseline.sheets[0].ignored_errors.insert((0, 0));
                (TxIntent::from_axis_permute(0, Axis::Row, 0, 1, 3).unwrap(), true)
            }
            CoordParticipant::Tables => {
                baseline.sheets[0].tables.push(Table {
                    name: "Table1".into(),
                    display: "Table1".into(),
                    range: (0, 0, 1, 1),
                    columns: vec!["A".into(), "B".into()],
                    column_ids: vec![1, 2],
                    ..Table::default()
                });
                (TxIntent::from_axis_permute(0, Axis::Row, 0, 2, 3).unwrap(), true)
            }
            CoordParticipant::Pivots => {
                let spec2 = PivotSpec2 {
                    source: (0, 0, 1, 1),
                    rows: vec![AxisField::plain(0)],
                    cols: Vec::new(),
                    values: vec![ValueField {
                        col: 1,
                        agg: Agg::Sum,
                        label: None,
                        show_as: ShowAs::Normal,
                        base: None,
                    }],
                    filters: Vec::new(),
                    layout: PivotLayout::default(),
                };
                let mut piv = PivotTableModel::new(
                    "P1".into(),
                    "Data".into(),
                    Some(data_id),
                    1,
                    (2, 0),
                    spec2.clone(),
                );
                piv.rect = (2, 0, 4, 1);
                piv.last_meta.anchor = (2, 0);
                piv.last_meta.rect = (2, 0, 4, 1);
                baseline.pivots.push(piv);
                baseline.pivots_pending.push((
                    0,
                    PivotSpec {
                        source: (0, 0, 1, 1),
                        row_field: 0,
                        col_field: None,
                        value_field: 1,
                        agg: Agg::Sum,
                        group: GroupBy::None,
                        exclude: Vec::new(),
                    },
                    PivotResult::default(),
                    (0, 5, 1, 6),
                    "V".into(),
                ));
                baseline
                    .pivots_pending2
                    .push((0, spec2, PivotGrid::default(), (0, 8, 1, 9)));
                (TxIntent::from_axis_permute(0, Axis::Row, 0, 2, 4).unwrap(), true)
            }
            CoordParticipant::ArraysAndSpills => {
                baseline.sheets[0].set(0, 0, fml("=SEQUENCE(2,1)"));
                baseline.sheets[0].set(1, 0, num(2.0));
                baseline.sheets[0].arrays.push((CellRef::new(0, 0), (0, 0, 1, 0)));
                baseline.sheets[0].spilled.insert(CellRef::new(1, 0));
                baseline.sheets[0].spill_shapes.insert(CellRef::new(0, 0), (2, 1));
                (TxIntent::from_axis_permute(0, Axis::Row, 0, 2, 4).unwrap(), true)
            }
            CoordParticipant::AutoFilter => {
                let mut flt = Filter::new(0, 0, 1, 1);
                flt.criteria.insert(0, Criterion::new(CmpOp::Gt, "5"));
                baseline.sheets[0].filter = Some(flt);
                (TxIntent::from_axis_permute(0, Axis::Row, 0, 2, 4).unwrap(), true)
            }
            CoordParticipant::ScenariosAndProtectedRanges => {
                let mut sc = Scenario::new("S1");
                sc.cells = vec![((0, 0), "42".into())];
                baseline.sheets[0].scenarios.push(sc);
                baseline.sheets[0].scenario_settings.sqref = Some("A1:A2".into());
                baseline.sheets[0]
                    .protected_ranges
                    .push(ProtectedRange::new("PR1", vec![(0, 0, 1, 1)], ""));
                (TxIntent::from_axis_permute(0, Axis::Row, 0, 2, 4).unwrap(), true)
            }
            CoordParticipant::LayoutAndPage => {
                baseline.sheets[0].rows.set(0, 60.0);
                baseline.sheets[0].hidden_rows.insert(1);
                baseline.sheets[0].hidden_rows_user.insert(0);
                baseline.sheets[0].row_levels.insert(0, 2);
                baseline.sheets[0].freeze = Some((2, 1));
                baseline.sheets[0].page.print_area = Some((0, 0, 1, 2));
                baseline.sheets[0].page.title_rows = Some((0, 1));
                baseline.sheets[0].page.row_breaks = vec![2];
                baseline.sheets[0].pictures.push(Picture {
                    rect: (0, 3, 1, 5),
                    part: "xl/media/p1.png".into(),
                    data: Some(Arc::new(vec![1, 2, 3])),
                    ..Picture::default()
                });
                baseline.sheets[0].textboxes.push(TextBox {
                    id: 9,
                    from: (3, 0, 0, 0),
                    to: (5, 0, 1, 0),
                    text: "Box".into(),
                    ..TextBox::default()
                });
                (TxIntent::from_axis_permute(0, Axis::Row, 0, 2, 4).unwrap(), true)
            }
        };

        let trace = Trace::new("Invariant5:ParticipantOverlayRoundtrip", participant as u64);
        let before_state = BusinessState::capture(&baseline);

        let mut live = baseline.clone();
        WorkbookTx::execute(&mut live, intent, (0, 0, 0))
            .unwrap_or_else(|e| panic!("participant {participant:?} mutation failed: {e:?}"));
        let live_state = BusinessState::capture(&live);
        assert_ne!(
            live_state, before_state,
            "participant {participant:?} test fixture was not mutated!"
        );

        let mut late_joiner = baseline.clone();
        sync_authoritative_cells(&live, &mut late_joiner);
        if expects_diff_without_overlay {
            assert_ne!(
                BusinessState::capture(&late_joiner),
                live_state,
                "participant {participant:?} late_joiner already matched live without TxOverlay (vacuous test!)"
            );
        }

        let overlay = TxOverlay::capture(&live);
        assert!(
            overlay.wire_valid(),
            "participant {participant:?} overlay failed wire_valid()"
        );
        let json = serde_json::to_string(&overlay).unwrap();
        let overlay_rt: TxOverlay = serde_json::from_str(&json).unwrap();
        assert!(overlay_rt.wire_valid());
        assert!(
            overlay_rt.apply_to(&mut late_joiner),
            "participant {participant:?} overlay apply_to failed"
        );

        assert_business_state_eq(
            &live_state,
            &BusinessState::capture(&late_joiner),
            &trace,
            0,
            &format!("participant {participant:?} TxOverlay roundtrip"),
        );
    }
}
