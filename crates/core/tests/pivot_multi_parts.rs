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
        values: vec![ValueField { col: 2, agg: Agg::Sum, label: None, show_as: sheetcore::pivot_multi::ShowAs::Normal }],
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
            ValueField { col: 2, agg: Agg::Sum, label: None, show_as: sheetcore::pivot_multi::ShowAs::Normal },
            ValueField { col: 3, agg: Agg::Sum, label: None, show_as: sheetcore::pivot_multi::ShowAs::Normal },
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

/// 多值但无列字段：Excel 用纯 -2 数据轴表达（gold E 实测），我们照做而不是拒绝。
#[test]
fn multi_value_without_column_field_uses_pure_value_axis() {
    let f = Fixture::new("purevalue");
    let path = f.file("c.xlsx");
    let mut wb = book(&path);
    let mut spec = spec_b();
    spec.cols.clear();
    let grid = pivot_multi::compute(&wb.sheets[0], &spec);
    let rect = pivot_multi::write_grid(&mut wb.sheets[0], (8, 0), &grid);
    let id = sheetcore::pivot_xml::attach2_for_session(&path, "Data", &spec, &grid, rect, "test")
        .expect("纯值轴应可生成");
    assert_eq!(id, 1);
    let xml = String::from_utf8(
        sheetcore::fixups::read_part(&path, "xl/pivotTables/pivotTable1.xml").expect("缺部件"),
    )
    .unwrap();
    assert!(xml.contains("<colFields count=\"1\"><field x=\"-2\"/></colFields>"), "{xml}");
}
/// 保存路径：pivots_pending2 里的多字段透视必须真的写成 pivotTable 部件。
#[test]
fn save_attaches_multi_field_pivot_parts() {
    let f = Fixture::new("save2");
    let path = f.file("save.xlsx");
    let mut wb = book(&path);
    let spec = spec_a();
    let grid = pivot_multi::compute(&wb.sheets[0], &spec);
    let rect = pivot_multi::write_grid(&mut wb.sheets[0], (8, 0), &grid);
    wb.pivots_pending2.push((0, spec.clone(), grid, rect));
    xlsx::save(&path, &wb).expect("保存失败");

    let parts = pivot_parts(&path);
    assert!(
        parts.iter().any(|p| p.ends_with("pivotTable1.xml")),
        "保存后没有 pivotTable 部件：{parts:?}"
    );
    assert!(
        parts.iter().any(|p| p.ends_with("pivotCacheRecords1.xml")),
        "保存后没有缓存记录部件：{parts:?}"
    );
    let reopened = xlsx::load(&path).expect("重新打开失败");
    assert!(reopened.sheets[0].get(8, 0).is_some(), "透视表区域内容丢了");
}

fn spec_d() -> PivotSpec2 {
    PivotSpec2 {
        source: (0, 0, 6, 3),
        rows: vec![AxisField { col: 0, group: sheetcore::pivot::GroupBy::None }],
        cols: vec![
            AxisField { col: 1, group: sheetcore::pivot::GroupBy::None },
            AxisField { col: 3, group: sheetcore::pivot::GroupBy::None },
        ],
        values: vec![ValueField { col: 2, agg: Agg::Sum, label: None, show_as: sheetcore::pivot_multi::ShowAs::Normal }],
        filters: vec![],
        layout: PivotLayout::default(),
    }
}

/// 两个列字段：每个列字段都要标 axisCol，colItems 按层嵌套（Excel 会因字段没标轴直接丢表）。
#[test]
fn two_column_fields_are_both_marked_as_axis_fields() {
    let f = Fixture::new("2cols");
    let path = f.file("d.xlsx");
    let mut wb = book(&path);
    let spec = spec_d();
    let (id, _rows) = attach(&path, &mut wb, &spec);
    assert_eq!(id, 1);
    let parts = pivot_parts(&path);
    assert!(parts.iter().any(|p| p.ends_with("pivotTable1.xml")), "{parts:?}");
    // 两个列字段都必须标成 axisCol，否则 Excel 会静默丢掉整张透视表
    let xml = String::from_utf8(
        sheetcore::fixups::read_part(&path, "xl/pivotTables/pivotTable1.xml").expect("缺 pivotTable 部件"),
    )
    .unwrap();
    assert_eq!(xml.matches("axis=\"axisCol\"").count(), 2, "列字段标轴数量不对：{xml}");
    assert!(xml.contains("<field x=\"1\"/><field x=\"3\"/>"), "colFields 顺序不对：{xml}");
}

