//! C12.4 探针：造一份带「真 pivotTable 部件」的工作簿，交给 Excel/WPS 验证能不能当可编辑透视表打开。
//!
//! 用法：`cargo run -p sheetcore --example pivot_excel_probe -- <out.xlsx>`

use sheetcore::pivot::{Agg, GroupBy, PivotSpec};
use sheetcore::{Cell, Value, Workbook, pivot, pivot_xml, xlsx};

fn text(s: &str) -> Cell {
    Cell { value: Value::Text(s.into()), formula: None }
}

fn num(v: f64) -> Cell {
    Cell { value: Value::Number(v), formula: None }
}

fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "work/probe/c12/pivot-real.xlsx".into());
    let path = std::path::PathBuf::from(&out);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok();
    }

    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Data".into();
    let s = &mut wb.sheets[0];
    s.set(0, 0, text("地区"));
    s.set(0, 1, text("产品"));
    s.set(0, 2, text("金额"));
    let rows = [
        ("华东", "A", 10.0),
        ("华北", "B", 20.0),
        ("华东", "A", 30.0),
        ("华北", "B", 40.0),
        ("华东", "B", 50.0),
        ("华北", "A", 60.0),
    ];
    for (i, (region, product, amount)) in rows.iter().enumerate() {
        let r = i as u32 + 1;
        s.set(r, 0, text(region));
        s.set(r, 1, text(product));
        s.set(r, 2, num(*amount));
    }

    xlsx::save_atomic_with_receipt(&path, &wb)
        .expect("save base")
        .apply_to(&mut wb);
    wb.path = Some(path.clone());

    let spec = PivotSpec {
        source: (0, 0, 6, 2),
        row_field: 0,
        col_field: Some(1),
        value_field: 2,
        agg: Agg::Sum,
        group: GroupBy::None,
        exclude: Vec::new(),
    };
    let res = pivot::compute(&wb.sheets[0], &spec);
    let height = res.rows.len() as u32 + 1;
    // 透视表范围必须正好是「表头行 + 每个条目 + 总计」，多一行 Excel 会把整个透视表丢掉
    let rect = (8u32, 0u32, 8 + height - 1, 3u32);
    let id = pivot_xml::attach(&path, "Data", &spec, &res, rect, "求和项:金额").expect("attach pivot");
    println!("saved {out} pivot_table_id={id} rows={}", res.rows.len());
}
