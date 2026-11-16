//! C12.4 多字段透视部件：生成不报错、包内落位、字段数与网格一致。

use sheetcore::pivot::Agg;
use sheetcore::pivot_multi::{self, AxisField, PivotLayout, PivotSpec2, ValueField};
use sheetcore::{Cell, Value, Workbook, xlsx};
use std::path::PathBuf;

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "sc-pivot2-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self { dir }
    }
    fn file(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn text(s: &str) -> Cell {
    Cell { value: Value::Text(s.into()), formula: None }
}

fn num(v: f64) -> Cell {
    Cell { value: Value::Number(v), formula: None }
}

fn book(path: &std::path::Path) -> Workbook {
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
    for (i, (r, p, a, q)) in rows.iter().enumerate() {
        let row = i as u32 + 1;
        s.set(row, 0, text(r));
        s.set(row, 1, text(p));
        s.set(row, 2, num(*a));
        s.set(row, 3, num(*q));
    }
    xlsx::save_atomic_with_receipt(path, &wb).unwrap().apply_to(&mut wb);
    wb.path = Some(path.into());
    wb
}

fn spec_a() -> PivotSpec2 {
    PivotSpec2 {
        source: (0, 0, 6, 3),
        rows: vec![
            AxisField { col: 0, group: sheetcore::pivot::GroupBy::None },
            AxisField { col: 1, group: sheetcore::pivot::GroupBy::None },
        ],
        cols: vec![],
        values: vec![ValueField { col: 2, agg: Agg::Sum, label: None }],
        filters: vec![],
        layout: PivotLayout::default(),
    }
}

fn spec_b() -> PivotSpec2 {
    PivotSpec2 {
        source: (0, 0, 6, 3),
        rows: vec![AxisField { col: 0, group: sheetcore::pivot::GroupBy::None }],
        cols: vec![AxisField { col: 1, group: sheetcore::pivot::GroupBy::None }],
        values: vec![
            ValueField { col: 2, agg: Agg::Sum, label: None },
            ValueField { col: 3, agg: Agg::Sum, label: None },
        ],
        filters: vec![],
        layout: PivotLayout::default(),
    }
}

fn attach(path: &std::path::Path, wb: &mut Workbook, spec: &PivotSpec2) -> (u32, usize) {
    let grid = pivot_multi::compute(&wb.sheets[0], spec);
    let rect = pivot_multi::write_grid(&mut wb.sheets[0], (8, 0), &grid);
    let id = sheetcore::pivot_xml::attach2_for_session(path, "Data", spec, &grid, rect, "test")
        .expect("多字段透视附挂失败");
    (id, grid.rows.len())
}

fn pivot_parts(path: &std::path::Path) -> Vec<String> {
    sheetcore::fixups::parts(path)
        .into_iter()
        .filter(|p| p.contains("pivotTable") || p.contains("pivotCache"))
        .collect()
}

#[test]
fn two_row_fields_emit_one_item_per_grid_row() {
    let f = Fixture::new("2rows");
    let path = f.file("a.xlsx");
    let mut wb = book(&path);
    let spec = spec_a();
    let (id, rows) = attach(&path, &mut wb, &spec);
    assert_eq!(id, 1);
    let parts = pivot_parts(&path);
    assert!(parts.iter().any(|p| p.ends_with("pivotTable1.xml")), "{parts:?}");
    assert!(parts.iter().any(|p| p.ends_with("pivotCacheDefinition1.xml")), "{parts:?}");
    // 网格里 4 个叶子行 + 2 个小计 + 1 个总计
    assert_eq!(rows, 7);
}

#[test]
fn two_value_fields_register_two_data_fields() {
    let f = Fixture::new("2values");
    let path = f.file("b.xlsx");
    let mut wb = book(&path);
    let spec = spec_b();
    let (id, _rows) = attach(&path, &mut wb, &spec);
    assert_eq!(id, 1);
    let parts = pivot_parts(&path);
    assert_eq!(
        parts.iter().filter(|p| p.ends_with(".xml") && p.contains("pivotTable")).count(),
        1
    );
}

#[test]
fn multi_value_without_column_field_is_rejected_instead_of_written_broken() {
    let f = Fixture::new("reject");
    let path = f.file("c.xlsx");
    let mut wb = book(&path);
    let mut spec = spec_b();
    spec.cols.clear();
    let grid = pivot_multi::compute(&wb.sheets[0], &spec);
    let rect = pivot_multi::write_grid(&mut wb.sheets[0], (8, 0), &grid);
    let err = sheetcore::pivot_xml::attach2_for_session(&path, "Data", &spec, &grid, rect, "test")
        .unwrap_err()
        .to_string();
    assert!(err.contains("多值透视必须带列字段"), "{err}");
}