fn spec_e() -> PivotSpec2 {
    PivotSpec2 {
        source: (0, 0, 6, 3),
        rows: vec![AxisField { col: 0, group: sheetcore::pivot::GroupBy::None }],
        cols: vec![],
        values: vec![
            ValueField { col: 2, agg: Agg::Sum, label: None, show_as: sheetcore::pivot_multi::ShowAs::Normal },
            ValueField { col: 2, agg: Agg::Count, label: None, show_as: sheetcore::pivot_multi::ShowAs::Normal },
            ValueField { col: 2, agg: Agg::Avg, label: None, show_as: sheetcore::pivot_multi::ShowAs::Normal },
            ValueField { col: 2, agg: Agg::Max, label: None, show_as: sheetcore::pivot_multi::ShowAs::Normal },
            ValueField { col: 2, agg: Agg::Min, label: None, show_as: sheetcore::pivot_multi::ShowAs::Normal },
        ],
        filters: vec![],
        layout: PivotLayout::default(),
    }
}

/// 非求和聚合必须写 subtotal，否则 Excel 一律按求和重算；无列字段时列轴是纯 -2 数据轴。
#[test]
fn non_sum_aggregations_write_subtotal_attribute() {
    let f = Fixture::new("aggs");
    let path = f.file("e.xlsx");
    let mut wb = book(&path);
    let spec = spec_e();
    let (id, _rows) = attach(&path, &mut wb, &spec);
    assert_eq!(id, 1);
    let xml = String::from_utf8(
        sheetcore::fixups::read_part(&path, "xl/pivotTables/pivotTable1.xml").expect("缺 pivotTable 部件"),
    )
    .unwrap();
    for sub in ["count", "average", "max", "min"] {
        assert!(
            xml.contains(&format!("subtotal=\"{sub}\"")),
            "缺 subtotal=\"{sub}\"：{xml}"
        );
    }
    assert!(xml.contains("<dataFields count=\"5\">"), "数据字段数不对：{xml}");
    assert!(
        xml.contains("<colFields count=\"1\"><field x=\"-2\"/></colFields>"),
        "纯值轴应只有 -2 列字段：{xml}"
    );
    assert_eq!(xml.matches("<i i=\"").count(), 5, "每个值字段应有一个列项：{xml}");
}

fn spec_g() -> PivotSpec2 {
    PivotSpec2 {
        source: (0, 0, 6, 3),
        rows: vec![AxisField { col: 0, group: sheetcore::pivot::GroupBy::None }],
        cols: vec![
            AxisField { col: 1, group: sheetcore::pivot::GroupBy::None },
            AxisField { col: 3, group: sheetcore::pivot::GroupBy::None },
        ],
        values: vec![
            ValueField { col: 2, agg: Agg::Sum, label: None, show_as: sheetcore::pivot_multi::ShowAs::Normal },
            ValueField { col: 2, agg: Agg::Count, label: None, show_as: sheetcore::pivot_multi::ShowAs::Normal },
        ],
        filters: vec![],
        layout: PivotLayout::default(),
    }
}

/// 两个列字段 + 两个值字段：colFields 末尾要有 -2，每个叶子按值展开，总计按值展开。
#[test]
fn two_column_fields_with_two_values_expand_each_value() {
    let f = Fixture::new("2cols2vals");
    let path = f.file("g.xlsx");
    let mut wb = book(&path);
    let spec = spec_g();
    let (id, _rows) = attach(&path, &mut wb, &spec);
    assert_eq!(id, 1);
    let xml = String::from_utf8(
        sheetcore::fixups::read_part(&path, "xl/pivotTables/pivotTable1.xml").expect("缺部件"),
    )
    .unwrap();
    assert!(
        xml.contains("<colFields count=\"3\"><field x=\"1\"/><field x=\"3\"/><field x=\"-2\"/></colFields>"),
        "colFields 不对：{xml}"
    );
    // 每个叶子 2 个值 → 4 个叶子 8 项；加 2 个组小计 + 2 个总计 = 12 项（还有 2 个一级组）
    assert!(xml.contains("i=\"1\""), "缺少第二个值的列项：{xml}");
    // 行轴 1 个总计 + 列轴按 2 个值展开的 2 个 = 3
    assert_eq!(xml.matches("<i t=\"grand\"").count(), 3, "总计项数不对：{xml}");
}

