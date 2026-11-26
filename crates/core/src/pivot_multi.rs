//! 多字段透视模型（C12.2）：多行/多列/多值/筛选/小计/总计。
//!
//! 结果仍是「值化」网格（Excel 里粘贴为值），但规格与结果按字段列表建模，
//! 侧栏可以重排字段、增删值字段、勾选筛选并刷新。真正的 pivotTable 部件
//! 由 `pivot_xml::attach` 另行附加，导入的复杂透视保持原样。

use crate::model::{Sheet, Value};
use crate::pivot::{group_key, Agg, GroupBy};
use std::collections::BTreeMap;

/// 行/列字段：列号 + 分组方式
#[derive(Clone, Debug, PartialEq)]
pub struct AxisField {
    pub col: u32,
    pub group: GroupBy,
}

impl AxisField {
    pub fn plain(col: u32) -> Self {
        Self { col, group: GroupBy::None }
    }
}

/// 显示值方式（Excel: showDataAs 属性）
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ShowAs {
    #[default]
    Normal,
    PercentOfRow,
    PercentOfColumn,
    PercentOfTotal,
    /// 占父行汇总百分比
    PercentOfParentRow,
    /// 占父列汇总百分比
    PercentOfParentCol,
    /// 按行累计汇总
    RunningTotal,
    /// 按行占累计汇总百分比
    PercentRunningTotal,
    /// 降序排名（最大为 1）
    RankDesc,
    /// 升序排名（最小为 1）
    RankAsc,
    /// 与上一项（或基准项）的差
    DifferencePrev,
    /// 与上一项（或基准项）的百分比差
    PercentDifferencePrev,
    /// Excel 的「索引」：(值 × 总计) ÷ (行合计 × 列合计)
    Index,
}

impl ShowAs {
    pub const ALL: &'static [ShowAs] = &[
        ShowAs::Normal,
        ShowAs::PercentOfTotal,
        ShowAs::PercentOfRow,
        ShowAs::PercentOfColumn,
        ShowAs::PercentOfParentRow,
        ShowAs::PercentOfParentCol,
        ShowAs::RunningTotal,
        ShowAs::PercentRunningTotal,
        ShowAs::RankDesc,
        ShowAs::RankAsc,
        ShowAs::DifferencePrev,
        ShowAs::PercentDifferencePrev,
        ShowAs::Index,
    ];

    /// Excel 写在 `<dataField showDataAs="...">` 里的值
    pub fn xml(self) -> Option<&'static str> {
        match self {
            ShowAs::Normal => None,
            ShowAs::PercentOfRow => Some("percentOfRow"),
            ShowAs::PercentOfColumn => Some("percentOfColumn"),
            ShowAs::PercentOfTotal => Some("percentOfTotal"),
            ShowAs::PercentOfParentRow => Some("percentOfParentRow"),
            ShowAs::PercentOfParentCol => Some("percentOfParentCol"),
            ShowAs::RunningTotal => Some("runTotal"),
            ShowAs::PercentRunningTotal => Some("percentOfRunningTotal"),
            ShowAs::RankDesc => Some("rankDescending"),
            ShowAs::RankAsc => Some("rankAscending"),
            ShowAs::DifferencePrev => Some("difference"),
            ShowAs::PercentDifferencePrev => Some("percent"),
            ShowAs::Index => Some("index"),
        }
    }

    pub fn from_ooxml(s: Option<&str>) -> Self {
        match s {
            Some("percentOfRow") => ShowAs::PercentOfRow,
            Some("percentOfColumn") => ShowAs::PercentOfColumn,
            Some("percentOfTotal") => ShowAs::PercentOfTotal,
            Some("percentOfParentRow") => ShowAs::PercentOfParentRow,
            Some("percentOfParentCol") => ShowAs::PercentOfParentCol,
            Some("runTotal") => ShowAs::RunningTotal,
            Some("percentOfRunningTotal") => ShowAs::PercentRunningTotal,
            Some("rankDescending") => ShowAs::RankDesc,
            Some("rankAscending") => ShowAs::RankAsc,
            Some("difference") => ShowAs::DifferencePrev,
            Some("percent") | Some("percentDiff") => ShowAs::PercentDifferencePrev,
            Some("index") => ShowAs::Index,
            _ => ShowAs::Normal,
        }
    }

    /// 这类显示值要不要套百分比格式（Excel 只给百分比类写 numFmtId="10"）
    pub fn percent_format(self) -> bool {
        matches!(
            self,
            ShowAs::PercentOfRow
                | ShowAs::PercentOfColumn
                | ShowAs::PercentOfTotal
                | ShowAs::PercentOfParentRow
                | ShowAs::PercentOfParentCol
                | ShowAs::PercentRunningTotal
                | ShowAs::PercentDifferencePrev
        )
    }

    /// 界面上的中文名（保留原有 7 项稳定标签，供既有测试与紧凑控件使用）
    pub fn label(self) -> &'static str {
        match self {
            ShowAs::Normal => "值",
            ShowAs::PercentOfRow => "占行合计%",
            ShowAs::PercentOfColumn => "占列合计%",
            ShowAs::PercentOfTotal => "占总计%",
            ShowAs::PercentOfParentRow => "占父行汇总%",
            ShowAs::PercentOfParentCol => "占父列汇总%",
            ShowAs::RunningTotal => "累计汇总",
            ShowAs::PercentRunningTotal => "占累计汇总%",
            ShowAs::RankDesc => "降序排名",
            ShowAs::RankAsc => "升序排名",
            ShowAs::DifferencePrev => "与上一项差",
            ShowAs::PercentDifferencePrev => "与上一项差%",
            ShowAs::Index => "索引",
        }
    }
}

/// 值字段：列号 + 汇总方式 + 可选显示名 + 显示值方式
#[derive(Clone, Debug, PartialEq)]
pub struct ValueField {
    pub col: u32,
    pub agg: Agg,
    pub label: Option<String>,
    pub show_as: ShowAs,
    /// 自定义基准（显示值方式用）：(源列, 项标签)；None = 与上一项/默认基准
    pub base: Option<(u32, String)>,
}

/// 筛选字段：keep 为空表示全部
#[derive(Clone, Debug, PartialEq)]
pub struct FilterField {
    pub col: u32,
    pub keep: Vec<String>,
}

/// 行/列标签的排序口径
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AxisSort {
    /// 拼音序（Excel 新建数据透视表的默认行为，A/B/C 实验已定论）
    Pinyin,
    /// 数据源出现顺序（Excel 打开既有文件时按 cache 的 rowItems 顺序）
    Source,
}

impl Default for AxisSort {
    fn default() -> Self {
        AxisSort::Pinyin
    }
}

/// 布局
#[derive(Clone, Debug, PartialEq)]
pub struct PivotLayout {
    /// true = 表格形式（每层行标签都重复），false = 压缩形式（只在组首显示）
    pub tabular: bool,
    /// 是否显示一级小计行（行字段 ≥ 2 时）
    pub subtotals: bool,
    /// 是否显示总计行
    pub grand_totals: bool,
    /// 标签排序口径
    pub sort: AxisSort,
}

