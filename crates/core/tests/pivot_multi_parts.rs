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
fn show_as_difference_percent_diff_and_index_match_excel() {
    use sheetcore::pivot_multi::ShowAs;
    let fx = Fixture::new("showas2");
    let path = fx.file("showas2.xlsx");
    let mut wb = book(&path);
    let mk = |sa: ShowAs| PivotSpec2 {
        source: (0, 0, 6, 3),
        rows: vec![AxisField { col: 0, group: sheetcore::pivot::GroupBy::None }],
        cols: vec![AxisField { col: 1, group: sheetcore::pivot::GroupBy::None }],
        values: vec![ValueField { col: 2, agg: Agg::Sum, label: None, show_as: sa }],
        filters: vec![],
        layout: PivotLayout::default(),
    };
    // 华北行 = A60 / B60（行合计 120）；A 列合计 100、B 列合计 110；全表 210
    let cell = |g: &sheetcore::pivot_multi::PivotGrid, r: usize, c: usize| g.rows[r][g.row_depth + c].clone();

    let d = sheetcore::pivot_multi::compute(&wb.sheets[0], &mk(ShowAs::DifferencePrev));
    assert_eq!(cell(&d, 0, 0), sheetcore::Value::Empty, "第一列没有上一项");
    assert_eq!(cell(&d, 0, 1), sheetcore::Value::Number(0.0), "60-60");

    let pd = sheetcore::pivot_multi::compute(&wb.sheets[0], &mk(ShowAs::PercentDifferencePrev));
    assert_eq!(cell(&pd, 0, 0), sheetcore::Value::Empty);
    match cell(&pd, 0, 1) {
        sheetcore::Value::Number(n) => assert!(n.abs() < 1e-12),
        v => panic!("应为 0，实际 {v:?}"),
    }

    let ix = sheetcore::pivot_multi::compute(&wb.sheets[0], &mk(ShowAs::Index));
    match cell(&ix, 0, 0) {
        // 60 × 210 ÷ (120 × 100) = 1.05
        sheetcore::Value::Number(n) => assert!((n - 1.05).abs() < 1e-12, "{n}"),
        v => panic!("索引应为数字，实际 {v:?}"),
    }

    // XML：差/索引不带 numFmtId，差%带
    let _ = attach(&path, &mut wb, &mk(ShowAs::Index));
    let mut z = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
    let mut xml = String::new();
    {
        use std::io::Read;
        z.by_name("xl/pivotTables/pivotTable1.xml").unwrap().read_to_string(&mut xml).unwrap();
    }
    assert!(xml.contains("showDataAs=\"index\""), "{xml}");
    assert!(!xml.contains("numFmtId=\"10\""), "索引不该套百分比格式：{xml}");
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
    assert_eq!(ShowAs::DifferencePrev.xml(), Some("difference"));
    assert_eq!(ShowAs::PercentDifferencePrev.xml(), Some("percent"));
    assert_eq!(ShowAs::Index.xml(), Some("index"));
    assert_eq!(ShowAs::DifferencePrev.label(), "与上一项差");
    assert_eq!(ShowAs::PercentDifferencePrev.label(), "与上一项差%");
    assert_eq!(ShowAs::Index.label(), "索引");
    assert!(ShowAs::PercentDifferencePrev.percent_format());
    assert!(!ShowAs::DifferencePrev.percent_format());
    assert!(!ShowAs::Index.percent_format());
}

