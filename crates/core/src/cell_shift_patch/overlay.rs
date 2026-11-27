use super::*;
use crate::cf::CfSheet;
use crate::layout::{Axis as LayoutAxis, DEFAULT_COL_W, DEFAULT_ROW_H, HiddenIndices};
use crate::page::PageSetup;
use crate::protection::ProtectedRange;
use crate::sort_filter::{ColorFilter, Criterion, DateGroupItem, DynamicFilter, Filter, TopFilter};
use crate::table::Table;

pub const CURRENT_OVERLAY_FORMAT_VERSION: u8 = 1;
const MAX_AXIS_SIZE: f32 = 100_000.0;

fn is_zero_u8(v: &u8) -> bool {
    *v == 0
}

fn default_row_axis() -> OverlayAxis {
    OverlayAxis {
        default: DEFAULT_ROW_H,
        spans: Vec::new(),
    }
}

fn default_col_axis() -> OverlayAxis {
    OverlayAxis {
        default: DEFAULT_COL_W,
        spans: Vec::new(),
    }
}

fn is_default_row_axis(ax: &OverlayAxis) -> bool {
    ax.spans.is_empty() && ax.default.to_bits() == DEFAULT_ROW_H.to_bits()
}

fn is_default_col_axis(ax: &OverlayAxis) -> bool {
    ax.spans.is_empty() && ax.default.to_bits() == DEFAULT_COL_W.to_bits()
}

fn is_default_page(p: &PageSetup) -> bool {
    *p == PageSetup::default()
}

fn is_default_scenario_settings(s: &OverlayScenarioSettings) -> bool {
    s.current.is_none() && s.show.is_none() && s.sqref.is_none()
}

fn valid_intervals(intervals: &[(u32, u32)], limit: u32) -> bool {
    let mut prev_hi: Option<u32> = None;
    for &(lo, hi) in intervals {
        if lo > hi || hi >= limit {
            return false;
        }
        if let Some(p) = prev_hi
            && lo <= p
        {
            return false;
        }
        prev_hi = Some(hi);
    }
    true
}

fn valid_page_setup(p: &PageSetup) -> bool {
    let max_text = crate::sync::MAX_THREAD_ENTRY_TEXT_BYTES;
    p.margins
        .iter()
        .all(|m| m.is_finite() && (0.0..=100.0).contains(m))
        && p.watermark.font_size.is_finite()
        && p.watermark.angle_deg.is_finite()
        && p.watermark.opacity.is_finite()
        && p.header.len() <= max_text
        && p.footer.len() <= max_text
        && p.first_header.len() <= max_text
        && p.first_footer.len() <= max_text
        && p.even_header.len() <= max_text
        && p.even_footer.len() <= max_text
        && p.watermark.text.len() <= max_text
        && p.print_area.as_ref().is_none_or(valid_rect)
        && p.title_rows
            .is_none_or(|(a, b)| a <= b && b < crate::sync::MAX_ROWS)
        && p.title_cols
            .is_none_or(|(a, b)| a <= b && b < crate::sync::MAX_COLS)
        && p.row_breaks.iter().all(|&r| r < crate::sync::MAX_ROWS)
        && p.col_breaks.iter().all(|&c| c < crate::sync::MAX_COLS)
}

fn valid_table(t: &Table) -> bool {
    let max_short = crate::sync::MAX_SHORT_TEXT;
    let max_cols = crate::sync::MAX_COLS as usize;
    valid_rect(&t.range)
        && t.name.len() <= max_short
        && t.display.len() <= max_short
        && t.style_name.len() <= max_short
        && t.columns.len() <= max_cols
        && t.column_ids.len() <= max_cols
        && t.totals_fns.len() <= max_cols
        && t.columns.iter().all(|c| c.len() <= max_short)
        && t.totals_fns.iter().all(|f| f.len() <= max_short)
}

