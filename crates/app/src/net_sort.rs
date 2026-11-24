//! The receiver expands a sort against its own immutable source. Only the exact
//! operations (including ownership timestamps, cached results and styles) whose
//! digest matches the sender are accepted. A miss requests ordinary cell frames.
use sha2::{Digest, Sha256};
use sheetcore::style::CellStyle;
use sheetcore::sync::{Op, SortPermutation, ValueWire};
use std::sync::Arc;

#[derive(Clone, Copy, Debug)]
pub(crate) struct SortTarget {
    pub document: u64,
    pub sheet: usize,
    pub identity: u64,
    pub coordinates: u64,
}

impl SortTarget {
    pub fn valid(&self, document: u64, workbook: &sheetcore::Workbook) -> bool {
        self.document == document
            && workbook.sheets.get(self.sheet).is_some_and(|sheet| {
                self.identity == sheet.identity && self.coordinates == sheet.coordinate_revision()
            })
    }
}

type StyleItem = (u32, u32, CellStyle);

#[derive(Debug)]
pub(crate) struct SortBatch {
    pub plan: SortPermutation,
    pub ops: Vec<Arc<Op>>,
    pub styles: Vec<StyleItem>,
    checksum: std::sync::OnceLock<[u8; 32]>,
}

impl SortBatch {
    pub fn new(plan: SortPermutation, ops: Vec<Arc<Op>>, styles: Vec<StyleItem>) -> Self {
        Self {
            plan,
            ops,
            styles,
            checksum: Default::default(),
        }
    }

    pub fn offer(&self) -> SortPermutation {
        let mut plan = self.plan.clone();
        plan.digest = *self
            .checksum
            .get_or_init(|| digest(&self.ops, &self.styles));
        plan
    }

    pub fn expand(
        plan: &SortPermutation,
        read: impl FnMut(u32, u32) -> (ValueWire, Option<Arc<str>>, Arc<CellStyle>),
    ) -> Option<Self> {
        Self::expand_while(plan, None, read, || true)
    }

    fn expand_while(
        plan: &SortPermutation,
        active_offsets: Option<&[u32]>,
        mut read: impl FnMut(u32, u32) -> (ValueWire, Option<Arc<str>>, Arc<CellStyle>),
        pending: impl Fn() -> bool,
    ) -> Option<Self> {
        if !plan.valid() {
            return None;
        }
        let (r1, c1, r2, c2) = plan.region;
        let (first, start, end) = if plan.horizontal {
            (c1, r1, r2)
        } else {
            (r1, c1, c2)
        };
        let coords = |line, offset| {
            if plan.horizontal {
                (offset, line)
            } else {
                (line, offset)
            }
        };
        let mut ops = Vec::new();
        let mut styles = Vec::new();
        for (i, &src) in plan.order.iter().enumerate() {
            if i % 64 == 0 && !pending() {
                return None;
            }
            let dst = first + i as u32;
            if dst == src {
                continue;
            }
            let mut visit_offset = |step: usize, offset: u32| -> Option<()> {
                if step != 0 && step % 2048 == 0 && !pending() {
                    return None;
                }
                let (r, c) = coords(src, offset);
                let (row, col) = coords(dst, offset);
                let (value, formula, style) = read(r, c);
                let (before, prev_formula, prev_style) = read(row, col);
                let formula = formula.map(|f| {
                    let shift = i64::from(dst) - i64::from(src);
                    let translated = sheetcore::refs::translate(
                        &f,
                        if plan.horizontal { 0 } else { shift },
                        if plan.horizontal { shift } else { 0 },
                        &plan.name,
                    );
                    if translated == *f {
                        f
                    } else {
                        Arc::<str>::from(translated)
                    }
                });
                if before == value && formula == prev_formula && style == prev_style {
                    return Some(());
                }
                if style != prev_style {
                    styles.push((row, col, (*style).clone()));
                }
                let ts = plan.first_ts.checked_add(ops.len() as u64)?;
                if ts > sheetcore::sync::MAX_LOGICAL_TS {
                    return None;
                }
                ops.push(Arc::new(Op {
                    sheet: plan.sheet,
                    row,
                    col,
                    value,
                    formula,
                    ts,
                    site: plan.site,
                }));
                Some(())
            };
            if let Some(offsets) = active_offsets {
                for (step, &offset) in offsets.iter().enumerate() {
                    visit_offset(step, offset)?;
                }
            } else {
                for (step, offset) in (start..=end).enumerate() {
                    visit_offset(step, offset)?;
                }
            }
        }
        (digest(&ops, &styles) == plan.digest).then(|| Self {
            plan: plan.clone(),
            ops,
            styles,
            checksum: plan.digest.into(),
        })
    }
}

