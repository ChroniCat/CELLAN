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
}

impl ShowAs {
    /// Excel 写在 `<dataField showDataAs="...">` 里的值
    pub fn xml(self) -> Option<&'static str> {
        match self {
            ShowAs::Normal => None,
            ShowAs::PercentOfRow => Some("percentOfRow"),
            ShowAs::PercentOfColumn => Some("percentOfColumn"),
            ShowAs::PercentOfTotal => Some("percentOfTotal"),
        }
    }

    /// 界面上的中文名
    pub fn label(self) -> &'static str {
        match self {
            ShowAs::Normal => "值",
            ShowAs::PercentOfRow => "占行合计%",
            ShowAs::PercentOfColumn => "占列合计%",
            ShowAs::PercentOfTotal => "占总计%",
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
    grid.rows.push(row);
}

/// 计算多字段透视
pub fn compute(sheet: &Sheet, spec: &PivotSpec2) -> PivotGrid {
    let (r1, _c1, r2, _c2) = spec.source;
    let hdr = |c: u32| header(sheet, r1, c);
    let mut grid = PivotGrid {
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

    let n_head = if spec.cols.is_empty() { 1 } else { spec.cols.len() + 1 };
    let total_cols = spec.rows.len() + col_paths.len() * spec.values.len();
    for hi in 0..n_head {
        let mut row = vec![String::new(); total_cols];
        if hi == 0 {
            for (i, f) in spec.rows.iter().enumerate() {
                if i < total_cols {
                    row[i] = hdr(f.col);
                }
            }
        }
        for (ci, p) in col_paths.iter().enumerate() {
            for vi in 0..spec.values.len() {
                let idx = spec.rows.len() + ci * spec.values.len() + vi;
                if idx >= total_cols {
                    continue;
                }
                if hi + 1 == n_head {
                    let v = &spec.values[vi];
                    row[idx] = v
                        .label
                        .clone()
                        .unwrap_or_else(|| format!("{} {}", hdr(v.col), v.agg.label()));
                } else if hi < p.len() {
                    row[idx] = p[hi].clone();
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
                srow[0] = Value::Text(format!("{top} 小计").into());
            }
            for ci in 0..col_paths.len() {
                for vi in 0..spec.values.len() {
                    srow[spec.rows.len() + ci * spec.values.len() + vi] =
                        sub[ci][vi].finish(spec.values[vi].agg);
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
            let mut row_sum = 0.0;
            for c in 0..ncols {
                row_sum += as_num(&grid.rows[r][rd + c * vc + vi]);
            }
            for c in 0..ncols {
                let idx = rd + c * vc + vi;
                let v = as_num(&grid.rows[r][idx]);
                let denom = match mode {
                    ShowAs::PercentOfRow => row_sum,
                    ShowAs::PercentOfColumn => col_tot[c],
                    ShowAs::PercentOfTotal => total,
                    ShowAs::Normal => 1.0,
                };
                grid.rows[r][idx] = if denom.abs() > f64::EPSILON {
                    Value::Number(v / denom)
                } else {
                    Value::Empty
                };
            }
        }
    }
}

/// 把网格写进工作表（从 at 开始），返回占用区域
pub fn write_grid(sheet: &mut Sheet, at: (u32, u32), grid: &PivotGrid) -> (u32, u32, u32, u32) {
    let (r0, c0) = at;
    let mut r = r0;
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