fn valid_protected_range(pr: &ProtectedRange) -> bool {
    let max_short = crate::sync::MAX_SHORT_TEXT;
    pr.title.len() <= max_short
        && pr.algorithm.len() <= max_short
        && pr.hash.len() <= max_short
        && pr.salt.len() <= max_short
        && pr.sqref.iter().all(valid_rect)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct OverlayAxis {
    default: f32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    spans: Vec<(u32, u32, f32)>,
}

impl OverlayAxis {
    fn capture(ax: &LayoutAxis) -> Self {
        Self {
            default: ax.default_size(),
            spans: ax.spans().iter().map(|s| (s.start, s.end, s.size)).collect(),
        }
    }

    fn valid(&self, limit: u32) -> bool {
        if !self.default.is_finite() || !(0.0..=MAX_AXIS_SIZE).contains(&self.default) {
            return false;
        }
        let mut prev_hi: Option<u32> = None;
        for &(lo, hi, sz) in &self.spans {
            if lo > hi
                || hi >= limit
                || !sz.is_finite()
                || !(0.0..=MAX_AXIS_SIZE).contains(&sz)
            {
                return false;
            }
            if let Some(p) = prev_hi
                && lo <= p
            {
                return false;
            }
            prev_hi = Some(hi);
        }
        true
    }

    fn to_axis(&self) -> LayoutAxis {
        LayoutAxis::from_default_and_spans(self.default, self.spans.clone())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct PictureGeometry {
    part: String,
    rect: Rect,
    off: [i64; 4],
}

impl PictureGeometry {
    fn read(pic: &crate::pic::Picture) -> Self {
        Self {
            part: pic.part.clone(),
            rect: pic.rect,
            off: pic.off,
        }
    }

    fn valid(&self) -> bool {
        self.part.len() <= crate::sync::MAX_SHORT_TEXT && valid_rect(&self.rect)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct TextBoxGeometry {
    id: u32,
    from: (u32, i64, u32, i64),
    to: (u32, i64, u32, i64),
}

impl TextBoxGeometry {
    fn read(tb: &crate::textbox::TextBox) -> Self {
        Self {
            id: tb.id,
            from: tb.from,
            to: tb.to,
        }
    }

    fn valid(&self) -> bool {
        // TextBox anchors store `(col, col_off_emu, row, row_off_emu)`.
        valid_point(self.from.2, self.from.0) && valid_point(self.to.2, self.to.0)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct OverlayLayout {
    #[serde(default = "default_row_axis", skip_serializing_if = "is_default_row_axis")]
    rows: OverlayAxis,
    #[serde(default = "default_col_axis", skip_serializing_if = "is_default_col_axis")]
    cols: OverlayAxis,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    hidden_rows: Vec<(u32, u32)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    hidden_rows_user: Vec<(u32, u32)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    hidden_cols: Vec<(u32, u32)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    row_levels: Vec<(u32, u8)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    col_levels: Vec<(u32, u8)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    freeze: Option<(u32, u32)>,
    #[serde(default, skip_serializing_if = "is_default_page")]
    page: PageSetup,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pictures: Vec<PictureGeometry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    textboxes: Vec<TextBoxGeometry>,
}

impl Default for OverlayLayout {
    fn default() -> Self {
        Self {
            rows: default_row_axis(),
            cols: default_col_axis(),
            hidden_rows: Vec::new(),
            hidden_rows_user: Vec::new(),
            hidden_cols: Vec::new(),
            row_levels: Vec::new(),
            col_levels: Vec::new(),
            freeze: None,
            page: PageSetup::default(),
            pictures: Vec::new(),
            textboxes: Vec::new(),
        }
    }
}

impl OverlayLayout {
    fn is_default(&self) -> bool {
        is_default_row_axis(&self.rows)
            && is_default_col_axis(&self.cols)
            && self.hidden_rows.is_empty()
            && self.hidden_rows_user.is_empty()
            && self.hidden_cols.is_empty()
            && self.row_levels.is_empty()
            && self.col_levels.is_empty()
            && self.freeze.is_none()
            && is_default_page(&self.page)
            && self.pictures.is_empty()
            && self.textboxes.is_empty()
    }

    fn item_count(&self) -> usize {
        self.rows
            .spans
            .len()
            .saturating_add(self.cols.spans.len())
            .saturating_add(self.hidden_rows.len())
            .saturating_add(self.hidden_rows_user.len())
            .saturating_add(self.hidden_cols.len())
            .saturating_add(self.row_levels.len())
            .saturating_add(self.col_levels.len())
            .saturating_add(self.page.row_breaks.len())
            .saturating_add(self.page.col_breaks.len())
            .saturating_add(self.pictures.len())
            .saturating_add(self.textboxes.len())
    }

    fn valid(&self) -> bool {
        self.rows.valid(crate::sync::MAX_ROWS)
            && self.cols.valid(crate::sync::MAX_COLS)
            && valid_intervals(&self.hidden_rows, crate::sync::MAX_ROWS)
            && valid_intervals(&self.hidden_rows_user, crate::sync::MAX_ROWS)
            && valid_intervals(&self.hidden_cols, crate::sync::MAX_COLS)
            && self.row_levels.iter().all(|&(r, _)| r < crate::sync::MAX_ROWS)
            && self.col_levels.iter().all(|&(c, _)| c < crate::sync::MAX_COLS)
            && self
                .freeze
                .is_none_or(|(r, c)| r < crate::sync::MAX_ROWS && c < crate::sync::MAX_COLS)
            && valid_page_setup(&self.page)
            && self.pictures.iter().all(PictureGeometry::valid)
            && self.textboxes.iter().all(TextBoxGeometry::valid)
    }

    fn capture(sh: &Sheet) -> Self {
        let mut row_levels: Vec<(u32, u8)> = sh.row_levels.iter().map(|(&k, &v)| (k, v)).collect();
        row_levels.sort_unstable_by_key(|&(k, _)| k);
        let mut col_levels: Vec<(u32, u8)> = sh.col_levels.iter().map(|(&k, &v)| (k, v)).collect();
        col_levels.sort_unstable_by_key(|&(k, _)| k);
        Self {
            rows: OverlayAxis::capture(&sh.rows),
            cols: OverlayAxis::capture(&sh.cols),
            hidden_rows: sh.hidden_rows.intervals().to_vec(),
            hidden_rows_user: sh.hidden_rows_user.intervals().to_vec(),
            hidden_cols: sh.hidden_cols.intervals().to_vec(),
            row_levels,
            col_levels,
            freeze: sh.freeze,
            page: sh.page.clone(),
            pictures: sh.pictures.iter().map(PictureGeometry::read).collect(),
            textboxes: sh.textboxes.iter().map(TextBoxGeometry::read).collect(),
        }
    }

    fn apply(&self, sh: &mut Sheet) {
        let mut layout_changed = false;
        let desired_rows = self.rows.to_axis();
        if sh.rows != desired_rows {
            sh.rows = desired_rows;
            layout_changed = true;
        }
        let desired_cols = self.cols.to_axis();
        if sh.cols != desired_cols {
            sh.cols = desired_cols;
            layout_changed = true;
        }
        let desired_hr = HiddenIndices::from_intervals(self.hidden_rows.iter().copied());
        if sh.hidden_rows != desired_hr {
            sh.hidden_rows = desired_hr;
            layout_changed = true;
        }
        let desired_hru = HiddenIndices::from_intervals(self.hidden_rows_user.iter().copied());
        if sh.hidden_rows_user != desired_hru {
            sh.hidden_rows_user = desired_hru;
            layout_changed = true;
        }
        let desired_hc = HiddenIndices::from_intervals(self.hidden_cols.iter().copied());
        if sh.hidden_cols != desired_hc {
            sh.hidden_cols = desired_hc;
            layout_changed = true;
        }
        let desired_rl: HashMap<u32, u8> = self.row_levels.iter().copied().collect();
        if sh.row_levels != desired_rl {
            sh.row_levels = desired_rl;
            layout_changed = true;
        }
        let desired_cl: HashMap<u32, u8> = self.col_levels.iter().copied().collect();
        if sh.col_levels != desired_cl {
            sh.col_levels = desired_cl;
            layout_changed = true;
        }
        if layout_changed {
            sh.layout_dirty = true;
        }
        if sh.freeze != self.freeze {
            sh.freeze = self.freeze;
            sh.freeze_dirty = true;
        }
        if sh.page != self.page {
            sh.page.clone_from(&self.page);
            sh.page_dirty = true;
        }
        if !self.pictures.is_empty() && sh.pictures.len() == self.pictures.len() {
            let mut pic_changed = false;
            for (geo, pic) in self.pictures.iter().zip(sh.pictures.iter_mut()) {
                if pic.part == geo.part && (pic.rect != geo.rect || pic.off != geo.off) {
                    pic.rect = geo.rect;
                    pic.off = geo.off;
                    pic_changed = true;
                }
            }
            sh.pictures_dirty |= pic_changed;
        }
        if !self.textboxes.is_empty() && sh.textboxes.len() == self.textboxes.len() {
            let mut tb_changed = false;
            for (geo, tb) in self.textboxes.iter().zip(sh.textboxes.iter_mut()) {
                if tb.id == geo.id && (tb.from != geo.from || tb.to != geo.to) {
                    tb.from = geo.from;
                    tb.to = geo.to;
                    tb_changed = true;
                }
            }
            sh.textboxes_dirty |= tb_changed;
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
struct OverlayFilter {
    range: Rect,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    allows: Vec<(u32, Vec<String>)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    criteria: Vec<(u32, Criterion)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    date_groups: Vec<(u32, Vec<DateGroupItem>)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    color_filters: Vec<(u32, ColorFilter)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    top_filters: Vec<(u32, TopFilter)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    dynamic_filters: Vec<(u32, DynamicFilter)>,
}

impl OverlayFilter {
    fn capture(f: &Filter) -> Self {
        let mut allows: Vec<(u32, Vec<String>)> = f
            .allows
            .iter()
            .map(|(&c, set)| {
                let mut vals: Vec<String> = set.iter().cloned().collect();
                vals.sort_unstable();
                (c, vals)
            })
            .collect();
        allows.sort_unstable_by_key(|(c, _)| *c);
        let mut criteria: Vec<(u32, Criterion)> =
            f.criteria.iter().map(|(&c, v)| (c, v.clone())).collect();
        criteria.sort_unstable_by_key(|(c, _)| *c);
        let mut date_groups: Vec<(u32, Vec<DateGroupItem>)> = f
            .date_groups
            .iter()
            .map(|(&c, v)| {
                let mut items = v.clone();
                items.sort_unstable();
                (c, items)
            })
            .collect();
        date_groups.sort_unstable_by_key(|(c, _)| *c);
        let mut color_filters: Vec<(u32, ColorFilter)> =
            f.color_filters.iter().map(|(&c, &v)| (c, v)).collect();
        color_filters.sort_unstable_by_key(|(c, _)| *c);
        let mut top_filters: Vec<(u32, TopFilter)> =
            f.top_filters.iter().map(|(&c, &v)| (c, v)).collect();
        top_filters.sort_unstable_by_key(|(c, _)| *c);
        let mut dynamic_filters: Vec<(u32, DynamicFilter)> =
            f.dynamic_filters.iter().map(|(&c, &v)| (c, v)).collect();
        dynamic_filters.sort_unstable_by_key(|(c, _)| *c);
        Self {
            range: f.range,
            allows,
            criteria,
            date_groups,
            color_filters,
            top_filters,
            dynamic_filters,
        }
    }

    fn item_count(&self) -> usize {
        1usize
            .saturating_add(self.allows.iter().map(|(_, v)| 1 + v.len()).sum())
            .saturating_add(self.criteria.len())
            .saturating_add(self.date_groups.iter().map(|(_, v)| 1 + v.len()).sum())
            .saturating_add(self.color_filters.len())
            .saturating_add(self.top_filters.len())
            .saturating_add(self.dynamic_filters.len())
    }

    fn valid(&self) -> bool {
        let max_text = crate::sync::MAX_THREAD_ENTRY_TEXT_BYTES;
        valid_rect(&self.range)
            && self.allows.iter().all(|(c, vals)| {
                *c < crate::sync::MAX_COLS && vals.iter().all(|v| v.len() <= max_text)
            })
            && self.criteria.iter().all(|(c, cr)| {
                *c < crate::sync::MAX_COLS
                    && cr.val.len() <= max_text
                    && cr.val2.as_ref().is_none_or(|v| v.len() <= max_text)
                    && cr
                        .second
                        .as_ref()
                        .is_none_or(|(_, _, v)| v.len() <= max_text)
            })
            && self
                .date_groups
                .iter()
                .all(|(c, _)| *c < crate::sync::MAX_COLS)
            && self
                .color_filters
                .iter()
                .all(|(c, _)| *c < crate::sync::MAX_COLS)
            && self
                .top_filters
                .iter()
                .all(|(c, _)| *c < crate::sync::MAX_COLS)
            && self
                .dynamic_filters
                .iter()
                .all(|(c, _)| *c < crate::sync::MAX_COLS)
    }

    fn to_filter(&self) -> Filter {
        Filter {
            range: self.range,
            allows: self
                .allows
                .iter()
                .map(|(c, vals)| (*c, vals.iter().cloned().collect()))
                .collect(),
            criteria: self.criteria.iter().cloned().collect(),
            date_groups: self.date_groups.iter().cloned().collect(),
            color_filters: self.color_filters.iter().copied().collect(),
            top_filters: self.top_filters.iter().copied().collect(),
            dynamic_filters: self.dynamic_filters.iter().copied().collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct OverlayScenario {
    name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    comment: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    cells: Vec<((u32, u32), String)>,
    #[serde(default)]
    hidden: bool,
    #[serde(default)]
    locked: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    user: Option<String>,
    #[serde(default)]
    explicit_count: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    input_properties: Vec<((u32, u32), Vec<(String, String)>)>,
}

impl OverlayScenario {
    fn capture(sc: &crate::scenarios::Scenario) -> Self {
        Self {
            name: sc.name.clone(),
            comment: sc.comment.clone(),
            cells: sc.cells.clone(),
            hidden: sc.hidden,
            locked: sc.locked,
            user: sc.user().map(str::to_owned),
            explicit_count: sc.explicit_count(),
            input_properties: sc.sorted_input_properties(),
        }
    }

    fn item_count(&self) -> usize {
        1usize
            .saturating_add(self.cells.len())
            .saturating_add(self.input_properties.iter().map(|(_, p)| 1 + p.len()).sum())
    }

    fn valid(&self) -> bool {
        let max_short = crate::sync::MAX_SHORT_TEXT;
        let max_text = crate::sync::MAX_THREAD_ENTRY_TEXT_BYTES;
        self.name.len() <= max_short
            && self.comment.len() <= max_text
            && self.user.as_ref().is_none_or(|u| u.len() <= max_short)
            && self
                .cells
                .iter()
                .all(|((r, c), v)| valid_point(*r, *c) && v.len() <= max_text)
            && self.input_properties.iter().all(|((r, c), props)| {
                valid_point(*r, *c)
                    && props
                        .iter()
                        .all(|(k, v)| k.len() <= max_short && v.len() <= max_short)
            })
    }

    fn to_scenario(&self) -> crate::scenarios::Scenario {
        crate::scenarios::Scenario::from_portable(
            self.name.clone(),
            self.comment.clone(),
            self.cells.clone(),
            self.hidden,
            self.locked,
            self.user.clone(),
            self.explicit_count,
            self.input_properties.clone(),
        )
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct OverlayScenarioSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    current: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    show: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sqref: Option<String>,
}

impl OverlayScenarioSettings {
    fn capture(s: &crate::scenarios::ScenarioSettings) -> Self {
        Self {
            current: s.current,
            show: s.show,
            sqref: s.sqref.clone(),
        }
    }

    fn valid(&self) -> bool {
        self.sqref
            .as_ref()
            .is_none_or(|s| s.len() <= crate::sync::MAX_THREAD_ENTRY_TEXT_BYTES)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct PivotGeometry {
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    imported_part: Option<String>,
    source_sheet: String,
    target_sheet: usize,
    anchor: (u32, u32),
    rect: Rect,
    last_meta_anchor: (u32, u32),
    last_meta_rect: Rect,
    source: Rect,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    row_cols: Vec<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    col_cols: Vec<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    val_cols: Vec<(u32, Option<u32>)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    filter_cols: Vec<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    slicer_cols: Vec<u32>,
}

impl PivotGeometry {
    fn read(p: &crate::pivot_engine::PivotTableModel) -> Self {
        Self {
            name: p.name.clone(),
            imported_part: p.imported_part.clone(),
            source_sheet: p.source_sheet.clone(),
            target_sheet: p.target_sheet,
            anchor: p.anchor,
            rect: p.rect,
            last_meta_anchor: p.last_meta.anchor,
            last_meta_rect: p.last_meta.rect,
            source: p.spec.source,
            row_cols: p.spec.rows.iter().map(|f| f.col).collect(),
            col_cols: p.spec.cols.iter().map(|f| f.col).collect(),
            val_cols: p
                .spec
                .values
                .iter()
                .map(|f| (f.col, f.base.as_ref().map(|(c, _)| *c)))
                .collect(),
            filter_cols: p.spec.filters.iter().map(|f| f.col).collect(),
            slicer_cols: p.options.slicers.iter().map(|s| s.col).collect(),
        }
    }

    fn item_count(&self) -> usize {
        1usize
            .saturating_add(self.row_cols.len())
            .saturating_add(self.col_cols.len())
            .saturating_add(self.val_cols.len())
            .saturating_add(self.filter_cols.len())
            .saturating_add(self.slicer_cols.len())
    }

    fn valid(&self, sheet_count: usize) -> bool {
        let max_short = crate::sync::MAX_SHORT_TEXT;
        let max_c = crate::sync::MAX_COLS;
        self.name.len() <= max_short
            && self.source_sheet.len() <= max_short
            && self
                .imported_part
                .as_ref()
                .is_none_or(|p| p.len() <= max_short)
            && self.target_sheet < sheet_count
            && valid_point(self.anchor.0, self.anchor.1)
            && valid_rect(&self.rect)
            && valid_point(self.last_meta_anchor.0, self.last_meta_anchor.1)
            && valid_rect(&self.last_meta_rect)
            && valid_rect(&self.source)
            && self.row_cols.iter().all(|&c| c < max_c)
            && self.col_cols.iter().all(|&c| c < max_c)
            && self
                .val_cols
                .iter()
                .all(|&(c, b)| c < max_c && b.is_none_or(|bc| bc < max_c))
            && self.filter_cols.iter().all(|&c| c < max_c)
            && self.slicer_cols.iter().all(|&c| c < max_c)
    }

    fn matches_local(&self, p: &crate::pivot_engine::PivotTableModel) -> bool {
        match (&self.imported_part, &p.imported_part) {
            (Some(a), Some(b)) => a == b,
            (None, None) => self.name == p.name,
            _ => false,
        }
    }

    fn apply_to(
        &self,
        p: &mut crate::pivot_engine::PivotTableModel,
        sheets: &[Sheet],
    ) {
        let mut changed = false;
        if p.source_sheet != self.source_sheet {
            p.source_sheet.clone_from(&self.source_sheet);
            changed = true;
        }
        let resolved_sid = sheets
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(&self.source_sheet))
            .map(|s| s.identity);
        if p.source_sheet_id.is_some() && p.source_sheet_id != resolved_sid {
            p.source_sheet_id = resolved_sid;
        }
        if p.target_sheet != self.target_sheet {
            p.target_sheet = self.target_sheet;
            changed = true;
        }
        if p.anchor != self.anchor {
            p.anchor = self.anchor;
            changed = true;
        }
        if p.rect != self.rect {
            p.rect = self.rect;
            changed = true;
        }
        if p.last_meta.anchor != self.last_meta_anchor {
            p.last_meta.anchor = self.last_meta_anchor;
            changed = true;
        }
        if p.last_meta.rect != self.last_meta_rect {
            p.last_meta.rect = self.last_meta_rect;
            changed = true;
        }
        if p.spec.source != self.source {
            p.spec.source = self.source;
            changed = true;
        }
        if p.spec.rows.len() == self.row_cols.len() {
            for (f, &nc) in p.spec.rows.iter_mut().zip(&self.row_cols) {
                if f.col != nc {
                    f.col = nc;
                    changed = true;
                }
            }
        }
        if p.spec.cols.len() == self.col_cols.len() {
            for (f, &nc) in p.spec.cols.iter_mut().zip(&self.col_cols) {
                if f.col != nc {
                    f.col = nc;
                    changed = true;
                }
            }
        }
        if p.spec.values.len() == self.val_cols.len() {
            for (f, &(nc, nb)) in p.spec.values.iter_mut().zip(&self.val_cols) {
                if f.col != nc {
                    f.col = nc;
                    changed = true;
                }
                match (&mut f.base, nb) {
                    (Some((bc, _)), Some(nbc)) if *bc != nbc => {
                        *bc = nbc;
                        changed = true;
                    }
                    (Some(_), None) => {
                        f.base = None;
                        changed = true;
                    }
                    _ => {}
                }
            }
        }
        if p.spec.filters.len() == self.filter_cols.len() {
            for (f, &nc) in p.spec.filters.iter_mut().zip(&self.filter_cols) {
                if f.col != nc {
                    f.col = nc;
                    changed = true;
                }
            }
        }
        if p.options.slicers.len() == self.slicer_cols.len() {
            for (s, &nc) in p.options.slicers.iter_mut().zip(&self.slicer_cols) {
                if s.col != nc {
                    s.col = nc;
                    changed = true;
                }
            }
        }
        if changed {
            p.modified = true;
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct PendingPivotGeometry {
    sheet: usize,
    source: Rect,
    output: Rect,
    row_field: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    col_field: Option<u32>,
    value_field: u32,
    value_caption: String,
}

impl PendingPivotGeometry {
    fn valid(&self, sheet_count: usize) -> bool {
        let max_c = crate::sync::MAX_COLS;
        self.sheet < sheet_count
            && valid_rect(&self.source)
            && valid_rect(&self.output)
            && self.row_field < max_c
            && self.col_field.is_none_or(|c| c < max_c)
            && self.value_field < max_c
            && self.value_caption.len() <= crate::sync::MAX_SHORT_TEXT
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct PendingPivot2Geometry {
    sheet: usize,
    source: Rect,
    output: Rect,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    row_cols: Vec<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    col_cols: Vec<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    val_cols: Vec<(u32, Option<u32>)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    filter_cols: Vec<u32>,
}

impl PendingPivot2Geometry {
    fn item_count(&self) -> usize {
        1usize
            .saturating_add(self.row_cols.len())
            .saturating_add(self.col_cols.len())
            .saturating_add(self.val_cols.len())
            .saturating_add(self.filter_cols.len())
    }

    fn valid(&self, sheet_count: usize) -> bool {
        let max_c = crate::sync::MAX_COLS;
        self.sheet < sheet_count
            && valid_rect(&self.source)
            && valid_rect(&self.output)
            && self.row_cols.iter().all(|&c| c < max_c)
            && self.col_cols.iter().all(|&c| c < max_c)
            && self
                .val_cols
                .iter()
                .all(|&(c, b)| c < max_c && b.is_none_or(|bc| bc < max_c))
            && self.filter_cols.iter().all(|&c| c < max_c)
    }
}

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
    chart_anchors: Vec<(String, Rect)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    sparklines: Vec<crate::sparkline::Sparkline>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tables: Vec<Table>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    filter: Option<OverlayFilter>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    arrays: Vec<((u32, u32), Rect)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    spilled: Vec<(u32, u32)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    spill_shapes: Vec<((u32, u32), (u32, u32))>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    scenarios: Vec<OverlayScenario>,
    #[serde(default, skip_serializing_if = "is_default_scenario_settings")]
    scenario_settings: OverlayScenarioSettings,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    protected_ranges: Vec<ProtectedRange>,
    #[serde(default, skip_serializing_if = "OverlayLayout::is_default")]
    layout: OverlayLayout,
}

/// Current metadata of the authoritative workbook. Captured at join time, so
/// subsequent ordinary edits or a structural move take precedence over history.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CellShiftOverlay {
    #[serde(default, skip_serializing_if = "is_zero_u8")]
    format_version: u8,
    sheets: Vec<SheetOverlay>,
    palette: Vec<(CellStyle, StyleExtras)>,
    sources: Vec<(String, SourceGeometry)>,
    names: Vec<(String, Option<usize>, String)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pivots: Vec<PivotGeometry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pivots_pending: Vec<PendingPivotGeometry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pivots_pending2: Vec<PendingPivot2Geometry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pivot_shifts: Vec<(String, u8, u32, u32, bool)>,
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
    #[inline]
    pub fn format_version(&self) -> u8 {
        self.format_version
    }

    #[inline]
    pub fn is_full_state(&self) -> bool {
        self.format_version >= 1 && self.format_version <= CURRENT_OVERLAY_FORMAT_VERSION
    }

    pub fn wire_valid(&self) -> bool {
        if self.format_version > CURRENT_OVERLAY_FORMAT_VERSION {
            return false;
        }
        let mut count: usize = 0;
        for s in &self.sheets {
            count = count
                .saturating_add(s.notes.len())
                .saturating_add(s.threads.len())
                .saturating_add(s.rich.len())
                .saturating_add(s.links.len())
                .saturating_add(s.ignored_errors.len())
                .saturating_add(s.merges.len())
                .saturating_add(s.cf.blocks.len())
                .saturating_add(s.dv.len())
                .saturating_add(s.chart_refs.len())
                .saturating_add(s.chart_anchors.len())
                .saturating_add(s.sparklines.len())
                .saturating_add(s.styles.rows.len())
                .saturating_add(s.styles.cols.len())
                .saturating_add(s.styles.runs.len())
                .saturating_add(
                    s.tables
                        .iter()
                        .map(|t| {
                            1usize
                                .saturating_add(t.columns.len())
                                .saturating_add(t.column_ids.len())
                                .saturating_add(t.totals_fns.len())
                        })
                        .fold(0usize, usize::saturating_add),
                )
                .saturating_add(s.filter.as_ref().map_or(0, OverlayFilter::item_count))
                .saturating_add(s.arrays.len())
                .saturating_add(s.spilled.len())
                .saturating_add(s.spill_shapes.len())
                .saturating_add(
                    s.scenarios
                        .iter()
                        .map(OverlayScenario::item_count)
                        .fold(0usize, usize::saturating_add),
                )
                .saturating_add(
                    s.protected_ranges
                        .iter()
                        .map(|pr| 1usize.saturating_add(pr.sqref.len()))
                        .fold(0usize, usize::saturating_add),
                )
                .saturating_add(s.layout.item_count());
        }
        count = count
            .saturating_add(self.sources.len())
            .saturating_add(self.names.len())
            .saturating_add(
                self.pivots
                    .iter()
                    .map(PivotGeometry::item_count)
                    .fold(0usize, usize::saturating_add),
            )
            .saturating_add(self.pivots_pending.len())
            .saturating_add(
                self.pivots_pending2
                    .iter()
                    .map(PendingPivot2Geometry::item_count)
                    .fold(0usize, usize::saturating_add),
            )
            .saturating_add(self.pivot_shifts.len());

        let sheet_count = self.sheets.len();
        sheet_count <= crate::sync::MAX_SHEET_INDEX
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
            && count <= crate::sync::MAX_BATCH_ITEMS
            && self.names.iter().all(|(n, scope, text)| {
                valid_defined_name(n, *scope)
                    && super::valid_name_formula(text)
                    && scope.is_none_or(|i| i < sheet_count)
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
                    && s.chart_anchors
                        .iter()
                        .all(|(part, rect)| part.len() <= crate::sync::MAX_SHORT_TEXT && valid_rect(rect))
                    && s.sparklines.iter().all(valid_sparkline)
                    && s.tables.iter().all(valid_table)
                    && s.filter.as_ref().is_none_or(OverlayFilter::valid)
                    && s.arrays
                        .iter()
                        .all(|((r, c), rect)| valid_point(*r, *c) && valid_rect(rect))
                    && s.spilled.iter().all(|&(r, c)| valid_point(r, c))
                    && s.spill_shapes.iter().all(|&((r, c), (h, w))| {
                        valid_point(r, c)
                            && (h == 0 || r.checked_add(h - 1).is_some_and(|e| e < crate::sync::MAX_ROWS))
                            && (w == 0 || c.checked_add(w - 1).is_some_and(|e| e < crate::sync::MAX_COLS))
                    })
                    && s.scenarios.iter().all(OverlayScenario::valid)
                    && s.scenario_settings.valid()
                    && s.protected_ranges.iter().all(valid_protected_range)
                    && s.layout.valid()
            })
            && self
                .sources
                .iter()
                .all(|(n, g)| n.len() <= crate::sync::MAX_SHORT_TEXT && g.valid())
            && self.pivots.iter().all(|p| p.valid(sheet_count))
            && self.pivots_pending.iter().all(|p| p.valid(sheet_count))
            && self.pivots_pending2.iter().all(|p| p.valid(sheet_count))
            && self.pivot_shifts.iter().all(|(name, axis, at, n, _)| {
                name.len() <= crate::sync::MAX_SHORT_TEXT
                    && *axis <= 1
                    && {
                        let lim = if *axis == 0 {
                            crate::sync::MAX_ROWS
                        } else {
                            crate::sync::MAX_COLS
                        };
                        *at < lim && *n <= lim
                    }
            })
    }

    pub fn capture(wb: &Workbook) -> Self {
        use crate::participant::CoordParticipant;

        let mut overlay = Self {
            format_version: CURRENT_OVERLAY_FORMAT_VERSION,
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
                    chart_anchors: Vec::new(),
                    sparklines: Vec::new(),
                    tables: Vec::new(),
                    filter: None,
                    arrays: Vec::new(),
                    spilled: Vec::new(),
                    spill_shapes: Vec::new(),
                    scenarios: Vec::new(),
                    scenario_settings: OverlayScenarioSettings::default(),
                    protected_ranges: Vec::new(),
                    layout: OverlayLayout::default(),
                })
                .collect(),
            palette: Vec::new(),
            sources: Vec::new(),
            names: Vec::new(),
            pivots: Vec::new(),
            pivots_pending: Vec::new(),
            pivots_pending2: Vec::new(),
            pivot_shifts: Vec::new(),
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
                    dst.chart_anchors = s
                        .charts
                        .iter()
                        .map(|ch| (ch.part.clone(), ch.anchor))
                        .collect();
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
            P::Tables => {
                for (dst, s) in self.sheets.iter_mut().zip(&wb.sheets) {
                    dst.tables = s.tables.clone();
                }
            }
            P::AutoFilter => {
                for (dst, s) in self.sheets.iter_mut().zip(&wb.sheets) {
                    dst.filter = s.filter.as_ref().map(OverlayFilter::capture);
                }
            }
            P::ArraysAndSpills => {
                for (dst, s) in self.sheets.iter_mut().zip(&wb.sheets) {
                    dst.arrays = s
                        .arrays
                        .iter()
                        .map(|&(anchor, rect)| ((anchor.row, anchor.col), rect))
                        .collect();
                    let mut sp: Vec<(u32, u32)> =
                        s.spilled.iter().map(|k| (k.row, k.col)).collect();
                    sp.sort_unstable();
                    dst.spilled = sp;
                    dst.spill_shapes = entries(&s.spill_shapes);
                }
            }
            P::ScenariosAndProtectedRanges => {
                for (dst, s) in self.sheets.iter_mut().zip(&wb.sheets) {
                    dst.scenarios = s.scenarios.iter().map(OverlayScenario::capture).collect();
                    dst.scenario_settings = OverlayScenarioSettings::capture(&s.scenario_settings);
                    dst.protected_ranges = s.protected_ranges.clone();
                }
            }
            P::LayoutAndPage => {
                for (dst, s) in self.sheets.iter_mut().zip(&wb.sheets) {
                    dst.layout = OverlayLayout::capture(s);
                }
            }
            P::Pivots => {
                self.pivots = wb.pivots.iter().map(PivotGeometry::read).collect();
                self.pivots_pending = wb
                    .pivots_pending
                    .iter()
                    .map(|(idx, spec, _, out, caption)| PendingPivotGeometry {
                        sheet: *idx,
                        source: spec.source,
                        output: *out,
                        row_field: spec.row_field,
                        col_field: spec.col_field,
                        value_field: spec.value_field,
                        value_caption: caption.clone(),
                    })
                    .collect();
                self.pivots_pending2 = wb
                    .pivots_pending2
                    .iter()
                    .map(|(idx, spec, _, out)| PendingPivot2Geometry {
                        sheet: *idx,
                        source: spec.source,
                        output: *out,
                        row_cols: spec.rows.iter().map(|f| f.col).collect(),
                        col_cols: spec.cols.iter().map(|f| f.col).collect(),
                        val_cols: spec
                            .values
                            .iter()
                            .map(|f| (f.col, f.base.as_ref().map(|(c, _)| *c)))
                            .collect(),
                        filter_cols: spec.filters.iter().map(|f| f.col).collect(),
                    })
                    .collect();
                self.pivot_shifts = wb.pivot_shifts.clone();
            }
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
        let full = self.is_full_state();
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
                    if !overlay.chart_anchors.is_empty()
                        && sh.charts.len() == overlay.chart_anchors.len()
                    {
                        let mut changed = false;
                        for ((part, anchor), ch) in
                            overlay.chart_anchors.iter().zip(sh.charts.iter_mut())
                        {
                            if ch.part == *part && ch.anchor != *anchor {
                                ch.anchor = *anchor;
                                changed = true;
                            }
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
            P::Tables => {
                for (overlay, sh) in self.sheets.iter().zip(&mut wb.sheets) {
                    if (full || !overlay.tables.is_empty()) && sh.tables != overlay.tables {
                        sh.tables.clone_from(&overlay.tables);
                        sh.tables_dirty = true;
                    }
                }
            }
            P::AutoFilter => {
                for (overlay, sh) in self.sheets.iter().zip(&mut wb.sheets) {
                    if full || overlay.filter.is_some() {
                        let desired = overlay.filter.as_ref().map(OverlayFilter::to_filter);
                        if sh.filter != desired {
                            sh.filter = desired;
                            sh.filter_dirty = true;
                        }
                    }
                }
            }
            P::ArraysAndSpills => {
                for (overlay, sh) in self.sheets.iter().zip(&mut wb.sheets) {
                    let has_any = !overlay.arrays.is_empty()
                        || !overlay.spilled.is_empty()
                        || !overlay.spill_shapes.is_empty();
                    if !(full || has_any) {
                        continue;
                    }
                    let desired_arrays: Vec<(CellRef, Rect)> = overlay
                        .arrays
                        .iter()
                        .map(|&((r, c), rect)| (CellRef::new(r, c), rect))
                        .collect();
                    if sh.arrays != desired_arrays {
                        sh.arrays = desired_arrays;
                    }
                    let mut cur_spilled: Vec<(u32, u32)> =
                        sh.spilled.iter().map(|k| (k.row, k.col)).collect();
                    cur_spilled.sort_unstable();
                    if cur_spilled != overlay.spilled {
                        sh.spilled = overlay
                            .spilled
                            .iter()
                            .map(|&(r, c)| CellRef::new(r, c))
                            .collect();
                    }
                    replace_entries(&mut sh.spill_shapes, &overlay.spill_shapes);
                }
            }
            P::ScenariosAndProtectedRanges => {
                for (overlay, sh) in self.sheets.iter().zip(&mut wb.sheets) {
                    let has_scenarios = !overlay.scenarios.is_empty()
                        || !is_default_scenario_settings(&overlay.scenario_settings);
                    if full || has_scenarios {
                        let desired_scenarios: Vec<_> = overlay
                            .scenarios
                            .iter()
                            .map(OverlayScenario::to_scenario)
                            .collect();
                        if sh.scenarios != desired_scenarios {
                            sh.scenarios = desired_scenarios;
                            sh.scenarios_dirty = true;
                        }
                        if sh.scenario_settings.current != overlay.scenario_settings.current
                            || sh.scenario_settings.show != overlay.scenario_settings.show
                            || sh.scenario_settings.sqref != overlay.scenario_settings.sqref
                        {
                            sh.scenario_settings.current = overlay.scenario_settings.current;
                            sh.scenario_settings.show = overlay.scenario_settings.show;
                            sh.scenario_settings
                                .sqref
                                .clone_from(&overlay.scenario_settings.sqref);
                            sh.scenarios_dirty = true;
                        }
                    }
                    if (full || !overlay.protected_ranges.is_empty())
                        && sh.protected_ranges != overlay.protected_ranges
                    {
                        sh.protected_ranges.clone_from(&overlay.protected_ranges);
                        sh.protected_ranges_dirty = true;
                    }
                }
            }
            P::LayoutAndPage => {
                for (overlay, sh) in self.sheets.iter().zip(&mut wb.sheets) {
                    if full || !overlay.layout.is_default() {
                        overlay.layout.apply(sh);
                    }
                }
            }
            P::Pivots => {
                if full || !self.pivots.is_empty() {
                    let mut used = vec![false; self.pivots.len()];
                    let sheets_ref = &wb.sheets;
                    let removed_parts = &mut wb.pivots_removed_parts;
                    wb.pivots.retain_mut(|local| {
                        if let Some((idx, geo)) = self
                            .pivots
                            .iter()
                            .enumerate()
                            .find(|(i, g)| !used[*i] && g.matches_local(local))
                        {
                            used[idx] = true;
                            geo.apply_to(local, sheets_ref);
                            true
                        } else if full {
                            if let Some(part) = local.imported_part.take()
                                && !removed_parts.contains(&part)
                            {
                                removed_parts.push(part);
                            }
                            false
                        } else {
                            true
                        }
                    });
                }
                if full || !self.pivots_pending.is_empty() {
                    if self.pivots_pending.is_empty() && full {
                        wb.pivots_pending.clear();
                    } else if wb.pivots_pending.len() == self.pivots_pending.len() {
                        for ((idx, spec, _, out, _), geo) in
                            wb.pivots_pending.iter_mut().zip(&self.pivots_pending)
                        {
                            *idx = geo.sheet;
                            spec.source = geo.source;
                            spec.row_field = geo.row_field;
                            spec.col_field = geo.col_field;
                            spec.value_field = geo.value_field;
                            *out = geo.output;
                        }
                    } else if full {
                        let mut used = vec![false; self.pivots_pending.len()];
                        wb.pivots_pending.retain_mut(|(idx, spec, _, out, cap)| {
                            if let Some((gi, geo)) = self
                                .pivots_pending
                                .iter()
                                .enumerate()
                                .find(|(i, g)| !used[*i] && g.value_caption == *cap)
                            {
                                used[gi] = true;
                                *idx = geo.sheet;
                                spec.source = geo.source;
                                spec.row_field = geo.row_field;
                                spec.col_field = geo.col_field;
                                spec.value_field = geo.value_field;
                                *out = geo.output;
                                true
                            } else {
                                false
                            }
                        });
                    }
                }
                if full || !self.pivots_pending2.is_empty() {
                    let apply_p2 =
                        |entry: &mut (usize, crate::pivot_multi::PivotSpec2, crate::pivot_multi::PivotGrid, Rect),
                         geo: &PendingPivot2Geometry| {
                            let (idx, spec, _, out) = entry;
                            *idx = geo.sheet;
                            spec.source = geo.source;
                            *out = geo.output;
                            if spec.rows.len() == geo.row_cols.len() {
                                for (f, &nc) in spec.rows.iter_mut().zip(&geo.row_cols) {
                                    f.col = nc;
                                }
                            }
                            if spec.cols.len() == geo.col_cols.len() {
                                for (f, &nc) in spec.cols.iter_mut().zip(&geo.col_cols) {
                                    f.col = nc;
                                }
                            }
                            if spec.values.len() == geo.val_cols.len() {
                                for (f, &(nc, nb)) in spec.values.iter_mut().zip(&geo.val_cols) {
                                    f.col = nc;
                                    match (&mut f.base, nb) {
                                        (Some((bc, _)), Some(nbc)) => *bc = nbc,
                                        (Some(_), None) => f.base = None,
                                        _ => {}
                                    }
                                }
                            }
                            if spec.filters.len() == geo.filter_cols.len() {
                                for (f, &nc) in spec.filters.iter_mut().zip(&geo.filter_cols) {
                                    f.col = nc;
                                }
                            }
                        };
                    if self.pivots_pending2.is_empty() && full {
                        wb.pivots_pending2.clear();
                    } else if wb.pivots_pending2.len() == self.pivots_pending2.len() {
                        for (entry, geo) in
                            wb.pivots_pending2.iter_mut().zip(&self.pivots_pending2)
                        {
                            apply_p2(entry, geo);
                        }
                    } else if full {
                        let mut used = vec![false; self.pivots_pending2.len()];
                        wb.pivots_pending2.retain_mut(|entry| {
                            let (idx, spec, _, _) = entry;
                            if let Some((gi, geo)) =
                                self.pivots_pending2.iter().enumerate().find(|(i, g)| {
                                    !used[*i]
                                        && g.sheet == *idx
                                        && g.row_cols.len() == spec.rows.len()
                                        && g.col_cols.len() == spec.cols.len()
                                        && g.val_cols.len() == spec.values.len()
                                        && g.filter_cols.len() == spec.filters.len()
                                })
                            {
                                used[gi] = true;
                                apply_p2(entry, geo);
                                true
                            } else {
                                false
                            }
                        });
                    }
                }
                if (full || !self.pivot_shifts.is_empty()) && wb.pivot_shifts != self.pivot_shifts {
                    wb.pivot_shifts.clone_from(&self.pivot_shifts);
                }
            }
        }
    }
}
