//! 结构性改动后的三处引用：定义名、图表系列、透视表源区域
use sheetcore::model::{Cell, Value, Workbook};
use sheetcore::structural::Structural;

#[test]
fn defined_names_shift() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "数据".into();
    wb.names.push(("销售额".into(), None, "数据!$B$2:$B$9".into()));
    wb.names.push(("本表格".into(), Some(0), "$A$1".into()));
    sheetcore::structural::apply_structural_wb(&mut wb, 0, &Structural::InsertRows { at: 0, n: 2 });
    assert_eq!(wb.names[0].2, "数据!$B$4:$B$11", "带表名的名称要跟着搬");
    assert!(wb.names_dirty);
}

#[test]
#[ignore = "requires optional synthetic fixtures under tests/test-work"]
fn chart_series_refs_shift_and_are_written_back() {
    let src = std::path::Path::new("tests/test-work/table.xlsx");
    assert!(src.exists(), "required test fixture is missing: {}", src.display());
    let mut wb = sheetcore::xlsx::load(src).unwrap();
    let name = wb.sheet(0).name.clone();
    let region = sheetcore::sort_filter::current_region(wb.sheet(0), 1, 1);
    let ch = sheetcore::chart_write::chart_from_selection(
        wb.sheet(0),
        &name,
        region,
        "测试",
        sheetcore::charts::ChartKind::Column,
    );
    wb.sheet_mut(0).charts.push(ch.clone());
    let out = std::path::PathBuf::from(format!("tests/test-work/refshift_{}.xlsx", std::process::id()));
    let _ = std::fs::remove_file(&out);
    sheetcore::xlsx::save(&out, &wb).unwrap();
    sheetcore::chart_write::insert_chart(&out, &name, &ch).unwrap();

    // 重新读入（图表带部件路径），插两行，存盘
    let mut wb2 = sheetcore::xlsx::load(&out).unwrap();
    assert!(!wb2.sheet(0).charts.is_empty(), "先要读到图表");
    let before = wb2.sheet(0).charts[0].series[0].val_ref.clone();
    sheetcore::structural::apply_structural_wb(&mut wb2, 0, &Structural::InsertRows { at: 0, n: 2 });
    let after = wb2.sheet(0).charts[0].series[0].val_ref.clone();
    assert_ne!(before, after, "系列引用应被改写: {before} -> {after}");
    assert!(wb2.sheet(0).charts_dirty);
    sheetcore::xlsx::save(&out, &wb2).unwrap();

    // 文件里的 chart 部件也应换成新引用
    let zip = zip::ZipArchive::new(std::fs::File::open(&out).unwrap()).unwrap();
    let mut z = zip;
    let mut part = String::new();
    let names: Vec<String> = (0..z.len()).map(|i| z.name_for_index(i).unwrap().to_string()).collect();
    let cp = names.iter().find(|n| n.starts_with("xl/charts/chart")).unwrap().clone();
    use std::io::Read;
    z.by_name(&cp).unwrap().read_to_string(&mut part).unwrap();
    assert!(part.contains(&after), "chart 部件里没有新引用 {after}\n{part}");
    let _ = std::fs::remove_file(&out);
}

#[test]
fn pending_pivot_source_shifts() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "S".into();
    for r in 0..5u32 {
        wb.sheet_mut(0).set(r, 0, Cell { value: Value::Number(r as f64), formula: None });
        wb.sheet_mut(0).set(r, 1, Cell { value: Value::Number(1.0), formula: None });
    }
    let spec = sheetcore::pivot::PivotSpec {
        source: (0, 0, 4, 1),
        row_field: 0,
        col_field: None,
        value_field: 1,
        agg: sheetcore::pivot::Agg::Sum,
        group: sheetcore::pivot::GroupBy::None,
        exclude: Vec::new(),
};
    let res = sheetcore::pivot::compute(wb.sheet(0), &spec);
    wb.pivots_pending.push((0, spec, res, (0, 4, 0, 5), "数值".into()));
    sheetcore::structural::apply_structural_wb(&mut wb, 0, &Structural::InsertRows { at: 0, n: 3 });
    assert_eq!(wb.pivots_pending[0].1.source, (3, 0, 7, 1), "透视表源区域要跟着下移");
    assert_eq!(wb.pivot_shifts.len(), 1, "要给保存留一条记录");
}

#[test]
#[ignore = "requires optional synthetic fixtures under tests/test-work"]
fn saving_keeps_chart_parts() {
    // 回归：umya 只写它懂的部件，图表曾在「读入→保存」后整份消失
    let src = std::path::Path::new("tests/test-work/chart.xlsx");
    assert!(src.exists(), "required test fixture is missing: {}", src.display());
    let wb = sheetcore::xlsx::load(src).unwrap();
    let out = std::path::PathBuf::from(format!("tests/test-work/keepparts_{}.xlsx", std::process::id()));
    let _ = std::fs::remove_file(&out);
    sheetcore::xlsx::save(&out, &wb).unwrap();
    let z = zip::ZipArchive::new(std::fs::File::open(&out).unwrap()).unwrap();
    let names: Vec<String> = (0..z.len()).map(|i| z.name_for_index(i).unwrap().to_string()).collect();
    assert!(names.iter().any(|n| n.starts_with("xl/charts/chart")), "图表部件丢了：{names:?}");
    assert!(names.iter().any(|n| n.contains("drawings/_rels")), "drawing 关系丢了");
    // drawing 里的锚点也还在（否则图表有部件但显示不出来）
    let mut z = zip::ZipArchive::new(std::fs::File::open(&out).unwrap()).unwrap();
    let mut drawing = String::new();
    use std::io::Read;
    z.by_name("xl/drawings/drawing1.xml").unwrap().read_to_string(&mut drawing).unwrap();
    assert!(drawing.contains("CellAnchor"), "锚点丢了: {drawing}");
    // 读回来时图表还在（曾经因为 drawing 被写成空壳而丢失）
    let back = sheetcore::xlsx::load(&out).unwrap();
    assert!(
        !back.sheet(0).charts.is_empty(),
        "保存后重开应该还能读到图表"
    );
    let _ = std::fs::remove_file(&out);
}

#[test]
#[ignore = "requires optional synthetic fixtures under tests/test-work"]
fn foreign_charts_survive_round_trip() {
    // 别人做的图表（openpyxl 两个图）保存后必须还在，且能读回来
    let src = std::path::Path::new("tests/test-work/chart.xlsx");
    assert!(src.exists(), "required test fixture is missing: {}", src.display());
    let wb = sheetcore::xlsx::load(src).unwrap();
    let n_before = wb.sheet(0).charts.len();
    assert!(n_before > 0);
    let out = std::path::PathBuf::from(format!("tests/test-work/foreign_{}.xlsx", std::process::id()));
    let _ = std::fs::remove_file(&out);
    sheetcore::xlsx::save(&out, &wb).unwrap();
    let back = sheetcore::xlsx::load(&out).unwrap();
    assert_eq!(back.sheet(0).charts.len(), n_before, "保存后图表数量变了");
    let _ = std::fs::remove_file(&out);
}
