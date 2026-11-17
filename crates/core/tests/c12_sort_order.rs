//! C12.2：行标签排序口径 —— 拼音（Excel 新建默认）vs 数据源顺序（既有文件 rowItems）

use sheetcore::pivot::Agg;
use sheetcore::pivot_multi::{self, AxisField, AxisSort, PivotLayout, PivotSpec2, ValueField};
use sheetcore::{Cell, Value, Workbook, xlsx};
use std::path::PathBuf;

struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "sc-c12sort-{tag}-{}-{}",
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
        if std::env::var("SC_KEEP").is_ok() {
            println!("KEPT={}", self.dir.display());
            return;
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn text(s: &str) -> Cell {
    Cell { value: Value::Text(s.into()), formula: None }
}

fn num(v: f64) -> Cell {
    Cell { value: Value::Number(v), formula: None }
}

/// 源顺序：张三、阿里、曹操（拼音序应为 阿里、曹操、张三）
fn book(path: &std::path::Path) -> Workbook {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Data".into();
    let s = &mut wb.sheets[0];
    s.set(0, 0, text("人"));
    s.set(0, 1, text("金额"));
    for (i, (n, v)) in [("张三", 1.0), ("阿里", 2.0), ("曹操", 3.0)].iter().enumerate() {
        let r = i as u32 + 1;
        s.set(r, 0, text(n));
        s.set(r, 1, num(*v));
    }
    xlsx::save_atomic_with_receipt(path, &wb).unwrap().apply_to(&mut wb);
    wb.path = Some(path.into());
    wb
}

fn spec(sort: AxisSort) -> PivotSpec2 {
    PivotSpec2 {
        source: (0, 0, 3, 1),
        rows: vec![AxisField::plain(0)],
        cols: vec![],
        values: vec![ValueField {
            col: 1,
            agg: Agg::Sum,
            label: None,
            show_as: pivot_multi::ShowAs::Normal,
            base: None,
        }],
        filters: vec![],
        layout: PivotLayout { sort, ..Default::default() },
    }
}

#[test]
fn pinyin_and_source_orders_differ() {
    let f = Fixture::new("orders");
    let path = f.file("a.xlsx");
    let wb = book(&path);
    let py = pivot_multi::compute(&wb.sheets[0], &spec(AxisSort::Pinyin));
    let src = pivot_multi::compute(&wb.sheets[0], &spec(AxisSort::Source));
    let labels = |g: &pivot_multi::PivotGrid| -> Vec<String> {
        g.rows
            .iter()
            .filter_map(|r| r.first().map(|v| v.display()))
            .filter(|s| s != "总计")
            .collect()
    };
    assert_eq!(labels(&py), vec!["阿里", "曹操", "张三"], "拼音序");
    assert_eq!(labels(&src), vec!["张三", "阿里", "曹操"], "数据源顺序");
}

#[test]
fn source_order_is_written_into_row_items() {
    let f = Fixture::new("rowitems");
    let path = f.file("b.xlsx");
    let mut wb = book(&path);
    let sp = spec(AxisSort::Source);
    let grid = pivot_multi::compute(&wb.sheets[0], &sp);
    let rect = pivot_multi::write_grid(&mut wb.sheets[0], (5, 0), &grid);
    sheetcore::pivot_xml::attach2_for_session(&path, "Data", &sp, &grid, rect, "test")
        .expect("附挂");
    let xml = {
        use std::io::Read;
        let mut z = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
        let mut s = String::new();
        z.by_name("xl/pivotTables/pivotTable1.xml")
            .unwrap()
            .read_to_string(&mut s)
            .unwrap();
        s
    };
    // sharedItems 是源顺序（张三=0 阿里=1 曹操=2）；rowItems 必须按源顺序 0,1,2
    let idx: Vec<&str> = xml
        .split("<rowItems")
        .nth(1)
        .unwrap()
        .split("<x v=\"")
        .skip(1)
        .filter_map(|c| c.split('"').next())
        .collect();
    assert_eq!(&idx[..3], &["0", "1", "2"], "rowItems 顺序：{idx:?}");
}

#[test]
fn pinyin_order_is_written_into_row_items() {
    let f = Fixture::new("pyitems");
    let path = f.file("c.xlsx");
    let mut wb = book(&path);
    let sp = spec(AxisSort::Pinyin);
    let grid = pivot_multi::compute(&wb.sheets[0], &sp);
    let rect = pivot_multi::write_grid(&mut wb.sheets[0], (5, 0), &grid);
    sheetcore::pivot_xml::attach2_for_session(&path, "Data", &sp, &grid, rect, "test")
        .expect("附挂");
    let xml = {
        use std::io::Read;
        let mut z = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
        let mut s = String::new();
        z.by_name("xl/pivotTables/pivotTable1.xml")
            .unwrap()
            .read_to_string(&mut s)
            .unwrap();
        s
    };
    let idx: Vec<&str> = xml
        .split("<rowItems")
        .nth(1)
        .unwrap()
        .split("<x v=\"")
        .skip(1)
        .filter_map(|c| c.split('"').next())
        .collect();
    // 拼音：阿里(1) 曹操(2) 张三(0)
    assert_eq!(&idx[..3], &["1", "2", "0"], "rowItems 顺序：{idx:?}");
}