fn digest(ops: &[Arc<Op>], styles: &[StyleItem]) -> [u8; 32] {
    fn bytes(hash: &mut Sha256, value: &[u8]) {
        hash.update((value.len() as u64).to_le_bytes());
        hash.update(value);
    }
    let mut hash = Sha256::new();
    hash.update((ops.len() as u64).to_le_bytes());
    for op in ops {
        hash.update((op.sheet as u64).to_le_bytes());
        hash.update(op.row.to_le_bytes());
        hash.update(op.col.to_le_bytes());
        hash.update(op.site.to_le_bytes());
        hash.update(op.ts.to_le_bytes());
        match &op.value {
            ValueWire::Empty => hash.update([0]),
            ValueWire::Number(n) => {
                hash.update([1]);
                hash.update(n.to_bits().to_le_bytes());
            }
            ValueWire::Text(s) => {
                hash.update([2]);
                bytes(&mut hash, s.as_bytes());
            }
            ValueWire::Bool(b) => hash.update([3, *b as u8]),
            ValueWire::Error(s) => {
                hash.update([4]);
                bytes(&mut hash, s.as_bytes());
            }
        }
        hash.update([u8::from(op.formula.is_some())]);
        if let Some(f) = &op.formula {
            bytes(&mut hash, f.as_bytes());
        }
    }
    // 按唯一 CellStyle 缓存 JSON 字节，消除百万格带样式排序时百万次重复 serde_json::to_vec
    let mut style_json_cache: std::collections::HashMap<u64, Vec<(CellStyle, Vec<u8>)>> =
        std::collections::HashMap::new();
    for (r, c, style) in styles {
        hash.update(r.to_le_bytes());
        hash.update(c.to_le_bytes());
        let h = sheetcore::sync::cell_style_hash(style);
        let bucket = style_json_cache.entry(h).or_default();
        if let Some((_, encoded)) = bucket.iter().find(|(s, _)| s == style) {
            bytes(&mut hash, encoded);
        } else {
            let encoded = serde_json::to_vec(style).expect("serializable style");
            bytes(&mut hash, &encoded);
            bucket.push((style.clone(), encoded));
        }
    }
    hash.finalize().into()
}