#[test]
fn compact_layout_reaches_the_xml_and_excel_reads_it() {
    let fx = Fixture::new("compact");
    let path = fx.file("compact.xlsx");
    let mut wb = book(&path);
    let mut spec = spec_b();
    spec.layout.tabular = false; // 压缩形式
    let _ = attach(&path, &mut wb, &spec);

    let mut z = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
    let mut xml = String::new();
    {
        use std::io::Read;
        z.by_name("xl/pivotTables/pivotTable1.xml").unwrap().read_to_string(&mut xml).unwrap();
    }
    assert!(xml.contains("compact=\"1\" compactData=\"1\""), "压缩布局没写进 XML：{xml}");
    assert!(xml.contains("outline=\"0\" outlineData=\"0\""));

    // 留一份给 Excel 探针（临时目录会被 Drop 清掉）
    let keep = std::path::Path::new("F:/finance-native-sheets-ready/work/probe/c12/compact-multi.xlsx");
    let _ = std::fs::remove_file(keep);
    std::fs::copy(&path, keep).ok();
}

#[test]
fn show_as_percentages_match_excel_semantics() {
    let fx = Fixture::new("showas");
    let path = fx.file("showas.xlsx");
    let mut wb = book(&path);
    // 行=地区、列=产品、值=金额；Excel 实测：300/行500=0.6、300/列300=1、300/总1050=0.285714
    let mk = |sa: sheetcore::pivot_multi::ShowAs| PivotSpec2 {
        source: (0, 0, 6, 3),
        rows: vec![AxisField { col: 0, group: sheetcore::pivot::GroupBy::None }],
        cols: vec![AxisField { col: 1, group: sheetcore::pivot::GroupBy::None }],
        values: vec![ValueField {
            col: 2,
            agg: Agg::Sum,
            label: None,
            show_as: sa,
        }],
        filters: vec![],
        layout: PivotLayout::default(),
    };
    let num_at = |grid: &sheetcore::pivot_multi::PivotGrid, r: usize, c: usize| match grid.rows[r]
        [grid.row_depth + c]
    {
        sheetcore::Value::Number(n) => n,
        _ => f64::NAN,
    };

    // 华东/A = 10 + 30 = 40；华东行合计 = 40+50 = 90；A 列合计 = 40+30?（华北/A=60）→ 100；总 210
    let row = sheetcore::pivot_multi::compute(&wb.sheets[0], &mk(sheetcore::pivot_multi::ShowAs::PercentOfRow));
    // 行序按拼音：华北(bei) 在 华东(dong) 前；华北/A = 60，行合计 120，A 列合计 100，全表 210
    assert!((num_at(&row, 0, 0) - 60.0 / 120.0).abs() < 1e-12, "percentOfRow");

    let col = sheetcore::pivot_multi::compute(&wb.sheets[0], &mk(sheetcore::pivot_multi::ShowAs::PercentOfColumn));
    assert!((num_at(&col, 0, 0) - 60.0 / 100.0).abs() < 1e-12, "percentOfColumn");

    let tot = sheetcore::pivot_multi::compute(&wb.sheets[0], &mk(sheetcore::pivot_multi::ShowAs::PercentOfTotal));
    assert!((num_at(&tot, 0, 0) - 60.0 / 210.0).abs() < 1e-12, "percentOfTotal");

    // XML 必须带 showDataAs + numFmtId=10
    let spec = mk(sheetcore::pivot_multi::ShowAs::PercentOfTotal);
    let _ = attach(&path, &mut wb, &spec);
    let mut z = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
    let mut xml = String::new();
    {
        use std::io::Read;
        z.by_name("xl/pivotTables/pivotTable1.xml").unwrap().read_to_string(&mut xml).unwrap();
    }
    assert!(xml.contains("showDataAs=\"percentOfTotal\""), "{xml}");
    assert!(xml.contains("numFmtId=\"10\""), "{xml}");

    let keep = std::path::Path::new("F:/finance-native-sheets-ready/work/probe/c12/showas-ours.xlsx");
    let _ = std::fs::remove_file(keep);
    std::fs::copy(&path, keep).ok();
}

#[test]
fn show_as_labels_and_xml_names_are_stable() {
    use sheetcore::pivot_multi::ShowAs;
    assert_eq!(ShowAs::Normal.xml(), None);
    assert_eq!(ShowAs::PercentOfRow.xml(), Some("percentOfRow"));
    assert_eq!(ShowAs::PercentOfColumn.xml(), Some("percentOfColumn"));
    assert_eq!(ShowAs::PercentOfTotal.xml(), Some("percentOfTotal"));
    // 界面文案（改了要同步截图验收）
    assert_eq!(ShowAs::Normal.label(), "值");
    assert_eq!(ShowAs::PercentOfRow.label(), "占行合计%");
    assert_eq!(ShowAs::PercentOfColumn.label(), "占列合计%");
    assert_eq!(ShowAs::PercentOfTotal.label(), "占总计%");
}
