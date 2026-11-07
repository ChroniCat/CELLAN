use sheetcore::model::Value;
fn parts(p: &std::path::Path) -> Vec<String> {
    let z = zip::ZipArchive::new(std::fs::File::open(p).unwrap()).unwrap();
    let mut v: Vec<String> = (0..z.len()).map(|i| z.name_for_index(i).unwrap().to_string()).collect();
    v.sort();
    v.into_iter().filter(|n| n.contains("chart")).collect()
}
fn main() {
    let src = std::path::Path::new("F:/Finance/work/table.xlsx");
    let out = std::path::Path::new("F:/Finance/work/flow.xlsx");
    let _ = std::fs::remove_file(out);
    let mut wb = sheetcore::xlsx::load(src).unwrap();
    let name = wb.sheet(0).name.clone();
    let region = sheetcore::sort_filter::current_region(wb.sheet(0), 1, 1);
    let ch = sheetcore::chart_write::chart_from_selection(wb.sheet(0), &name, region, "测试",
        sheetcore::charts::ChartKind::Column);
    sheetcore::xlsx::save(out, &wb).unwrap();
    sheetcore::chart_write::insert_chart(out, &name, &ch).unwrap();
    println!("插入后: {:?}", parts(out));
    let mut wb2 = sheetcore::xlsx::load(out).unwrap();
    println!("读到的图表数 {} 路径={:?}", wb2.sheet(0).charts.len(),
        wb2.sheet(0).charts.first().map(|c| c.part.clone()));
    println!("wb2.path={:?}", wb2.path);
    sheetcore::structural::apply_structural_wb(&mut wb2, 0,
        &sheetcore::structural::Structural::InsertRows { at: 0, n: 2 });
    sheetcore::xlsx::save(out, &wb2).unwrap();
    println!("存回后: {:?}", parts(out));
    let _ = Value::Empty;
}