pub(crate) fn from_sheet(
    sh: &sheetcore::Sheet,
    palette: &sheetcore::style::StyleTable,
    plan: &SortPermutation,
    pending: impl Fn() -> bool,
) -> Option<SortBatch> {
    let (r1, c1, r2, c2) = plan.region;
    let active_offsets: Option<Vec<u32>> = if !plan.horizontal
        && sh.row_styles.keys().all(|&r| !(r1..=r2).contains(&r))
    {
        let offset_len = (c2 - c1 + 1) as usize;
        let mut used = vec![false; offset_len];
        let mut count = 0usize;
        for &c in sh.col_styles.keys() {
            if (c1..=c2).contains(&c) && !std::mem::replace(&mut used[(c - c1) as usize], true) {
                count += 1;
            }
        }
        if count < offset_len {
            for (k, _) in sh.iter_rect(plan.region) {
                if !std::mem::replace(&mut used[(k.col - c1) as usize], true) {
                    count += 1;
                    if count == offset_len {
                        break;
                    }
                }
            }
        }
        if count < offset_len {
            for (k, _) in sh.styles.iter_rect(plan.region) {
                if !std::mem::replace(&mut used[(k.col - c1) as usize], true) {
                    count += 1;
                    if count == offset_len {
                        break;
                    }
                }
            }
        }
        Some(
            used.into_iter()
                .enumerate()
                .filter_map(|(i, on)| on.then_some(c1 + i as u32))
                .collect(),
        )
    } else {
        None
    };
    let mut styles = std::collections::HashMap::new();
    SortBatch::expand_while(
        plan,
        active_offsets.as_deref(),
        |r, c| {
            let style = styles
                .entry(sh.style_of(r, c))
                .or_insert_with_key(|id| Arc::new(palette.get(*id).clone()));
            let cell = sh.get(r, c);
            (
                cell.as_deref().map_or(ValueWire::Empty, |v| (&v.value).into()),
                cell.as_deref().and_then(|v| v.formula.clone()),
                style.clone(),
            )
        },
        pending,
    )
}

// Persistent ordered maps give sort workers a stable view in constant time.
// A regular edit copies only the path to that key when a worker holds a snapshot.
type Key = (usize, u32, u32);
pub(crate) type Grid<V> = immutable_chunkmap::map::MapM<Key, Arc<V>>;
pub(crate) type SharedGrid<V> = Arc<std::sync::Mutex<Arc<Grid<V>>>>;
pub(crate) type SharedBaseline = Arc<std::sync::Mutex<Option<Arc<sheetcore::sync::BaselineBook>>>>;

#[derive(Clone, Debug, Default)]
pub(crate) struct SheetAxisStyles {
    pub rows: std::collections::BTreeMap<u32, Arc<CellStyle>>,
    pub cols: std::collections::BTreeMap<u32, Arc<CellStyle>>,
}

impl SheetAxisStyles {
    #[inline]
    pub fn get(&self, row: u32, col: u32) -> Option<&Arc<CellStyle>> {
        self.rows.get(&row).or_else(|| self.cols.get(&col))
    }
}

pub(crate) type AxisStylesMap = Arc<std::collections::HashMap<usize, SheetAxisStyles>>;

#[derive(Clone)]
pub(crate) struct RoomGrids {
    pub baseline: Option<Arc<sheetcore::sync::BaselineBook>>,
    pub axis_styles: AxisStylesMap,
    pub cells: Arc<Grid<Op>>,
    pub delta: Arc<Grid<Op>>,
    pub styles: Arc<Grid<CellStyle>>,
    pub delta_styles: Arc<Grid<CellStyle>>,
}

impl RoomGrids {
    pub fn capture(
        baseline: &SharedBaseline,
        axis_styles: AxisStylesMap,
        cells: &SharedGrid<Op>,
        delta: &SharedGrid<Op>,
        styles: &SharedGrid<CellStyle>,
        delta_styles: &SharedGrid<CellStyle>,
    ) -> Self {
        Self {
            baseline: baseline.lock().unwrap().clone(),
            axis_styles,
            cells: cells.lock().unwrap().clone(),
            delta: delta.lock().unwrap().clone(),
            styles: styles.lock().unwrap().clone(),
            delta_styles: delta_styles.lock().unwrap().clone(),
        }
    }

