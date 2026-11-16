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
    /// 与上一项（同一行里前一列）的差
    DifferencePrev,
    /// 与上一项的百分比差
    PercentDifferencePrev,
    /// Excel 的「索引」：(值 × 总计) ÷ (行合计 × 列合计)
    Index,
}

impl ShowAs {
    /// Excel 写在 `<dataField showDataAs="...">` 里的值
    pub fn xml(self) -> Option<&'static str> {
        match self {
            ShowAs::Normal => None,
            ShowAs::PercentOfRow => Some("percentOfRow"),
            ShowAs::PercentOfColumn => Some("percentOfColumn"),
            ShowAs::PercentOfTotal => Some("percentOfTotal"),
            ShowAs::DifferencePrev => Some("difference"),
            ShowAs::PercentDifferencePrev => Some("percent"),
            ShowAs::Index => Some("index"),
        }
    }

    /// 这类显示值要不要套百分比格式（Excel 只给百分比类写 numFmtId="10"）
    pub fn percent_format(self) -> bool {
        matches!(
            self,
            ShowAs::PercentOfRow
                | ShowAs::PercentOfColumn
                | ShowAs::PercentOfTotal
                | ShowAs::PercentDifferencePrev
        )
    }

    /// 界面上的中文名
    pub fn label(self) -> &'static str {
        match self {
            ShowAs::Normal => "值",
            ShowAs::PercentOfRow => "占行合计%",
            ShowAs::PercentOfColumn => "占列合计%",
            ShowAs::PercentOfTotal => "占总计%",
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
}

/// 筛选字段：keep 为空表示全部
#[derive(Clone, Debug, PartialEq)]
pub struct FilterField {
    pub col: u32,
    pub keep: Vec<String>,
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
}

impl Default for PivotLayout {
    fn default() -> Self {
        Self { tabular: true, subtotals: true, grand_totals: true }
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
            values: vec![ValueField { col: spec.value_field, agg: spec.agg, label: None, show_as: ShowAs::Normal }],
            filters: Vec::new(),
            layout: PivotLayout::default(),
        }
    }
}

