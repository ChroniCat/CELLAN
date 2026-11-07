fn main() {
    let p = std::env::args().nth(1).unwrap();
    let wb = sheetcore::xlsx::load(std::path::Path::new(&p)).unwrap();
    println!("表 {} 图表 {}", wb.sheet(0).name, wb.sheet(0).charts.len());
    let dir = sheetcore::patch::sheet_targets(std::path::Path::new(&p)).unwrap();
    println!("sheet targets {:?}", dir);
}