    pub fn expand(&self, plan: &SortPermutation) -> Option<SortBatch> {
        if !plan.valid() {
            return None;
        }
        let (r1, c1, r2, c2) = plan.region;
        let default = self.baseline.as_ref().map_or_else(
            || Arc::new(CellStyle::default_dark()),
            |b| Arc::new(b.styles.get(0).clone()),
        );
        let (offset_start, offset_end) = if plan.horizontal { (r1, r2) } else { (c1, c2) };
        let offset_len = (offset_end - offset_start + 1) as usize;
        let mut occupied_offset = vec![false; offset_len];
        let mut occupied_count = 0usize;
        let mark_slot = |occupied: &mut [bool], count: &mut usize, r: u32, c: u32| {
            let off = if plan.horizontal { r } else { c };
            let slot = &mut occupied[(off - offset_start) as usize];
            if !*slot {
                *slot = true;
                *count += 1;
            }
        };

        let sheet_axis = self.axis_styles.get(&plan.sheet);
        let mut fill_all_offsets = false;
        if let Some(ax) = sheet_axis {
            if plan.horizontal {
                if ax.cols.range(c1..=c2).next().is_some() {
                    fill_all_offsets = true;
                } else {
                    for (&r, _) in ax.rows.range(r1..=r2) {
                        mark_slot(&mut occupied_offset, &mut occupied_count, r, c1);
                    }
                }
            } else if ax.rows.range(r1..=r2).next().is_some() {
                fill_all_offsets = true;
            } else {
                for (&c, _) in ax.cols.range(c1..=c2) {
                    mark_slot(&mut occupied_offset, &mut occupied_count, r1, c);
                }
            }
        }

        let mut has_cell_overlay = false;
        for (&(_, r, c), _) in self
            .cells
            .range((plan.sheet, r1, c1)..=(plan.sheet, r2, c2))
        {
            if (c1..=c2).contains(&c) {
                has_cell_overlay = true;
                if !fill_all_offsets {
                    mark_slot(&mut occupied_offset, &mut occupied_count, r, c);
                }
            }
        }

        let mut has_style_overlay = false;
        for (&(_, r, c), _) in self
            .styles
            .range((plan.sheet, r1, c1)..=(plan.sheet, r2, c2))
        {
            if (c1..=c2).contains(&c) {
                has_style_overlay = true;
                if !fill_all_offsets {
                    mark_slot(&mut occupied_offset, &mut occupied_count, r, c);
                }
            }
        }

        if fill_all_offsets {
            occupied_offset.fill(true);
        } else if let Some(base) = &self.baseline {
            if occupied_count < offset_len {
                base.for_each_cell_in_rect(plan.sheet, plan.region, |r, c, _| {
                    if occupied_count < offset_len {
                        mark_slot(&mut occupied_offset, &mut occupied_count, r, c);
                    }
                });
            }
            if occupied_count < offset_len {
                base.for_each_style_in_rect(plan.sheet, plan.region, |r, c, _| {
                    if occupied_count < offset_len {
                        mark_slot(&mut occupied_offset, &mut occupied_count, r, c);
                    }
                });
            }
        }

        let active_offsets: Vec<u32> = occupied_offset
            .into_iter()
            .enumerate()
            .filter_map(|(i, used)| used.then_some(offset_start + i as u32))
            .collect();

        let mut style_id_cache: std::collections::HashMap<u16, Arc<CellStyle>> =
            std::collections::HashMap::new();

        SortBatch::expand_while(
            plan,
            Some(&active_offsets),
            |r, c| {
                let (val, formula) = if has_cell_overlay
                    && let Some(op) = self.cells.get(&(plan.sheet, r, c))
                {
                    if matches!(op.value, ValueWire::Empty) && op.formula.is_none() {
                        (ValueWire::Empty, None)
                    } else {
                        (op.value.clone(), op.formula.clone())
                    }
                } else if let Some(base) = &self.baseline
                    && let Some(vf) = base.cell_value_wire(plan.sheet, r, c)
                {
                    vf
                } else {
                    (ValueWire::Empty, None)
                };

                let st = if has_style_overlay
                    && let Some(explicit) = self.styles.get(&(plan.sheet, r, c))
                {
                    explicit.clone()
                } else if let Some(base) = &self.baseline
                    && let Some(sid) = base.cell_style_id(plan.sheet, r, c)
                {
                    style_id_cache
                        .entry(sid)
                        .or_insert_with(|| Arc::new(base.styles.get(sid).clone()))
                        .clone()
                } else if let Some(axis_st) = sheet_axis.and_then(|ax| ax.get(r, c)) {
                    axis_st.clone()
                } else {
                    default.clone()
                };
                (val, formula, st)
            },
            || true,
        )
    }