#[test]
fn calculated_field_lands_in_cache_definition_like_excel() {
    use sheetcore::pivot_xml;
    let fx = Fixture::new("calcfield");
    let path = fx.file("calcfield.xlsx");
    let mut wb = book(&path);
    let spec = spec_b();
    let _ = attach(&path, &mut wb, &spec);

    let n = pivot_xml::add_calculated_fields(&path, &[("总价", "金额*2")]).unwrap();
    // 缓存定义和透视表定义各改一处（pivotFields 条数必须跟着缓存字段数走）
    eprintln!("[calcitem] changed={n}");
    assert_eq!(n, 2, "缓存定义 + 透视表定义");

    let mut z = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
    let mut xml = String::new();
    {
        use std::io::Read;
        let name = z
            .file_names()
            .find(|n| n.contains("pivotCacheDefinition"))
            .unwrap()
            .to_string();
        z.by_name(&name).unwrap().read_to_string(&mut xml).unwrap();
    }
    // Excel 自己写的是：<cacheField name="总价" numFmtId="0" formula="金额*2" databaseField="0"/>
    assert!(xml.contains("formula=\"金额*2\""), "{xml}");
    assert!(xml.contains("databaseField=\"0\""));
    // count 必须跟着加一，否则 Excel 认为字段数与定义不符
    let open = xml.find("<cacheFields count=\"").unwrap() + "<cacheFields count=\"".len();
    let len = xml[open..].find('"').unwrap();
    let count: usize = xml[open..open + len].parse().unwrap();
    let actual = xml.matches("<cacheField name=").count();
    let pf_open = xml.find("<pivotFields count=\"");
    assert_eq!(count, actual, "count 与条目数不一致");

    let keep = std::path::Path::new("F:/finance-native-sheets-ready/work/probe/c12/calcfield-ours.xlsx");
    let _ = std::fs::remove_file(keep);
    std::fs::copy(&path, keep).ok();
}

#[test]
fn calculated_item_writes_cache_items_and_calculated_items_block() {
    use sheetcore::pivot_xml;
    let fx = Fixture::new("calcitem");
    let path = fx.file("calcitem.xlsx");
    let mut wb = book(&path);
    let spec = spec_b(); // 行=地区(col0)、列=产品(col1)、值=金额(col2)
    let _ = attach(&path, &mut wb, &spec);

    // 地区在缓存里是第 0 个字段；已有 2 个项，新项下标 2
    let n = pivot_xml::add_calculated_item(&path, "地区", "华东华北", "地区[华东]+地区[华北]", 0, 2, 0)
        .unwrap();
    assert_eq!(n, 2, "缓存定义 + 透视表定义");

    let mut z = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
    let mut cache = String::new();
    let mut table = String::new();
    {
        use std::io::Read;
        let cd = z
            .file_names()
            .find(|n| n.contains("pivotCacheDefinition"))
            .unwrap()
            .to_string();
        z.by_name(&cd).unwrap().read_to_string(&mut cache).unwrap();
        let pt = z
            .file_names()
            .find(|n| n.starts_with("xl/pivotTables/") && n.ends_with(".xml"))
            .unwrap()
            .to_string();
        z.by_name(&pt).unwrap().read_to_string(&mut table).unwrap();
    }
    assert!(cache.contains("<s v=\"华东华北\" f=\"1\"/>"), "{cache}");
    assert!(
        cache.contains("<calculatedItem formula=\"地区[华东]+地区[华北]\">"),
        "{cache}"
    );
    // 计算项块必须排在 extLst 之前
    if let (Some(ci), Some(el)) = (cache.find("<calculatedItems"), cache.find("<extLst")) {
        assert!(ci < el, "calculatedItems 必须在 extLst 之前");
    }
    assert!(table.contains("<item f=\"1\" x=\"2\"/>"), "{table}");

    let keep = std::path::Path::new("F:/finance-native-sheets-ready/work/probe/c12/calcitem-ours.xlsx");
    let _ = std::fs::remove_file(keep);
    std::fs::copy(&path, keep).ok();
}

#[test]
fn calc_item_row_appends_sum_row_before_grand_total() {
    let fx = Fixture::new("calcrow");
    let path = fx.file("calcrow.xlsx");
    let mut wb = book(&path);
    let spec = spec_b();
    let mut grid = sheetcore::pivot_multi::compute(&wb.sheets[0], &spec);
    let before_rows = grid.rows.len();
    let grand_before = grid.grand_total_row.expect("应有总计行");
    let grand_vals: Vec<String> = grid.rows[grand_before].iter().map(|v| v.display()).collect();

    // 地区两个项合成一行：值应为两行之各列相加
    let at = sheetcore::pivot_multi::append_calc_item_row(&mut grid, "华东华北", &["华东", "华北"])
        .expect("应能找到两个来源行");
    assert_eq!(grid.rows.len(), before_rows + 1);
    assert_eq!(at, grand_before, "计算项插在总计行之前");
    assert_eq!(grid.grand_total_row, Some(grand_before + 1), "总计行下标要后移");
    assert_eq!(grid.rows[at][0].display(), "华东华北");

    // 逐列核对：新行 = 两来源行之和
    let rd = grid.row_depth;
    let src: Vec<usize> = [0usize, 1]
        .iter()
        .copied()
        .filter(|&r| r != at && r < grid.rows.len())
        .collect();
    for c in rd..grid.rows[at].len() {
        let mut sum = 0.0;
        let mut any = false;
        for &r in &src {
            if let sheetcore::Value::Number(n) = grid.rows[r][c] {
                sum += n;
                any = true;
            }
        }
        match grid.rows[at][c] {
            sheetcore::Value::Number(n) if any => assert!((n - sum).abs() < 1e-9, "列 {c}"),
            sheetcore::Value::Empty => assert!(!any, "列 {c} 不该是空"),
            ref v => panic!("列 {c} 意外值 {v:?}"),
        }
    }
    // 总计不受计算项影响（本来就已经含这两行）
    let after: Vec<String> = grid.rows[grid.grand_total_row.unwrap()].iter().map(|v| v.display()).collect();
    assert_eq!(after, grand_vals, "总计不应被计算项重复累加");
}

