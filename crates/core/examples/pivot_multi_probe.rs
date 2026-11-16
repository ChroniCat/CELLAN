//! C12.4 多字段探针：生成带真 pivotTable 部件的多字段透视工作簿。
//!
//! 用法：`cargo run -p sheetcore --example pivot_multi_probe -- <A|B|C> <out.xlsx>`

use sheetcore::pivot::{Agg, GroupBy};
use sheetcore::pivot_multi::{self, AxisField, PivotLayout, PivotSpec2, ValueField};
use sheetcore::{Cell, Value, Workbook, xlsx};

fn text(s: &str) -> Cell {
    Cell { value: Value::Text(s.into()), formula: None }
}

fn num(v: f64) -> Cell {
    Cell { value: Value::Number(v), formula: None }
}

fn main() {
    let case = std::env::args().nth(1).unwrap_or_else(|| "A".into());
    let out = std::env::args()
        .nth(2)
        .unwrap_or_else(|| format!("work/probe/c12/multi-{case}.xlsx"));
    let path = std::path::PathBuf::from(&out);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok();
    }

    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Data".into();
    let s = &mut wb.sheets[0];
    for (i, h) in ["地区", "产品", "金额", "数量"].iter().enumerate() {
        s.set(0, i as u32, text(h));
    }
    let rows = [
        ("华东", "A", 10.0, 2.0),
        ("华北", "B", 20.0, 3.0),
        ("华东", "A", 30.0, 4.0),
        ("华北", "B", 40.0, 5.0),
        ("华东", "B", 50.0, 6.0),
        ("华北", "A", 60.0, 7.0),
    ];
    for (i, (region, product, amount, qty)) in rows.iter().enumerate() {
        let r = i as u32 + 1;
        s.set(r, 0, text(region));
        s.set(r, 1, text(product));
        s.set(r, 2, num(*amount));
        s.set(r, 3, num(*qty));
    }

    xlsx::save_atomic_with_receipt(&path, &wb)
        .expect("save base")
        .apply_to(&mut wb);
    wb.path = Some(path.clone());

    let spec = match case.to_ascii_uppercase().as_str() {
        "A" => PivotSpec2 {
            source: (0, 0, 6, 3),
            rows: vec![
                AxisField { col: 0, group: GroupBy::None },
                AxisField { col: 1, group: GroupBy::None },
            ],
            cols: vec![],
            values: vec![ValueField { col: 2, agg: Agg::Sum, label: None }],
            filters: vec![],
            layout: PivotLayout::default(),
        },
        "B" => PivotSpec2 {
            source: (0, 0, 6, 3),
            rows: vec![AxisField { col: 0, group: GroupBy::None }],
            cols: vec![AxisField { col: 1, group: GroupBy::None }],
            values: vec![
                ValueField { col: 2, agg: Agg::Sum, label: None },
                ValueField { col: 3, agg: Agg::Sum, label: None },
            ],
            filters: vec![],
            layout: PivotLayout::default(),
        },
        "C" => PivotSpec2 {
            source: (0, 0, 6, 3),
            rows: vec![
                AxisField { col: 0, group: GroupBy::None },
                AxisField { col: 1, group: GroupBy::None },
            ],
            cols: vec![AxisField { col: 3, group: GroupBy::None }],
            values: vec![ValueField { col: 2, agg: Agg::Sum, label: None }],
            filters: vec![],
            layout: PivotLayout::default(),
        },
        // 1 行 + 2 列 + 1 值（多列字段，嵌套 colItems）
        "D" => PivotSpec2 {
            source: (0, 0, 6, 3),
            rows: vec![AxisField { col: 0, group: GroupBy::None }],
            cols: vec![
                AxisField { col: 1, group: GroupBy::None },
                AxisField { col: 3, group: GroupBy::None },
            ],
            values: vec![ValueField { col: 2, agg: Agg::Sum, label: None }],
            filters: vec![],
            layout: PivotLayout::default(),
        },
        // 1 行 + 5 种聚合、无列字段（Excel 的纯值轴 -2）
        "E" => PivotSpec2 {
            source: (0, 0, 6, 3),
            rows: vec![AxisField { col: 0, group: GroupBy::None }],
            cols: vec![],
            values: vec![
                ValueField { col: 2, agg: Agg::Sum, label: None },
                ValueField { col: 2, agg: Agg::Count, label: None },
                ValueField { col: 2, agg: Agg::Avg, label: None },
                ValueField { col: 2, agg: Agg::Max, label: None },
                ValueField { col: 2, agg: Agg::Min, label: None },
            ],
            filters: vec![],
            layout: PivotLayout::default(),
        },
        // 1 行 + 1 列 + 2 值（求和 + 计数）：subtotal 与 -2 数据轴同时用
        "F" => PivotSpec2 {
            source: (0, 0, 6, 3),
            rows: vec![AxisField { col: 0, group: GroupBy::None }],
            cols: vec![AxisField { col: 1, group: GroupBy::None }],
            values: vec![
                ValueField { col: 2, agg: Agg::Sum, label: None },
                ValueField { col: 2, agg: Agg::Count, label: None },
            ],
            filters: vec![],
            layout: PivotLayout::default(),
        },
        other => panic!("未知用例 {other}"),
    };

    let grid = pivot_multi::compute(&wb.sheets[0], &spec);
    let rect = pivot_multi::write_grid(&mut wb.sheets[0], (8, 0), &grid);
    let id = sheetcore::pivot_xml::attach2_for_session(
        &path,
        "Data",
        &spec,
        &grid,
        rect,
        "probe",
    )
    .expect("attach multi pivot");
    println!(
        "case={case} saved {out} pivot_id={id} rect={rect:?} rows={} cols={} values={}",
        spec.rows.len(),
        spec.cols.len(),
        spec.values.len()
    );
}