    pub fn apply(&self, batch: &SortBatch) -> Self {
        let mut next = self.clone();
        let cells = Arc::make_mut(&mut next.cells);
        let delta = Arc::make_mut(&mut next.delta);
        for op in &batch.ops {
            upsert(cells, op);
            upsert(delta, op);
        }
        let styles = Arc::make_mut(&mut next.styles);
        let delta_styles = Arc::make_mut(&mut next.delta_styles);
        for (r, c, style) in &batch.styles {
            let key = (batch.plan.sheet, *r, *c);
            let style = Arc::new(style.clone());
            styles.insert_cow(key, style.clone());
            delta_styles.insert_cow(key, style);
        }
        next
    }

    // Caller holds the room ordering mutex across validation, publication and notification.
    pub fn publish(
        &self,
        next: &Self,
        cells: &SharedGrid<Op>,
        delta: &SharedGrid<Op>,
        styles: &SharedGrid<CellStyle>,
        delta_styles: &SharedGrid<CellStyle>,
    ) -> bool {
        let mut c = cells.lock().unwrap();
        let mut d = delta.lock().unwrap();
        let mut s = styles.lock().unwrap();
        let mut ds = delta_styles.lock().unwrap();
        if !(Arc::ptr_eq(&c, &self.cells)
            && Arc::ptr_eq(&d, &self.delta)
            && Arc::ptr_eq(&s, &self.styles)
            && Arc::ptr_eq(&ds, &self.delta_styles))
        {
            return false;
        }
        *c = next.cells.clone();
        *d = next.delta.clone();
        *s = next.styles.clone();
        *ds = next.delta_styles.clone();
        true
    }
}

pub(crate) fn upsert(map: &mut Grid<Op>, op: &Arc<Op>) {
    let key = (op.sheet, op.row, op.col);
    if map
        .get(&key)
        .is_none_or(|v| (op.ts, op.site) > (v.ts, v.site))
    {
        map.insert_cow(key, op.clone());
    }
}

/// Iterate only affected suffixes. Keep original entries available until every
/// destination has been computed, including inserts that overlap existing keys.
pub(crate) fn shift_grid<V>(
    grid: &SharedGrid<V>,
    sheet: usize,
    op: &sheetcore::structural::Structural,
    mut relocate: impl FnMut(&mut Arc<V>, u32, u32),
) {
    use sheetcore::structural::Structural::*;
    let mut root = grid.lock().unwrap();
    let source = root.clone();
    let target = Arc::make_mut(&mut root);
    let mut moved = Vec::new();
    let mut shift = |first, last| {
        for (&key, value) in source.range(first..=last) {
            target.remove_cow(&key);
            if let Some((r, c)) = sheetcore::sync::shift_structural_coord(key.1, key.2, op) {
                let mut value = value.clone();
                relocate(&mut value, r, c);
                moved.push(((sheet, r, c), value));
            }
        }
    };
    let last = (sheet, u32::MAX, u32::MAX);
    match *op {
        InsertRows { at, .. } | DeleteRows { at, .. } => shift((sheet, at, 0), last),
        InsertCols { at: 0, .. } | DeleteCols { at: 0, .. } => shift((sheet, 0, 0), last),
        InsertCols { at, .. } | DeleteCols { at, .. } => {
            let mut from = 0;
            while let Some((&(_, row, _), _)) = source.range((sheet, from, 0)..=last).next() {
                shift((sheet, row, at), (sheet, row, u32::MAX));
                let Some(next) = row.checked_add(1) else {
                    break;
                };
                from = next;
            }
        }
    }
    for (key, value) in moved {
        target.insert_cow(key, value);
    }
}