#[derive(Clone, Debug, Default)]
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
    fn push(&mut self, v: Option<&Value>) {
        let Some(v) = v else { return };
        match v {
            Value::Empty => {}
            Value::Number(n) => {
                self.count_nonempty += 1;
                self.numeric += 1;
                self.sum += *n;
                self.min = Some(self.min.map_or(*n, |m| m.min(*n)));
                self.max = Some(self.max.map_or(*n, |m| m.max(*n)));
            }
            other => {
                let t = other.display();
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
    col_paths: &[Vec<String>],
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
    for p in col_paths {
        for vi in 0..spec.values.len() {
            let v = m
                .and_then(|x| x.get(p))
                .and_then(|accs| accs.get(vi))
                .map(|a| a.finish(spec.values[vi].agg))
                .unwrap_or(Value::Empty);
            row.push(v);
        }
    }
    // 右侧「…汇总」总计列先占位，调用方填值
    while row.len() < total_cols {
        row.push(Value::Empty);
    }
    grid.rows.push(row);
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

    // 收集：行路径 → 列路径 → 每个值字段的累计
    let mut data: BTreeMap<Vec<String>, CellMap> = BTreeMap::new();
    for r in (r1 + 1)..=r2 {
        let mut keep = true;
        for f in &spec.filters {
            if f.keep.is_empty() {
                continue;
            }
            let v = sheet.get(r, f.col).map(|x| x.value.display()).unwrap_or_default();
            if !f.keep.iter().any(|k| *k == v) {
                keep = false;
                break;
            }
        }
        if !keep {
            continue;
        }
        let get = |c: u32| sheet.get(r, c).map(|x| x.value.clone()).unwrap_or(Value::Empty);
        let row_path: Vec<String> = spec
            .rows
            .iter()
            .map(|f| group_key(&get(f.col), &f.group))
            .collect();
        let col_path: Vec<String> = spec
            .cols
            .iter()
            .map(|f| group_key(&get(f.col), &f.group))
            .collect();
        let entry = data.entry(row_path).or_default().entry(col_path).or_default();
        entry.resize(spec.values.len(), Acc::default());
        for (i, vf) in spec.values.iter().enumerate() {
            entry[i].push(sheet.get(r, vf.col).map(|x| &x.value));
        }
    }

    // 列叶子标签
    let mut col_paths: Vec<Vec<String>> = Vec::new();
    for m in data.values() {
        for k in m.keys() {
            if !col_paths.contains(k) {
                col_paths.push(k.clone());
            }
        }
    }
    col_paths.sort_by(|a, b| crate::collate::cmp_pinyin(&a.join(" / "), &b.join(" / ")));
    if col_paths.is_empty() && spec.cols.is_empty() {
        col_paths.push(Vec::new());
    }
    for p in &col_paths {
        grid.col_labels.push(if p.is_empty() { String::new() } else { p.join(" / ") });
    }
    grid.col_paths = col_paths.clone();

    let n_head = if spec.cols.is_empty() { 1 } else { spec.cols.len() + 2 };
    // Excel 在有列字段且显示总计列时，右侧给每个值字段各留一列「<值名>汇总」
    let grand_block = if !spec.cols.is_empty() && spec.layout.grand_totals {
        spec.values.len()
    } else {
        0
    };
    let grand_base = spec.rows.len() + col_paths.len() * spec.values.len();
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
            for (ci, _p) in col_paths.iter().enumerate() {
                for vi in 0..spec.values.len() {
                    row[spec.rows.len() + ci * spec.values.len() + vi] = value_label(vi);
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
            for (ci, _p) in col_paths.iter().enumerate() {
                for vi in 0..spec.values.len() {
                    row[spec.rows.len() + ci * spec.values.len() + vi] = value_label(vi);
                }
            }
        } else {
            // 中间行：列标签逐层；紧邻字段名行的那一行右侧放「…汇总」
            let lvl = hi - 1;
            for (ci, p) in col_paths.iter().enumerate() {
                if lvl < p.len() {
                    row[spec.rows.len() + ci * spec.values.len()] = p[lvl].clone();
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

    // 逐组输出 + 小计 + 总计
    let mut prev: Vec<String> = vec![String::new(); spec.rows.len()];
    let mut keys: Vec<Vec<String>> = data.keys().cloned().collect();
    keys.sort_by(|a, b| crate::collate::cmp_pinyin(&a.join(" / "), &b.join(" / ")));
    let mut grand: Vec<Vec<Acc>> = vec![vec![Acc::default(); spec.values.len()]; col_paths.len()];
    let mut i = 0;
    while i < keys.len() {
        let top = keys[i].first().cloned().unwrap_or_default();
        let start = i;
        while i < keys.len() && keys[i].first().cloned().unwrap_or_default() == top {
            i += 1;
        }
        let mut sub: Vec<Vec<Acc>> = vec![vec![Acc::default(); spec.values.len()]; col_paths.len()];
        for k in &keys[start..i] {
            let m = &data[k];
            emit_row(&mut grid, k, Some(m), &col_paths, spec, &mut prev, total_cols);
            if grand_block > 0 {
                let last = grid.rows.len() - 1;
                for vi in 0..spec.values.len() {
                    let mut acc = Acc::default();
                    for p in col_paths.iter() {
                        if let Some(a) = m.get(p) {
                            if vi < a.len() {
                                acc.merge(&a[vi]);
                            }
                        }
                    }
                    grid.rows[last][grand_base + vi] = acc.finish(spec.values[vi].agg);
                }
            }
            for (ci, p) in col_paths.iter().enumerate() {
                if let Some(accs) = m.get(p) {
                    for vi in 0..spec.values.len().min(accs.len()) {
                        sub[ci][vi].merge(&accs[vi]);
                        grand[ci][vi].merge(&accs[vi]);
                    }
                }
            }
        }
        if spec.layout.subtotals && spec.rows.len() >= 2 {
            let mut srow = vec![Value::Text(String::new().into()); total_cols];
            if !srow.is_empty() {
                srow[0] = Value::Text(format!("{top} 汇总").into());
            }
            for ci in 0..col_paths.len() {
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
        for ci in 0..col_paths.len() {
            for vi in 0..spec.values.len() {
                grow[spec.rows.len() + ci * spec.values.len() + vi] =
                    grand[ci][vi].finish(spec.values[vi].agg);
            }
        }
        if grand_block > 0 {
            for vi in 0..spec.values.len() {
                let mut acc = Acc::default();
                for ci in 0..col_paths.len() {
                    acc.merge(&grand[ci][vi]);
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
                    ShowAs::Normal => Value::Number(v),
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
