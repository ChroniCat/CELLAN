use sheetcore::model::Workbook;
fn main() {
    let path = std::env::args().nth(1).expect("path");
    let wb = sheetcore::xlsx::load(std::path::Path::new(&path)).expect("load");
    let si = wb.sheets.iter().position(|s| s.name.starts_with("S4")).expect("S4");
    for s in wb.sheets.iter() {
        if !s.tables.is_empty() {
            for t in &s.tables {
                println!("表 {} 在 {} 范围 {:?} 列 {:?}", t.name, s.name, t.range, &t.columns[..t.columns.len().min(4)]);
            }
        }
    }
    let tl = sheetcore::table::load(std::path::Path::new(&path));
    println!("table::load 命中: {:?}", tl.iter().map(|(k,v)| (k.clone(), v.len())).collect::<Vec<_>>());
    let f = "SUM(StressOrders[含税人民币])";
    println!("展开: {:?}", sheetcore::table::expand_structured(&wb, si, f));
}