impl Default for PivotLayout {
    fn default() -> Self {
        Self {
            tabular: true,
            subtotals: true,
            grand_totals: true,
            sort: AxisSort::Pinyin,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PivotSpec2 {
    pub source: (u32, u32, u32, u32),
    pub rows: Vec<AxisField>,
    pub cols: Vec<AxisField>,
    pub values: Vec<ValueField>,
    pub filters: Vec<FilterField>,
    pub layout: PivotLayout,
}

impl PivotSpec2 {
    /// 从旧单字段规格升级
    pub fn from_legacy(spec: &crate::pivot::PivotSpec) -> Self {
        Self {
            source: spec.source,
            rows: vec![AxisField { col: spec.row_field, group: spec.group.clone() }],
            cols: spec.col_field.map(|c| vec![AxisField::plain(c)]).unwrap_or_default(),
            values: vec![ValueField { col: spec.value_field, agg: spec.agg, label: None, show_as: ShowAs::Normal, base: None }],
            filters: Vec::new(),
            layout: PivotLayout::default(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PivotGrid {
    /// 表头块：每行等长（行字段名占左侧，列标签在右侧）
    pub headers: Vec<Vec<String>>,
    /// 数据行：左侧 `row_depth` 格是行标签，其余是值
    pub rows: Vec<Vec<Value>>,
    pub row_depth: usize,
    pub value_cols: usize,
    /// 列的叶子标签
    pub col_labels: Vec<String>,
    /// 列叶子的逐层路径（与 col_labels 一一对应，供 pivotTable 部件写嵌套 colItems）
    pub col_paths: Vec<Vec<String>>,
    pub grand_total_row: Option<usize>,
    /// 页筛选标题区：(字段名, 选中值)。Excel 会把它们渲染在透视表上方
    pub page_filters: Vec<(String, String)>,
}

#[derive(Clone, Copy, Debug, Default)]
struct Acc {
    count_nonempty: u64,
    sum: f64,
    min: Option<f64>,
    max: Option<f64>,
    numeric: u64,
}

impl Acc {
    #[inline]
    fn push_num(&mut self, n: f64) {
        self.count_nonempty += 1;
        self.numeric += 1;
        self.sum += n;
        self.min = Some(self.min.map_or(n, |m| m.min(n)));
        self.max = Some(self.max.map_or(n, |m| m.max(n)));
    }

    fn push(&mut self, v: Option<&Value>) {
        let Some(v) = v else { return };
        match v {
            Value::Empty => {}
            Value::Number(n) => self.push_num(*n),
            other => {
                other.with_display_str(|t| {
                    if t.is_empty() {
                        return;
                    }
                    self.count_nonempty += 1;
                    if let Ok(n) = t.parse::<f64>() {
                        self.numeric += 1;
                        self.sum += n;
                        self.min = Some(self.min.map_or(n, |m| m.min(n)));
                        self.max = Some(self.max.map_or(n, |m| m.max(n)));
                    }
                });
            }
        }
    }

    fn merge(&mut self, o: &Acc) {
        self.count_nonempty += o.count_nonempty;
        self.numeric += o.numeric;
        self.sum += o.sum;
        self.min = match (self.min, o.min) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        self.max = match (self.max, o.max) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
    }

    fn finish(&self, agg: Agg) -> Value {
        match agg {
            Agg::Count => Value::Number(self.count_nonempty as f64),
            Agg::Sum => Value::Number(self.sum),
            Agg::Avg => {
                if self.numeric == 0 {
                    Value::Text("#DIV/0!".into())
                } else {
                    Value::Number(self.sum / self.numeric as f64)
                }
            }
            Agg::Max => self.max.map(Value::Number).unwrap_or(Value::Empty),
            Agg::Min => self.min.map(Value::Number).unwrap_or(Value::Empty),
            Agg::CountNums | Agg::DistinctCount => Value::Number(self.numeric as f64),
            Agg::Product | Agg::StdDev | Agg::StdDevP | Agg::Var | Agg::VarP | Agg::Median => {
                Value::Number(self.sum)
            }
        }
    }
}

fn header(sheet: &Sheet, r: u32, c: u32) -> String {
    sheet
        .get(r, c)
        .map(|x| x.value.display())
        .unwrap_or_else(|| crate::model::col_label(c))
}

type CellMap = BTreeMap<Vec<String>, Vec<Acc>>;

fn emit_row(
    grid: &mut PivotGrid,
    rpath: &[String],
    m: Option<&CellMap>,
    slots: &[ColSlot],
    spec: &PivotSpec2,
    prev: &mut [String],
    total_cols: usize,
) {
    let mut row: Vec<Value> = Vec::with_capacity(total_cols);
    for i in 0..spec.rows.len() {
        let v = rpath.get(i).cloned().unwrap_or_default();
        let show = spec.layout.tabular || prev.get(i) != Some(&v);
        row.push(Value::Text(if show { v.clone() } else { String::new() }.into()));
        if i < prev.len() {
            prev[i] = v;
        }
    }
    for slot in slots {
        for vi in 0..spec.values.len() {
            let v = match (m, slot) {
                (Some(cells), ColSlot::Leaf(p)) => cells
                    .get(p)
                    .and_then(|accs| accs.get(vi))
                    .map(|a| a.finish(spec.values[vi].agg))
                    .unwrap_or(Value::Empty),
                (Some(cells), ColSlot::Subtotal { prefix, .. }) => {
                    let mut acc = Acc::default();
                    for (p, accs) in cells.iter() {
                        if p.starts_with(prefix) {
                            if let Some(a) = accs.get(vi) {
                                acc.merge(a);
                            }
                        }
                    }
                    acc.finish(spec.values[vi].agg)
                }
                (None, _) => Value::Empty,
            };
            row.push(v);
        }
    }
    // 右侧「…汇总」总计列先占位，调用方填值
    while row.len() < total_cols {
        row.push(Value::Empty);
    }
    grid.rows.push(row);
}

#[derive(Clone, PartialEq, Eq, Hash)]
enum PathIds {
    Inline { len: u8, ids: [u32; 4] },
    Heap(Box<[u32]>),
}

impl PathIds {
    #[inline]
    fn from_slice(s: &[u32]) -> Self {
        if s.len() <= 4 {
            let mut ids = [0u32; 4];
            ids[..s.len()].copy_from_slice(s);
            Self::Inline {
                len: s.len() as u8,
                ids,
            }
        } else {
            Self::Heap(s.into())
        }
    }

    #[inline]
    fn as_slice(&self) -> &[u32] {
        match self {
            Self::Inline { len, ids } => &ids[..*len as usize],
            Self::Heap(b) => b,
        }
    }

    #[inline]
    fn remap(&self, map: &[u32]) -> Self {
        match self {
            Self::Inline { len, ids } => {
                let n = *len as usize;
                let mut out = [0u32; 4];
                for i in 0..n {
                    out[i] = map[ids[i] as usize];
                }
                Self::Inline { len: *len, ids: out }
            }
            Self::Heap(b) => Self::Heap(b.iter().map(|&id| map[id as usize]).collect()),
        }
    }
}

struct PivotInterner {
    strings: Vec<String>,
    map: crate::sort_filter::FxHashMap<String, u32>,
}

impl PivotInterner {
    fn new() -> Self {
        let mut map: crate::sort_filter::FxHashMap<String, u32> = Default::default();
        map.insert(String::new(), 0);
        Self {
            strings: vec![String::new()],
            map,
        }
    }

    #[inline]
    fn intern_owned(&mut self, s: String) -> u32 {
        if s.is_empty() {
            return 0;
        }
        if let Some(&id) = self.map.get(&s) {
            return id;
        }
        let id = self.strings.len() as u32;
        self.strings.push(s.clone());
        self.map.insert(s, id);
        id
    }

    #[inline]
    fn intern_str(&mut self, s: &str) -> u32 {
        if s.is_empty() {
            return 0;
        }
        if let Some(&id) = self.map.get(s) {
            return id;
        }
        let owned = s.to_string();
        let id = self.strings.len() as u32;
        self.strings.push(owned.clone());
        self.map.insert(owned, id);
        id
    }
}

struct FieldEncoder<'a> {
    col: u32,
    group: &'a GroupBy,
    empty_id: u32,
    ptr_cache: crate::sort_filter::FxHashMap<usize, u32>,
    num_cache: crate::sort_filter::FxHashMap<u64, u32>,
}

impl<'a> FieldEncoder<'a> {
    fn new(col: u32, group: &'a GroupBy, interner: &mut PivotInterner) -> Self {
        let empty_id = interner.intern_owned(group_key(&Value::Empty, group));
        Self {
            col,
            group,
            empty_id,
            ptr_cache: Default::default(),
            num_cache: Default::default(),
        }
    }

    #[inline]
    fn encode_val(&mut self, v: &Value, interner: &mut PivotInterner) -> u32 {
        match v {
            Value::Empty => self.empty_id,
            Value::Text(s) => {
                let ptr = s.as_ptr() as usize;
                if let Some(&id) = self.ptr_cache.get(&ptr) {
                    return id;
                }
                let id = if matches!(self.group, GroupBy::None) {
                    interner.intern_str(s)
                } else {
                    interner.intern_owned(group_key(v, self.group))
                };
                self.ptr_cache.insert(ptr, id);
                id
            }
            Value::Number(n) => {
                let nb = if *n == 0.0 { 0u64 } else { n.to_bits() };
                if let Some(&id) = self.num_cache.get(&nb) {
                    return id;
                }
                let id = interner.intern_owned(group_key(v, self.group));
                self.num_cache.insert(nb, id);
                id
            }
            other => interner.intern_owned(group_key(other, self.group)),
        }
    }

    #[inline]
    fn encode_from_slab(
        &mut self,
        slab: Option<&crate::store::ColSlab>,
        dict_table: Option<&[u32; 129]>,
        ro: usize,
        interner: &mut PivotInterner,
    ) -> u32 {
        let Some(slab) = slab else {
            return self.empty_id;
        };
        if let Some(tbl) = dict_table
            && let crate::store::ColSlab::Values(vs) = slab
            && let Some((codes, _)) = vs.dict_text()
        {
            return if ro < codes.len() {
                tbl[codes[ro] as usize]
            } else {
                self.empty_id
            };
        }
        match slab {
            crate::store::ColSlab::Numbers(ns) => match ns.get(ro as u32) {
                None => self.empty_id,
                Some(n) => {
                    let nb = if n == 0.0 { 0u64 } else { n.to_bits() };
                    if let Some(&id) = self.num_cache.get(&nb) {
                        id
                    } else {
                        let id = interner.intern_owned(group_key(&Value::Number(n), self.group));
                        self.num_cache.insert(nb, id);
                        id
                    }
                }
            },
            crate::store::ColSlab::Values(vs) => match vs.get_val(ro) {
                None => self.empty_id,
                Some(v) => self.encode_val(v, interner),
            },
            crate::store::ColSlab::Mixed(cs) => match cs.get_cell(ro) {
                None => self.empty_id,
                Some(cell) => self.encode_val(&cell.value, interner),
            },
        }
    }
}

struct PivotChunkResult {
    strings: Vec<String>,
    row_mins: crate::sort_filter::FxHashMap<PathIds, u32>,
    col_mins: crate::sort_filter::FxHashMap<PathIds, u32>,
    cells: crate::sort_filter::FxHashMap<(PathIds, PathIds), Vec<Acc>>,
}

fn aggregate_pivot_pages(
    sheet: &Sheet,
    spec: &PivotSpec2,
    compiled_filters: &[(u32, crate::sort_filter::FxHashSet<&str>, bool)],
    empty_passes_filters: bool,
    first_row: u32,
    last_row: u32,
    p_lo: u32,
    p_hi: u32,
) -> PivotChunkResult {
    let mut interner = PivotInterner::new();
    let mut row_encs: Vec<FieldEncoder<'_>> = spec
        .rows
        .iter()
        .map(|f| FieldEncoder::new(f.col, &f.group, &mut interner))
        .collect();
    let mut col_encs: Vec<FieldEncoder<'_>> = spec
        .cols
        .iter()
        .map(|f| FieldEncoder::new(f.col, &f.group, &mut interner))
        .collect();

    let empty_row_ids: Vec<u32> = row_encs.iter().map(|e| e.empty_id).collect();
    let empty_col_ids: Vec<u32> = col_encs.iter().map(|e| e.empty_id).collect();
    let empty_row_path = PathIds::from_slice(&empty_row_ids);
    let empty_col_path = PathIds::from_slice(&empty_col_ids);

    let mut row_mins: crate::sort_filter::FxHashMap<PathIds, u32> = Default::default();
    let mut col_mins: crate::sort_filter::FxHashMap<PathIds, u32> = Default::default();
    let mut cells: crate::sort_filter::FxHashMap<(PathIds, PathIds), Vec<Acc>> = Default::default();
    let n_vals = spec.values.len();
    let mut min_empty_row: Option<u32> = None;

    let mut row_buf = vec![0u32; row_encs.len()];
    let mut col_buf = vec![0u32; col_encs.len()];
    let mut row_dict_tables: Vec<Option<[u32; 129]>> = vec![None; row_encs.len()];
    let mut col_dict_tables: Vec<Option<[u32; 129]>> = vec![None; col_encs.len()];
    let mut filter_dict_tables: Vec<Option<[bool; 129]>> = vec![None; compiled_filters.len()];

    for pi in p_lo..=p_hi {
        let base = pi * crate::store::PAGE_ROWS;
        let r_start = first_row.max(base);
        let r_end = last_row.min(base + crate::store::PAGE_ROWS - 1);
        if r_start > r_end {
            continue;
        }
        let Some(page) = sheet.cells().page(pi) else {
            if empty_passes_filters {
                min_empty_row = Some(min_empty_row.map_or(r_start, |m| m.min(r_start)));
            }
            continue;
        };

        let lo = (r_start - base) as usize;
        let hi = (r_end - base) as usize;
        let start_w = lo / 64;
        let end_w = hi / 64;
        let mut range_mask = [0u64; 4];
        for (w, slot) in range_mask.iter_mut().enumerate().take(end_w + 1).skip(start_w) {
            let mut m = !0u64;
            if w == start_w {
                m &= !0u64 << (lo % 64);
            }
            if w == end_w && (hi % 64) < 63 {
                m &= (1u64 << ((hi % 64) + 1)) - 1;
            }
            *slot = m;
        }

        let filter_slabs: Vec<Option<&crate::store::ColSlab>> = compiled_filters
            .iter()
            .map(|(c, _, _)| page.cols.get(*c).map(|a| a.as_ref()))
            .collect();
        // Fast page skip if a required non-blank filter column is completely absent on this page!
        let mut impossible_page = false;
        for (fi, &(_, ref allowed, empty_ok)) in compiled_filters.iter().enumerate() {
            filter_dict_tables[fi] = None;
            match filter_slabs[fi] {
                None if !empty_ok => {
                    impossible_page = true;
                    break;
                }
                Some(slab) if !empty_ok && !slab.has_any_in(lo, hi + 1) => {
                    impossible_page = true;
                    break;
                }
                Some(crate::store::ColSlab::Values(vs)) => {
                    if let Some((_, dict)) = vs.dict_text() {
                        let mut tbl = [false; 129];
                        tbl[0] = empty_ok;
                        let mut any_match = empty_ok;
                        for (code, item) in dict.iter().enumerate().skip(1) {
                            let ok = match item {
                                Value::Text(s) => allowed.contains(s.as_ref()),
                                other => other.with_display_str(|s| allowed.contains(s)),
                            };
                            tbl[code] = ok;
                            any_match |= ok;
                        }
                        if !any_match {
                            impossible_page = true;
                            break;
                        }
                        filter_dict_tables[fi] = Some(tbl);
                    }
                }
                _ => {}
            }
        }
        if impossible_page {
            continue;
        }

        let row_slabs: Vec<Option<&crate::store::ColSlab>> = row_encs
            .iter()
            .map(|e| page.cols.get(e.col).map(|a| a.as_ref()))
            .collect();
        let col_slabs: Vec<Option<&crate::store::ColSlab>> = col_encs
            .iter()
            .map(|e| page.cols.get(e.col).map(|a| a.as_ref()))
            .collect();
        let val_slabs: Vec<Option<&crate::store::ColSlab>> = spec
            .values
            .iter()
            .map(|v| page.cols.get(v.col).map(|a| a.as_ref()))
            .collect();

        // Compute union occupancy mask across all referenced columns on this page.
        let mut union_mask = [0u64; 4];
        for slab in filter_slabs
            .iter()
            .chain(row_slabs.iter())
            .chain(col_slabs.iter())
            .chain(val_slabs.iter())
            .flatten()
        {
            let m = slab.row_mask();
            union_mask[0] |= m[0];
            union_mask[1] |= m[1];
            union_mask[2] |= m[2];
            union_mask[3] |= m[3];
        }

        // Handle all completely blank rows in `lo..=hi` in O(1)!
        if empty_passes_filters {
            for w in start_w..=end_w {
                let blank_bits = (!union_mask[w]) & range_mask[w];
                if blank_bits != 0 {
                    let first_blank_ro = w * 64 + blank_bits.trailing_zeros() as usize;
                    let blank_r = base + first_blank_ro as u32;
                    min_empty_row = Some(min_empty_row.map_or(blank_r, |m| m.min(blank_r)));
                    break;
                }
            }
        }

        let active_mask = [
            union_mask[0] & range_mask[0],
            union_mask[1] & range_mask[1],
            union_mask[2] & range_mask[2],
            union_mask[3] & range_mask[3],
        ];
        if active_mask == [0; 4] {
            continue;
        }

        // Pre-map `DictText` slabs once per 256-row page for row & col fields!
        for (i, enc) in row_encs.iter_mut().enumerate() {
            row_dict_tables[i] = if let Some(crate::store::ColSlab::Values(vs)) = row_slabs[i]
                && let Some((_, dict)) = vs.dict_text()
            {
                let mut tbl = [enc.empty_id; 129];
                for (code, item) in dict.iter().enumerate().skip(1) {
                    tbl[code] = enc.encode_val(item, &mut interner);
                }
                Some(tbl)
            } else {
                None
            };
        }
        for (i, enc) in col_encs.iter_mut().enumerate() {
            col_dict_tables[i] = if let Some(crate::store::ColSlab::Values(vs)) = col_slabs[i]
                && let Some((_, dict)) = vs.dict_text()
            {
                let mut tbl = [enc.empty_id; 129];
                for (code, item) in dict.iter().enumerate().skip(1) {
                    tbl[code] = enc.encode_val(item, &mut interner);
                }
                Some(tbl)
            } else {
                None
            };
        }

        for (w, &word_bits) in active_mask.iter().enumerate().take(end_w + 1).skip(start_w) {
            let mut bits = word_bits;
            while bits != 0 {
                let tz = bits.trailing_zeros() as usize;
                bits &= bits - 1;
                let ro = w * 64 + tz;
                let r = base + ro as u32;

                // 1. Check filters
                let mut keep = true;
                for (fi, &(_, ref allowed, empty_ok)) in compiled_filters.iter().enumerate() {
                    let ok = match filter_slabs[fi] {
                        None => empty_ok,
                        Some(slab) => {
                            if let Some(ref tbl) = filter_dict_tables[fi]
                                && let crate::store::ColSlab::Values(vs) = slab
                                && let Some((codes, _)) = vs.dict_text()
                            {
                                if ro < codes.len() {
                                    tbl[codes[ro] as usize]
                                } else {
                                    empty_ok
                                }
                            } else {
                                match slab.value_ref(ro as u32) {
                                    None => empty_ok,
                                    Some(v) => match v.as_ref() {
                                        Value::Empty => empty_ok,
                                        Value::Text(t) => allowed.contains(t.as_ref()),
                                        other => other.with_display_str(|s| allowed.contains(s)),
                                    },
                                }
                            }
                        }
                    };
                    if !ok {
                        keep = false;
                        break;
                    }
                }
                if !keep {
                    continue;
                }

                // 2. Encode row & col path IDs without allocating Strings
                for (i, enc) in row_encs.iter_mut().enumerate() {
                    row_buf[i] = enc.encode_from_slab(
                        row_slabs[i],
                        row_dict_tables[i].as_ref(),
                        ro,
                        &mut interner,
                    );
                }
                for (i, enc) in col_encs.iter_mut().enumerate() {
                    col_buf[i] = enc.encode_from_slab(
                        col_slabs[i],
                        col_dict_tables[i].as_ref(),
                        ro,
                        &mut interner,
                    );
                }
                let rpath = PathIds::from_slice(&row_buf);
                let cpath = PathIds::from_slice(&col_buf);

                row_mins
                    .entry(rpath.clone())
                    .and_modify(|m| *m = (*m).min(r))
                    .or_insert(r);
                col_mins
                    .entry(cpath.clone())
                    .and_modify(|m| *m = (*m).min(r))
                    .or_insert(r);

                let accs = cells
                    .entry((rpath, cpath))
                    .or_insert_with(|| vec![Acc::default(); n_vals]);
                for (vi, slab_opt) in val_slabs.iter().enumerate() {
                    let Some(slab) = *slab_opt else {
                        continue;
                    };
                    match slab {
                        crate::store::ColSlab::Numbers(ns) => {
                            if let Some(n) = ns.get(ro as u32) {
                                accs[vi].push_num(n);
                            }
                        }
                        crate::store::ColSlab::Values(vs) => {
                            if let Some(v) = vs.get_val(ro) {
                                accs[vi].push(Some(v));
                            }
                        }
                        crate::store::ColSlab::Mixed(cs) => {
                            if let Some(cell) = cs.get_cell(ro) {
                                accs[vi].push(Some(&cell.value));
                            }
                        }
                    }
                }
            }
        }
    }

    if let Some(r) = min_empty_row {
        row_mins
            .entry(empty_row_path.clone())
            .and_modify(|m| *m = (*m).min(r))
            .or_insert(r);
        col_mins
            .entry(empty_col_path.clone())
            .and_modify(|m| *m = (*m).min(r))
            .or_insert(r);
        cells
            .entry((empty_row_path, empty_col_path))
            .or_insert_with(|| vec![Acc::default(); n_vals]);
    }

    PivotChunkResult {
        strings: interner.strings,
        row_mins,
        col_mins,
        cells,
    }
}

/// 计算多字段透视
pub fn compute(sheet: &Sheet, spec: &PivotSpec2) -> PivotGrid {
    let (r1, _c1, r2, _c2) = spec.source;
    let hdr = |c: u32| header(sheet, r1, c);
    let page_filters: Vec<(String, String)> = spec
        .filters
        .iter()
        .filter(|f| !f.keep.is_empty())
        .map(|f| (hdr(f.col), f.keep.join(", ")))
        .collect();
    let mut grid = PivotGrid {
        page_filters,
        row_depth: spec.rows.len(),
        value_cols: spec.values.len(),
        ..Default::default()
    };

    // 预编译筛选允许集：O(1) FxHashSet 匹配替代逐行 Vec 线性扫描
    let compiled_filters: Vec<(u32, crate::sort_filter::FxHashSet<&str>, bool)> = spec
        .filters
        .iter()
        .filter(|f| !f.keep.is_empty())
        .map(|f| {
            let set: crate::sort_filter::FxHashSet<&str> =
                f.keep.iter().map(String::as_str).collect();
            let empty_ok = set.contains("");
            (f.col, set, empty_ok)
        })
        .collect();
    let empty_passes_filters = compiled_filters.iter().all(|&(_, _, empty_ok)| empty_ok);

    // 收集：行路径 → 列路径 → 每个值字段的累计（支持 256 行页字典编码 + 大表多线程分片归并）
    let mut data: BTreeMap<Vec<String>, CellMap> = BTreeMap::new();
    let mut row_first: std::collections::HashMap<Vec<String>, usize> = Default::default();
    let mut col_first: std::collections::HashMap<Vec<String>, usize> = Default::default();

    let first_row = r1.saturating_add(1);
    if first_row <= r2 {
        let p_first = first_row / crate::store::PAGE_ROWS;
        let p_last = r2 / crate::store::PAGE_ROWS;
        let page_span = (p_last - p_first + 1) as usize;
        let n_threads = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
            .min(8);

        let chunks: Vec<PivotChunkResult> = if (r2 - first_row + 1) >= 8_192
            && page_span >= 16
            && n_threads > 1
        {
            let pages_per_worker = page_span.div_ceil(n_threads) as u32;
            std::thread::scope(|scope| {
                let mut handles = Vec::new();
                let mut cur_p = p_first;
                while cur_p <= p_last {
                    let end_p = (cur_p + pages_per_worker - 1).min(p_last);
                    let filt_ref = &compiled_filters;
                    handles.push(scope.spawn(move || {
                        aggregate_pivot_pages(
                            sheet,
                            spec,
                            filt_ref,
                            empty_passes_filters,
                            first_row,
                            r2,
                            cur_p,
                            end_p,
                        )
                    }));
                    cur_p = end_p + 1;
                }
                handles.into_iter().map(|h| h.join().unwrap()).collect()
            })
        } else {
            vec![aggregate_pivot_pages(
                sheet,
                spec,
                &compiled_filters,
                empty_passes_filters,
                first_row,
                r2,
                p_first,
                p_last,
            )]
        };

        let mut global_interner = PivotInterner::new();
        let mut global_row_min: crate::sort_filter::FxHashMap<PathIds, u32> = Default::default();
        let mut global_col_min: crate::sort_filter::FxHashMap<PathIds, u32> = Default::default();
        let mut global_cells: crate::sort_filter::FxHashMap<(PathIds, PathIds), Vec<Acc>> =
            Default::default();

        for chunk in chunks {
            let mut id_remap = vec![0u32; chunk.strings.len()];
            for (local_id, s) in chunk.strings.into_iter().enumerate() {
                id_remap[local_id] = global_interner.intern_owned(s);
            }
            for (rp, min_r) in chunk.row_mins {
                let grp = rp.remap(&id_remap);
                global_row_min
                    .entry(grp)
                    .and_modify(|m| *m = (*m).min(min_r))
                    .or_insert(min_r);
            }
            for (cp, min_r) in chunk.col_mins {
                let gcp = cp.remap(&id_remap);
                global_col_min
                    .entry(gcp)
                    .and_modify(|m| *m = (*m).min(min_r))
                    .or_insert(min_r);
            }
            for ((rp, cp), accs) in chunk.cells {
                let key = (rp.remap(&id_remap), cp.remap(&id_remap));
                match global_cells.get_mut(&key) {
                    Some(dst) => {
                        for (d, s) in dst.iter_mut().zip(accs.iter()) {
                            d.merge(s);
                        }
                    }
                    None => {
                        global_cells.insert(key, accs);
                    }
                }
            }
        }

        let decode_path = |p: &PathIds, pool: &[String]| -> Vec<String> {
            p.as_slice().iter().map(|&id| pool[id as usize].clone()).collect()
        };

        // Reconstruct exact source appearance order (`min_r` ascending)
        let mut row_sorted: Vec<(u32, PathIds)> =
            global_row_min.into_iter().map(|(p, r)| (r, p)).collect();
        row_sorted.sort_unstable_by_key(|&(r, _)| r);
        let mut row_vec_cache: crate::sort_filter::FxHashMap<PathIds, Vec<String>> =
            Default::default();
        for (ord, (_, rp)) in row_sorted.into_iter().enumerate() {
            let vec_p = decode_path(&rp, &global_interner.strings);
            row_first.insert(vec_p.clone(), ord);
            row_vec_cache.insert(rp, vec_p);
        }

        let mut col_sorted: Vec<(u32, PathIds)> =
            global_col_min.into_iter().map(|(p, r)| (r, p)).collect();
        col_sorted.sort_unstable_by_key(|&(r, _)| r);
        let mut col_vec_cache: crate::sort_filter::FxHashMap<PathIds, Vec<String>> =
            Default::default();
        for (ord, (_, cp)) in col_sorted.into_iter().enumerate() {
            let vec_p = decode_path(&cp, &global_interner.strings);
            col_first.insert(vec_p.clone(), ord);
            col_vec_cache.insert(cp, vec_p);
        }

        for ((rp, cp), accs) in global_cells {
            let r_vec = row_vec_cache
                .get(&rp)
                .cloned()
                .unwrap_or_else(|| decode_path(&rp, &global_interner.strings));
            let c_vec = col_vec_cache
                .get(&cp)
                .cloned()
                .unwrap_or_else(|| decode_path(&cp, &global_interner.strings));
            data.entry(r_vec).or_default().insert(c_vec, accs);
        }
    }

    // 列叶子标签：直接从 col_first 提取唯一路径（消除 O(C²) Vec::contains），并预计算一次拼音排序键！
    let mut col_paths: Vec<Vec<String>> = match spec.layout.sort {
        AxisSort::Pinyin => {
            let mut decorated: Vec<(Vec<u8>, Vec<String>)> = col_first
                .keys()
                .map(|p| (crate::collate::sort_key(&p.join(" / ")), p.clone()))
                .collect();
            decorated.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
            decorated.into_iter().map(|(_, p)| p).collect()
        }
        AxisSort::Source => {
            let mut pairs: Vec<(usize, Vec<String>)> = col_first
                .iter()
                .map(|(p, &ord)| (ord, p.clone()))
                .collect();
            pairs.sort_unstable_by_key(|&(ord, _)| ord);
            pairs.into_iter().map(|(_, p)| p).collect()
        }
    };
    if col_paths.is_empty() && spec.cols.is_empty() {
        col_paths.push(Vec::new());
    }
    for p in &col_paths {
        grid.col_labels.push(if p.is_empty() { String::new() } else { p.join(" / ") });
    }
    grid.col_paths = col_paths.clone();
    let slots = build_col_slots(&col_paths, spec.layout.subtotals);

    let n_head = if spec.cols.is_empty() {
        1
    } else if spec.cols.len() >= 2 {
        // Excel：值名 + 列字段名同行 / 上层标签 + 汇总 / 叶子标签
        3
    } else {
        spec.cols.len() + 2
    };
    // Excel 在有列字段且显示总计列时，右侧给每个值字段各留一列「<值名>汇总」
    let grand_block = if !spec.cols.is_empty() && spec.layout.grand_totals {
        spec.values.len()
    } else {
        0
    };
    let grand_base = spec.rows.len() + slots.len() * spec.values.len();
    let total_cols = grand_base + grand_block;
    let value_label = |vi: usize| -> String {
        let v = &spec.values[vi];
        v.label
            .clone()
            .unwrap_or_else(|| format!("{} {}", hdr(v.col), v.agg.label()))
    };
    for hi in 0..n_head {
        let mut row = vec![String::new(); total_cols];
        if spec.cols.is_empty() {
            // 单行表头：行字段名 + 值名
            if hi == 0 {
                for (i, f) in spec.rows.iter().enumerate() {
                    if i < total_cols {
                        row[i] = hdr(f.col);
                    }
                }
            }
            for (ci, slot) in slots.iter().enumerate() {
                if matches!(slot, ColSlot::Leaf(_)) {
                    for vi in 0..spec.values.len() {
                        row[spec.rows.len() + ci * spec.values.len() + vi] = value_label(vi);
                    }
                }
            }
        } else if spec.cols.len() >= 2 {
            // Excel 的两层列字段版式（3 行）
            if hi == 0 {
                row[0] = value_label(0);
                for (d, f) in spec.cols.iter().enumerate() {
                    if spec.rows.len() + d < total_cols {
                        row[spec.rows.len() + d] = hdr(f.col);
                    }
                }
            } else if hi == 1 {
                let mut prev_top = String::new();
                for (ci, slot) in slots.iter().enumerate() {
                    let cell = spec.rows.len() + ci * spec.values.len();
                    match slot {
                        ColSlot::Leaf(p) => {
                            let top = p.first().cloned().unwrap_or_default();
                            if top != prev_top {
                                row[cell] = top.clone();
                                prev_top = top;
                            }
                        }
                        ColSlot::Subtotal { label, .. } => row[cell] = label.clone(),
                    }
                }
                if grand_block > 0 {
                    row[grand_base] = "总计".to_string();
                }
            } else {
                for (i, f) in spec.rows.iter().enumerate() {
                    row[i] = hdr(f.col);
                }
                for (ci, slot) in slots.iter().enumerate() {
                    if let ColSlot::Leaf(p) = slot {
                        row[spec.rows.len() + ci * spec.values.len()] =
                            p.last().cloned().unwrap_or_default();
                    }
                }
            }
        } else if hi == 0 {
            // 第一行：列字段名 + 「值」（总计列的表头）
            row[spec.rows.len()] = hdr(spec.cols[0].col);
            if grand_block > 0 {
                row[grand_base] = "值".to_string();
            }
        } else if hi + 1 == n_head {
            // 最后一行：行字段名 + 每个值槽的值名
            for (i, f) in spec.rows.iter().enumerate() {
                row[i] = hdr(f.col);
            }
            for (ci, _p) in slots.iter().enumerate() {
                for vi in 0..spec.values.len() {
                    row[spec.rows.len() + ci * spec.values.len() + vi] = value_label(vi);
                }
            }
        } else {
            // 中间行：列标签逐层；紧邻字段名行的那一行右侧放「…汇总」
            let lvl = hi - 1;
            for (ci, slot) in slots.iter().enumerate() {
                match slot {
                    ColSlot::Leaf(p) => {
                        if lvl < p.len() {
                            row[spec.rows.len() + ci * spec.values.len()] = p[lvl].clone();
                        }
                    }
                    ColSlot::Subtotal { label, .. } if lvl + 1 == spec.cols.len() => {
                        row[spec.rows.len() + ci * spec.values.len()] = label.clone();
                    }
                    ColSlot::Subtotal { .. } => {}
                }
            }
            if hi == 1 && grand_block > 0 {
                for vi in 0..spec.values.len() {
                    row[grand_base + vi] = format!("{}汇总", value_label(vi));
                }
            }
        }
        grid.headers.push(row);
    }

    // 逐组输出 + 小计 + 总计（预计算一次拼音排序键，移出 sort_by 闭包！）
    let mut prev: Vec<String> = vec![String::new(); spec.rows.len()];
    let keys: Vec<Vec<String>> = match spec.layout.sort {
        AxisSort::Pinyin => {
            let mut decorated: Vec<(Vec<u8>, Vec<String>)> = data
                .keys()
                .map(|k| (crate::collate::sort_key(&k.join(" / ")), k.clone()))
                .collect();
            decorated.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
            decorated.into_iter().map(|(_, k)| k).collect()
        }
        AxisSort::Source => {
            let mut ks: Vec<Vec<String>> = data.keys().cloned().collect();
            ks.sort_by_key(|k| row_first.get(k).copied().unwrap_or(usize::MAX));
            ks
        }
    };
    let mut grand: Vec<Vec<Acc>> = vec![vec![Acc::default(); spec.values.len()]; slots.len()];
    let mut i = 0;
    while i < keys.len() {
        let top = keys[i].first().cloned().unwrap_or_default();
        let start = i;
        while i < keys.len() && keys[i].first().cloned().unwrap_or_default() == top {
            i += 1;
        }
        let mut sub: Vec<Vec<Acc>> = vec![vec![Acc::default(); spec.values.len()]; slots.len()];
        for k in &keys[start..i] {
            let m = &data[k];
            emit_row(&mut grid, k, Some(m), &slots, spec, &mut prev, total_cols);
            if grand_block > 0 {
                let last = grid.rows.len() - 1;
                for vi in 0..spec.values.len() {
                    let mut acc = Acc::default();
                    for (p, accs) in m.iter() {
                        if vi < accs.len() {
                            let _ = p;
                            acc.merge(&accs[vi]);
                        }
                    }
                    grid.rows[last][grand_base + vi] = acc.finish(spec.values[vi].agg);
                }
            }
            for (ci, slot) in slots.iter().enumerate() {
                if let ColSlot::Leaf(p) = slot {
                    if let Some(accs) = m.get(p) {
                        for vi in 0..spec.values.len().min(accs.len()) {
                            sub[ci][vi].merge(&accs[vi]);
                            grand[ci][vi].merge(&accs[vi]);
                        }
                    }
                }
            }
        }
        if spec.layout.subtotals && spec.rows.len() >= 2 {
            let mut srow = vec![Value::Text(String::new().into()); total_cols];
            if !srow.is_empty() {
                srow[0] = Value::Text(format!("{top} 汇总").into());
            }
            for ci in 0..slots.len() {
                for vi in 0..spec.values.len() {
                    srow[spec.rows.len() + ci * spec.values.len() + vi] =
                        sub[ci][vi].finish(spec.values[vi].agg);
                }
            }
            if grand_block > 0 {
                for vi in 0..spec.values.len() {
                    let mut acc = Acc::default();
                    for ci in 0..col_paths.len() {
                        acc.merge(&sub[ci][vi]);
                    }
                    srow[grand_base + vi] = acc.finish(spec.values[vi].agg);
                }
            }
            grid.rows.push(srow);
            prev = vec![String::new(); spec.rows.len()];
        }
    }
    if spec.layout.grand_totals && !grid.rows.is_empty() {
        let mut grow = vec![Value::Text(String::new().into()); total_cols];
        if !grow.is_empty() {
            grow[0] = Value::Text("总计".into());
        }
        fill_subtotals(&slots, &mut grand);
        for ci in 0..slots.len() {
            for vi in 0..spec.values.len() {
                grow[spec.rows.len() + ci * spec.values.len() + vi] =
                    grand[ci][vi].finish(spec.values[vi].agg);
            }
        }
        if grand_block > 0 {
            for vi in 0..spec.values.len() {
                let mut acc = Acc::default();
                for ci in 0..slots.len() {
                    if matches!(slots[ci], ColSlot::Leaf(_)) {
                        acc.merge(&grand[ci][vi]);
                    }
                }
                grow[grand_base + vi] = acc.finish(spec.values[vi].agg);
            }
        }
        grid.grand_total_row = Some(grid.rows.len());
        grid.rows.push(grow);
    }
    apply_show_as(&mut grid, spec);
    grid
}

/// 数字取值（非数字按 0）
fn as_num(v: &Value) -> f64 {
    match *v {
        Value::Number(n) => n,
        _ => 0.0,
    }
}

/// 分母为 0 时留空，与 Excel 一致
fn ratio(v: f64, denom: f64) -> Value {
    if denom.abs() > f64::EPSILON {
        Value::Number(v / denom)
    } else {
        Value::Empty
    }
}

/// 按 Excel 的 showDataAs 语义把聚合值换成比值（存 0.6 而不是 60，格式靠 numFmtId=10）
fn apply_show_as(grid: &mut PivotGrid, spec: &PivotSpec2) {
    let rd = spec.rows.len();
    let vc = spec.values.len();
    if vc == 0 {
        return;
    }
    let ncols = grid.col_labels.len();
    for vi in 0..vc {
        let mode = spec.values[vi].show_as;
        if mode == ShowAs::Normal {
            continue;
        }
        let mut col_tot = vec![0.0f64; ncols];
        if let Some(g) = grid.grand_total_row {
            for c in 0..ncols {
                col_tot[c] = as_num(&grid.rows[g][rd + c * vc + vi]);
            }
        }
        let total: f64 = col_tot.iter().sum();
        for r in 0..grid.rows.len() {
            let raw: Vec<f64> = (0..ncols)
                .map(|c| as_num(&grid.rows[r][rd + c * vc + vi]))
                .collect();
            let row_sum: f64 = raw.iter().sum();
            let mut prev: Option<f64> = None;
            for c in 0..ncols {
                let idx = rd + c * vc + vi;
                let v = raw[c];
                let new = match mode {
                    ShowAs::PercentOfRow => ratio(v, row_sum),
                    ShowAs::PercentOfColumn => ratio(v, col_tot[c]),
                    ShowAs::PercentOfTotal => ratio(v, total),
                    // 上一项差 / 上一项差% / 索引：第一列没有「上一项」，按 Excel 留空
                    ShowAs::DifferencePrev => match prev {
                        Some(p) => Value::Number(v - p),
                        None => Value::Empty,
                    },
                    ShowAs::PercentDifferencePrev => match prev {
                        Some(p) => ratio(v - p, p.abs()),
                        None => Value::Empty,
                    },
                    ShowAs::Index => ratio(v * total, row_sum * col_tot[c]),
                    ShowAs::Normal
                    | ShowAs::PercentOfParentRow
                    | ShowAs::PercentOfParentCol
                    | ShowAs::RunningTotal
                    | ShowAs::PercentRunningTotal
                    | ShowAs::RankDesc
                    | ShowAs::RankAsc => Value::Number(v),
                };
                grid.rows[r][idx] = new;
                prev = Some(v);
            }
        }
    }
}

/// 把网格写进工作表（从 at 开始），返回占用区域
pub fn write_grid(sheet: &mut Sheet, at: (u32, u32), grid: &PivotGrid) -> (u32, u32, u32, u32) {
    let (r0, c0) = at;
    let mut r = r0;
    for (i, (name, val)) in grid.page_filters.iter().enumerate() {
        sheet.set(
            r + i as u32,
            c0,
            crate::model::Cell { value: Value::Text(name.clone().into()), formula: None },
        );
        sheet.set(
            r + i as u32,
            c0 + 1,
            crate::model::Cell { value: Value::Text(val.clone().into()), formula: None },
        );
    }
    if !grid.page_filters.is_empty() {
        r += grid.page_filters.len() as u32 + 1; // 标题区 + 一空行
    }
    for h in &grid.headers {
        for (i, s) in h.iter().enumerate() {
            if !s.is_empty() {
                sheet.set(
                    r,
                    c0 + i as u32,
                    crate::model::Cell { value: Value::Text(s.clone().into()), formula: None },
                );
            }
        }
        r += 1;
    }
    for row in &grid.rows {
        for (i, v) in row.iter().enumerate() {
            if !matches!(v, Value::Empty) {
                sheet.set(r, c0 + i as u32, crate::model::Cell { value: v.clone(), formula: None });
            }
        }
        r += 1;
    }
    let cols = grid
        .headers
        .iter()
        .map(|h| h.len())
        .chain(grid.rows.iter().map(|x| x.len()))
        .max()
        .unwrap_or(0) as u32;
    (r0, c0, r.saturating_sub(1), c0 + cols.saturating_sub(1))
}

/// 列方向上的一个槽位：列叶子，或某个上层项的子总计列
#[derive(Clone, Debug, PartialEq)]
pub enum ColSlot {
    Leaf(Vec<String>),
    Subtotal { prefix: Vec<String>, label: String },
}

/// 由列叶子路径生成槽位表：列字段 ≥2 层且开小计时，每个上层项后插一列「<项> 汇总」。
///
/// Excel 的两层列字段就是这种排法（`A 无效 | A 有效 | A 汇总 | B 无效 | B 有效 | B 汇总`）。
pub fn build_col_slots(col_paths: &[Vec<String>], subtotals: bool) -> Vec<ColSlot> {
    let depth = col_paths.first().map(|p| p.len()).unwrap_or(0);
    if !subtotals || depth < 2 {
        return col_paths.iter().cloned().map(ColSlot::Leaf).collect();
    }
    let mut out: Vec<ColSlot> = Vec::new();
    let mut i = 0;
    while i < col_paths.len() {
        let top = col_paths[i][0].clone();
        out.push(ColSlot::Leaf(col_paths[i].clone()));
        i += 1;
        while i < col_paths.len() && col_paths[i][0] == top {
            out.push(ColSlot::Leaf(col_paths[i].clone()));
            i += 1;
        }
        out.push(ColSlot::Subtotal { prefix: vec![top.clone()], label: format!("{top} 汇总") });
    }
    out
}

/// 子总计槽的累计值 = 该上层前缀下所有叶子的合计
fn fill_subtotals(slots: &[ColSlot], acc: &mut [Vec<Acc>]) {
    for (ci, slot) in slots.iter().enumerate() {
        if let ColSlot::Subtotal { prefix, .. } = slot {
            for (li, leaf) in slots.iter().enumerate() {
                if let ColSlot::Leaf(p) = leaf {
                    if p.starts_with(prefix) && li != ci {
                        for vi in 0..acc[ci].len() {
                            let v = acc[li][vi].clone();
                            acc[ci][vi].merge(&v);
                        }
                    }
                }
            }
        }
    }
}

/// 字段面板里可调序的轴
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Axis {
    Rows,
    Cols,
}

/// 面板上的 ↑/↓：交换同一轴内两个字段的位置（即改变层级），越界返回 false。
pub fn move_axis(spec: &mut PivotSpec2, axis: Axis, i: usize, delta: isize) -> bool {
    let v = match axis {
        Axis::Rows => &mut spec.rows,
        Axis::Cols => &mut spec.cols,
    };
    let j = i as isize + delta;
    if j < 0 || j as usize >= v.len() {
        return false;
    }
    v.swap(i, j as usize);
    true
}

/// 计算项：在网格里追加一行「若干行标签之和」（Excel 的计算项就是这么落地的：
/// rowItems 与数据区都要多一行，只改 XML 定义会让 Excel 判文件坏）。
///
/// 返回新行在 `rows` 里的下标；找不到任何来源行时返回 None。
/// 计算项表达式里的运算符。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ItemOp {
    Add,
    Sub,
    Mul,
    Div,
}

/// 按表达式算一行计算项（支持 + - * /，先乘除后加减）。
///
/// `terms` 第一项之前的运算符忽略。任一来源项找不到就返回 None，不改网格。
pub fn append_calc_item_expr(
    grid: &mut PivotGrid,
    name: &str,
    terms: &[(ItemOp, String)],
) -> Option<usize> {
    let rd = grid.row_depth.max(1);
    // 两层行字段时同名项既有明细行也有「<项> 汇总」行：优先取汇总行，
    // 否则「华东*2」只会算出第一条明细（Excel 里计算项是对整组求和）。
    let find = |item: &str| -> Option<usize> {
        let sub_label = format!("{item} 汇总");
        let is_live = |r: usize, _row: &Vec<Value>| Some(r) != grid.grand_total_row;
        if let Some(r) = grid.rows.iter().enumerate().position(|(r, row)| {
            is_live(r, row) && row.first().is_some_and(|v| v.display() == sub_label)
        }) {
            return Some(r);
        }
        grid.rows.iter().enumerate().position(|(r, row)| {
            is_live(r, row) && row.first().is_some_and(|v| v.display() == item)
        })
    };
    let rows: Vec<usize> = terms.iter().map(|(_, it)| find(it)).collect::<Option<Vec<_>>>()?;
    if rows.is_empty() {
        return None;
    }
    let width = grid.rows.first().map(|r| r.len()).unwrap_or(0);
    let mut line = vec![Value::Empty; width];
    if !line.is_empty() {
        line[0] = Value::Text(name.into());
    }
    for c in rd..width {
        let vals: Vec<Option<f64>> = rows
            .iter()
            .map(|&r| match grid.rows[r][c] {
                Value::Number(n) => Some(n),
                _ => None,
            })
            .collect();
        if vals.iter().all(|v| v.is_none()) {
            continue;
        }
        // 第一遍：乘除合并（缺失值当 0 处理，除零保留 0 避免 NaN 落盘）
        let mut folded: Vec<(ItemOp, f64)> = Vec::new();
        for (i, (op, _)) in terms.iter().enumerate() {
            let v = vals[i].unwrap_or(0.0);
            if i > 0 && matches!(op, ItemOp::Mul | ItemOp::Div) {
                if let Some(last) = folded.last_mut() {
                    last.1 = if matches!(op, ItemOp::Mul) {
                        last.1 * v
                    } else if v == 0.0 {
                        0.0
                    } else {
                        last.1 / v
                    };
                    continue;
                }
            }
            folded.push((*op, v));
        }
        let mut acc = folded[0].1;
        for (op, v) in folded.iter().skip(1) {
            acc = match op {
                ItemOp::Sub => acc - v,
                _ => acc + v,
            };
        }
        line[c] = Value::Number(acc);
    }
    let at = grid.grand_total_row.unwrap_or(grid.rows.len());
    grid.rows.insert(at, line);
    if let Some(g) = grid.grand_total_row.as_mut() {
        *g += 1;
    }
    Some(at)
}

pub fn append_calc_item_row(grid: &mut PivotGrid, name: &str, sources: &[&str]) -> Option<usize> {
    let rd = grid.row_depth.max(1);
    let idxs: Vec<usize> = grid
        .rows
        .iter()
        .enumerate()
        .filter(|(r, row)| {
            Some(*r) != grid.grand_total_row
                && row
                    .first()
                    .is_some_and(|v| sources.iter().any(|s| v.display() == **s))
        })
        .map(|(r, _)| r)
        .collect();
    if idxs.is_empty() {
        return None;
    }
    let width = grid.rows.first().map(|r| r.len()).unwrap_or(0);
    let mut line = vec![Value::Empty; width];
    if !line.is_empty() {
        line[0] = Value::Text(name.into());
    }
    for c in rd..width {
        let mut sum = 0.0;
        let mut any = false;
        for &r in &idxs {
            if let Value::Number(n) = grid.rows[r][c] {
                sum += n;
                any = true;
            }
        }
        if any {
            line[c] = Value::Number(sum);
        }
    }
    // 插在总计行之前（Excel 里计算项也是排在普通项之后、总计之前）
    let at = grid.grand_total_row.unwrap_or(grid.rows.len());
    grid.rows.insert(at, line);
    if let Some(g) = grid.grand_total_row.as_mut() {
        *g += 1;
    }
    Some(at)
}


#[cfg(test)]
mod wave5_pivot_multi_tests {
    use super::*;
    use crate::model::{Cell, Sheet, Value};

    #[test]
    fn parallel_dict_encoded_pivot_multi_matches_sequential_totals_and_source_order() {
        let mut sh = Sheet::new("BigPivot");
        for (c, h) in ["区域", "品类", "渠道", "销量"].iter().enumerate() {
            sh.set(0, c as u32, Cell { value: Value::Text((*h).into()), formula: None });
        }
        let regions = ["华南", "华东", "华北", "西南"];
        let cats = ["A类", "B类", "C类"];
        let channels = ["线上", "线下"];
        // 10,000 rows (>= 8,192 parallel threshold, ~40 pages >= 16)
        for r in 1..=10_000u32 {
            sh.set(r, 0, Cell { value: Value::Text(regions[(r as usize) % 4].into()), formula: None });
            sh.set(r, 1, Cell { value: Value::Text(cats[(r as usize) % 3].into()), formula: None });
            sh.set(r, 2, Cell { value: Value::Text(channels[(r as usize) % 2].into()), formula: None });
            sh.set(r, 3, Cell { value: Value::Number(10.0), formula: None });
        }
        let mut spec = PivotSpec2 {
            source: (0, 0, 10_000, 3),
            rows: vec![AxisField::plain(0)],
            cols: vec![AxisField::plain(1)],
            values: vec![ValueField {
                col: 3,
                agg: Agg::Sum,
                label: None,
                show_as: ShowAs::Normal,
                base: None,
            }],
            filters: vec![FilterField {
                col: 2,
                keep: vec!["线下".into()], // odd rows only -> 5,000 rows * 10.0 = 50,000.0
            }],
            layout: PivotLayout {
                tabular: true,
                subtotals: false,
                grand_totals: true,
                sort: AxisSort::Source,
            },
        };
        let grid = compute(&sh, &spec);
        // First odd row is r=1 -> region = regions[1] = "华东", r=3 -> regions[3] = "西南" (even indices 0/2 filtered out!)
        assert_eq!(grid.rows[0][0], Value::Text("华东".into()));
        assert_eq!(grid.rows[1][0], Value::Text("西南".into()));
        let gt = grid.grand_total_row.unwrap();
        let last_col = grid.rows[gt].len() - 1;
        assert_eq!(grid.rows[gt][last_col], Value::Number(50_000.0));

        // Pinyin sort order check
        spec.layout.sort = AxisSort::Pinyin;
        let grid_py = compute(&sh, &spec);
        assert_eq!(grid_py.rows[0][0], Value::Text("华东".into()));
    }
}