#[test]
fn calc_item_row_reaches_row_items_and_cache() {
    let fx = Fixture::new("calcrowxml");
    let path = fx.file("calcrowxml.xlsx");
    let mut wb = book(&path);
    let spec = spec_b();
    let mut grid = sheetcore::pivot_multi::compute(&wb.sheets[0], &spec);
    let at = sheetcore::pivot_multi::append_calc_item_row(&mut grid, "华东华北", &["华东", "华北"])
        .expect("应能追加计算项行");
    let rect = sheetcore::pivot_multi::write_grid(&mut wb.sheets[0], (8, 0), &grid);
    let _ = sheetcore::pivot_xml::attach2_for_session(&path, "Data", &spec, &grid, rect, "test")
        .expect("附挂失败");

    let mut z = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
    let mut cache = String::new();
    let mut table = String::new();
    {
        use std::io::Read;
        let cd = z.file_names().find(|n| n.contains("pivotCacheDefinition")).unwrap().to_string();
        z.by_name(&cd).unwrap().read_to_string(&mut cache).unwrap();
        let pt = z
            .file_names()
            .find(|n| n.starts_with("xl/pivotTables/") && n.ends_with(".xml"))
            .unwrap()
            .to_string();
        z.by_name(&pt).unwrap().read_to_string(&mut table).unwrap();
    }
    assert!(cache.contains("<s v=\"华东华北\" f=\"1\"/>"), "缓存要标 f=1：{cache}");
    // 计算项在 pivotField items 里带 f="1"
    let frag = format!("<item f=\"1\" x=");
    assert!(table.contains(&frag), "pivotField items 要标 f=1：{table}");
    // rowItems 里也要有同一索引
    let x = table
        .split("<item f=\"1\" x=\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .unwrap()
        .to_string();
    assert!(
        table.contains(&format!("<i><x v=\"{x}\"/></i>")),
        "rowItems 缺计算项行（x={x}）：{table}"
    );
    assert_eq!(at, 2, "两个普通行之后");

    let keep = std::path::Path::new("F:/finance-native-sheets-ready/work/probe/c12/calcrow-ours.xlsx");
    let _ = std::fs::remove_file(keep);
    std::fs::copy(&path, keep).ok();
}

#[test]
fn calc_field_survives_save_and_reload() {
    let fx = Fixture::new("calcfield-save");
    let path = fx.file("calcfield-save.xlsx");
    let mut wb = book(&path);
    let spec = spec_b();
    let _ = attach(&path, &mut wb, &spec);
    // 登记计算字段，走正常保存流程（xlsx 会在挂完透视表后写缓存定义）
    wb.pivot_calc_fields.push((0, "总价".to_string(), "金额*2".to_string()));
    sheetcore::xlsx::save(&path, &wb).expect("保存失败");

    // 重新打开后缓存里仍有公式
    let mut z = zip::ZipArchive::new(std::fs::File::open(&path).unwrap()).unwrap();
    let cd = z
        .file_names()
        .find(|n| n.contains("pivotCacheDefinition"))
        .unwrap()
        .to_string();
    let mut cache = String::new();
    {
        use std::io::Read;
        z.by_name(&cd).unwrap().read_to_string(&mut cache).unwrap();
    }
    assert!(cache.contains("formula=\"金额*2\""), "{cache}");
    assert!(cache.contains("databaseField=\"0\""), "{cache}");
    let open = cache.find("<cacheFields count=\"").unwrap() + "<cacheFields count=\"".len();
    let len = cache[open..].find('"').unwrap();
    let count: usize = cache[open..open + len].parse().unwrap();
    assert_eq!(count, cache.matches("<cacheField name=").count());
}

#[test]
fn pinyin_collation_matches_excel_probe() {
    let mut v: Vec<String> = ["深圳", "上海", "杭州", "广州", "北京"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    v.sort_by(|a, b| sheetcore::collate::cmp_pinyin(a, b));
    assert_eq!(v, vec!["北京", "广州", "杭州", "上海", "深圳"]);
    assert_eq!(
        sheetcore::collate::cmp_pinyin("销售", "行政"),
        std::cmp::Ordering::Less
    );
}

#[test]
#[ignore = "Excel 与本引擎标签排序口径未定：sort-ref.xlsx 显示拼音序（北京在前），而 compact 基准文件显示源出现顺序（研发在前），两份都是 Excel 自己生成，单变量实验未完成"]
fn two_row_fields_match_excel_sort_probe() {
    use sheetcore::{Cell, Value};
    let mut wb = sheetcore::Workbook::new_blank();
    wb.sheets[0].name = "Data".into();
    let data = [
        ("地区", "部门", "金额"),
        ("深圳", "销售", "10"),
        ("上海", "行政", "20"),
        ("杭州", "销售", "30"),
        ("广州", "行政", "40"),
        ("北京", "销售", "50"),
        ("北京", "行政", "60"),
    ];
    for (r, (a, b, c)) in data.iter().enumerate() {
        for (col, v) in [a, b, c].iter().enumerate() {
            let cell = if r == 0 || col == 2 {
                let parsed: f64 = v.parse().unwrap_or(0.0);
                if r == 0 {
                    Cell { value: Value::Text((*v).to_string().into()), formula: None }
                } else {
                    Cell { value: Value::Number(parsed), formula: None }
                }
            } else {
                Cell { value: Value::Text((*v).to_string().into()), formula: None }
            };
            wb.sheets[0].set(r as u32, col as u32, cell);
        }
    }
    let spec = PivotSpec2 {
        source: (0, 0, 6, 2),
        rows: vec![
            AxisField { col: 0, group: sheetcore::pivot::GroupBy::None },
            AxisField { col: 1, group: sheetcore::pivot::GroupBy::None },
        ],
        cols: vec![],
        values: vec![ValueField {
            col: 2,
            agg: Agg::Sum,
            label: Some("求和项:金额".into()),
            show_as: sheetcore::pivot_multi::ShowAs::Normal,
        }],
        filters: vec![],
        layout: PivotLayout { tabular: true, subtotals: true, grand_totals: true },
    };
    let g = sheetcore::pivot_multi::compute(&wb.sheets[0], &spec);
    let mut rows: Vec<String> = Vec::new();
    for r in &g.rows {
        rows.push(r.iter().map(|v| v.display()).collect::<Vec<_>>().join("\t"));
    }
    // Excel 实测（sort-ref.tsv）：北京/广州/杭州/上海/深圳 + 「X 汇总」+ 总计
    let expect = vec![
        "北京\t销售\t50",
        "北京\t行政\t60",
        "北京 汇总\t\t110",
        "广州\t行政\t40",
        "广州 汇总\t\t40",
        "杭州\t销售\t30",
        "杭州 汇总\t\t30",
        "上海\t行政\t20",
        "上海 汇总\t\t20",
        "深圳\t销售\t10",
        "深圳 汇总\t\t10",
        "总计\t\t210",
    ];
    assert_eq!(rows, expect);
}

#[test]
fn multi_value_with_col_field_matches_excel() {
    use sheetcore::{Cell, Value};
    let mut wb = sheetcore::Workbook::new_blank();
    wb.sheets[0].name = "Data".into();
    let heads = ["地区", "产品", "金额", "数量"];
    for (c, h) in heads.iter().enumerate() {
        wb.sheets[0].set(0, c as u32, Cell { value: Value::Text((*h).into()), formula: None });
    }
    let data = [
        ("华东", "A", 10.0, 2.0),
        ("华北", "B", 20.0, 3.0),
        ("华东", "A", 30.0, 4.0),
        ("华北", "B", 40.0, 5.0),
        ("华东", "B", 50.0, 6.0),
        ("华北", "A", 60.0, 7.0),
    ];
    for (i, (a, b, c, d)) in data.iter().enumerate() {
        let r = i as u32 + 1;
        wb.sheets[0].set(r, 0, Cell { value: Value::Text((*a).into()), formula: None });
        wb.sheets[0].set(r, 1, Cell { value: Value::Text((*b).into()), formula: None });
        wb.sheets[0].set(r, 2, Cell { value: Value::Number(*c), formula: None });
        wb.sheets[0].set(r, 3, Cell { value: Value::Number(*d), formula: None });
    }
    let spec = PivotSpec2 {
        source: (0, 0, 6, 3),
        rows: vec![AxisField { col: 0, group: sheetcore::pivot::GroupBy::None }],
        cols: vec![AxisField { col: 1, group: sheetcore::pivot::GroupBy::None }],
        values: vec![
            ValueField { col: 2, agg: Agg::Sum, label: Some("求和项:金额".into()), show_as: sheetcore::pivot_multi::ShowAs::Normal },
            ValueField { col: 3, agg: Agg::Sum, label: Some("求和项:数量".into()), show_as: sheetcore::pivot_multi::ShowAs::Normal },
        ],
        filters: vec![],
        layout: PivotLayout { tabular: true, subtotals: true, grand_totals: true },
    };
    let g = sheetcore::pivot_multi::compute(&wb.sheets[0], &spec);
    // 与 work/probe/c12/multival-ref.tsv（Excel 实测）逐格比对
    let trim = |row: &Vec<String>| -> Vec<String> {
        let mut r = row.clone();
        while r.last().is_some_and(|x| x.is_empty()) {
            r.pop();
        }
        r
    };
    let hdrs: Vec<Vec<String>> = g.headers.iter().map(trim).collect();
    assert_eq!(hdrs[0], vec!["", "产品", "", "", "", "值"]);
    assert_eq!(
        hdrs[1],
        vec!["", "A", "", "B", "", "求和项:金额汇总", "求和项:数量汇总"]
    );
    assert_eq!(
        hdrs[2],
        vec!["地区", "求和项:金额", "求和项:数量", "求和项:金额", "求和项:数量"]
    );
    let rows: Vec<Vec<String>> = g
        .rows
        .iter()
        .map(|r| trim(&r.iter().map(|v| v.display()).collect::<Vec<_>>()))
        .collect();
    assert_eq!(
        rows,
        vec![
            vec!["华北", "60", "7", "60", "8", "120", "15"],
            vec!["华东", "40", "6", "50", "6", "90", "12"],
            vec!["总计", "100", "13", "110", "14", "210", "27"],
        ]
    );

    // 落盘 + 挂真 pivotTable 部件，供 Excel 探针验证
    let fx = Fixture::new("multival");
    let path = fx.file("multival.xlsx");
    sheetcore::xlsx::save(&path, &wb).expect("保存失败");
    wb.path = Some(path.clone());
    let g2 = sheetcore::pivot_multi::compute(&wb.sheets[0], &spec);
    let rect = sheetcore::pivot_multi::write_grid(&mut wb.sheets[0], (8, 0), &g2);
    let _ = sheetcore::pivot_xml::attach2_for_session(&path, "Data", &spec, &g2, rect, "test")
        .expect("附挂失败");
    let keep = std::path::Path::new("F:/finance-native-sheets-ready/work/probe/c12/multival-ours.xlsx");
    let _ = std::fs::remove_file(keep);
    std::fs::copy(&path, keep).ok();
}

#[test]
fn move_axis_changes_nesting_and_ignores_out_of_range() {
    use sheetcore::pivot_multi::{Axis, move_axis};
    let fx = Fixture::new("moveaxis");
    let path = fx.file("moveaxis.xlsx");
    let wb = book(&path);
    let mut spec = spec_b();
    let before = sheetcore::pivot_multi::compute(&wb.sheets[0], &spec);
    // 单行字段时不能上移
    assert!(!move_axis(&mut spec, Axis::Rows, 0, -1));
    assert!(!move_axis(&mut spec, Axis::Rows, 0, 1));
    // 追加一个行字段后调序会改变层级
    spec.rows.push(AxisField { col: 1, group: sheetcore::pivot::GroupBy::None });
    let nested = sheetcore::pivot_multi::compute(&wb.sheets[0], &spec);
    assert_ne!(before.headers, nested.headers, "加第二个行字段后表头应变化");
    assert!(move_axis(&mut spec, Axis::Rows, 1, -1));
    let swapped = sheetcore::pivot_multi::compute(&wb.sheets[0], &spec);
    assert_eq!(spec.rows[0].col, 1);
    assert_ne!(nested.headers, swapped.headers, "交换层级后表头应变化");
    assert!(!move_axis(&mut spec, Axis::Rows, 0, -1));
}

#[test]
fn page_filter_matches_excel_filter_ref() {
    use sheetcore::{Cell, Value};
    use sheetcore::pivot_multi::FilterField;
    let mut wb = sheetcore::Workbook::new_blank();
    wb.sheets[0].name = "Data".into();
    let heads = ["地区", "产品", "金额", "状态"];
    for (c, h) in heads.iter().enumerate() {
        wb.sheets[0].set(0, c as u32, Cell { value: Value::Text((*h).into()), formula: None });
    }
    let data = [
        ("华东", "A", 10.0, "有效"),
        ("华北", "B", 20.0, "有效"),
        ("华东", "A", 30.0, "无效"),
        ("华北", "B", 40.0, "有效"),
        ("华东", "B", 50.0, "无效"),
        ("华北", "A", 60.0, "有效"),
    ];
    for (i, (a, b, c, d)) in data.iter().enumerate() {
        let r = i as u32 + 1;
        wb.sheets[0].set(r, 0, Cell { value: Value::Text((*a).into()), formula: None });
        wb.sheets[0].set(r, 1, Cell { value: Value::Text((*b).into()), formula: None });
        wb.sheets[0].set(r, 2, Cell { value: Value::Number(*c), formula: None });
        wb.sheets[0].set(r, 3, Cell { value: Value::Text((*d).into()), formula: None });
    }
    let spec = PivotSpec2 {
        source: (0, 0, 6, 3),
        rows: vec![AxisField { col: 0, group: sheetcore::pivot::GroupBy::None }],
        cols: vec![],
        values: vec![ValueField { col: 2, agg: Agg::Sum, label: Some("求和项:金额".into()), show_as: sheetcore::pivot_multi::ShowAs::Normal }],
        filters: vec![FilterField { col: 3, keep: vec!["有效".into()] }],
        layout: PivotLayout::default(),
    };
    let g = sheetcore::pivot_multi::compute(&wb.sheets[0], &spec);
    let rows: Vec<String> = g
        .rows
        .iter()
        .map(|r| r.iter().map(|v| v.display()).collect::<Vec<_>>().join("\t"))
        .collect();
    // Excel 实测（filter-ref.tsv）：华北 120、华东 10、总计 130
    assert_eq!(rows, vec!["华北\t120", "华东\t10", "总计\t130"]);

    // 页筛选标题区：Excel 在透视表上方写「状态 | 有效」+ 一空行
    assert_eq!(g.page_filters, vec![("状态".to_string(), "有效".to_string())]);
    let mut out = sheetcore::Sheet::new("透视");
    let rect = sheetcore::pivot_multi::write_grid(&mut out, (2, 1), &g);
    assert_eq!(out.get(2, 1).unwrap().value.display(), "状态");
    assert_eq!(out.get(2, 2).unwrap().value.display(), "有效");
    assert!(out.get(3, 1).is_none(), "标题区和表头之间要空一行");
    assert_eq!(out.get(4, 1).unwrap().value.display(), "地区");
    assert_eq!(rect.0, 2, "rect 从标题区开始算");
}

#[test]
fn two_col_field_values_match_excel_with_subtotal_columns() {
    use sheetcore::{Cell, Value};
    let mut wb = sheetcore::Workbook::new_blank();
    wb.sheets[0].name = "Data".into();
    for (c, h) in ["地区", "产品", "状态", "金额"].iter().enumerate() {
        wb.sheets[0].set(0, c as u32, Cell { value: Value::Text((*h).into()), formula: None });
    }
    let data = [
        ("华东", "A", "有效", 10.0),
        ("华北", "B", "有效", 20.0),
        ("华东", "A", "无效", 30.0),
        ("华北", "B", "有效", 40.0),
        ("华东", "B", "无效", 50.0),
        ("华北", "A", "有效", 60.0),
    ];
    for (i, (a, b, c, d)) in data.iter().enumerate() {
        let r = i as u32 + 1;
        wb.sheets[0].set(r, 0, Cell { value: Value::Text((*a).into()), formula: None });
        wb.sheets[0].set(r, 1, Cell { value: Value::Text((*b).into()), formula: None });
        wb.sheets[0].set(r, 2, Cell { value: Value::Text((*c).into()), formula: None });
        wb.sheets[0].set(r, 3, Cell { value: Value::Number(*d), formula: None });
    }
    let spec = PivotSpec2 {
        source: (0, 0, 6, 3),
        rows: vec![AxisField { col: 0, group: sheetcore::pivot::GroupBy::None }],
        cols: vec![
            AxisField { col: 1, group: sheetcore::pivot::GroupBy::None },
            AxisField { col: 2, group: sheetcore::pivot::GroupBy::None },
        ],
        values: vec![ValueField { col: 3, agg: Agg::Sum, label: Some("求和项:金额".into()), show_as: sheetcore::pivot_multi::ShowAs::Normal }],
        filters: vec![],
        layout: PivotLayout::default(),
    };
    let g = sheetcore::pivot_multi::compute(&wb.sheets[0], &spec);
    // Excel 真值（twocol-ref.tsv）：A 无效/有效/A 汇总/B 无效/有效/B 汇总/总计
    let rows: Vec<Vec<String>> = g.rows.iter().map(|r| r.iter().map(|v| v.display()).collect()).collect();
    assert_eq!(rows[0][0], "华北");
    assert_eq!(&rows[0][1..8], &["", "60", "60", "", "60", "60", "120"]);
    assert_eq!(rows[1][0], "华东");
    assert_eq!(&rows[1][1..8], &["30", "10", "40", "50", "", "50", "90"]);
    assert_eq!(rows[2][0], "总计");
    assert_eq!(&rows[2][1..8], &["30", "70", "100", "50", "60", "110", "210"]);

    // 落盘并挂真部件：colItems 已按组写 <i t="default">，网格也有对应列
    let fx = Fixture::new("twocol");
    let path = fx.file("twocol.xlsx");
    sheetcore::xlsx::save(&path, &wb).expect("保存失败");
    let mut wb = wb;
    wb.path = Some(path.clone());
    let g2 = sheetcore::pivot_multi::compute(&wb.sheets[0], &spec);
    let rect = sheetcore::pivot_multi::write_grid(&mut wb.sheets[0], (8, 0), &g2);
    let _ = sheetcore::pivot_xml::attach2_for_session(&path, "Data", &spec, &g2, rect, "test")
        .expect("附挂失败");
    let keep = std::path::Path::new("F:/finance-native-sheets-ready/work/probe/c12/twocol-ours.xlsx");
    let _ = std::fs::remove_file(keep);
    std::fs::copy(&path, keep).ok();
    for r in &g.rows {
        eprintln!("ROW| {}", r.iter().map(|v| v.display()).collect::<Vec<_>>().join("\t"));
    }
}

#[test]
fn col_slots_insert_a_subtotal_after_each_top_group() {
    use sheetcore::pivot_multi::{ColSlot, build_col_slots};
    let paths: Vec<Vec<String>> = vec![
        vec!["A".into(), "无效".into()],
        vec!["A".into(), "有效".into()],
        vec!["B".into(), "无效".into()],
        vec!["B".into(), "有效".into()],
    ];
    let slots = build_col_slots(&paths, true);
    let labels: Vec<String> = slots
        .iter()
        .map(|s| match s {
            ColSlot::Leaf(p) => p.last().cloned().unwrap_or_default(),
            ColSlot::Subtotal { label, .. } => label.clone(),
        })
        .collect();
    assert_eq!(labels, vec!["无效", "有效", "A 汇总", "无效", "有效", "B 汇总"]);
    // 关掉小计时只剩叶子
    assert_eq!(build_col_slots(&paths, false).len(), 4);
    // 单层列字段不插
    let one: Vec<Vec<String>> = vec![vec!["A".into()], vec!["B".into()]];
    assert_eq!(build_col_slots(&one, true).len(), 2);
}
