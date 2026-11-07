fn main() {
    let p = std::path::Path::new("F:/Finance/work/pic.xlsx");
    let wb = sheetcore::xlsx::load(p).unwrap();
    for s in wb.sheets.iter() {
        println!("{}: {:?}", s.name, s.pictures);
    }
    if let Some(pic) = wb.sheets[0].pictures.first() {
        let bytes = sheetcore::pic::read_part(p, &pic.part);
        println!("部件 {} 字节 {:?}", pic.part, bytes.map(|b| b.len()));
    }
}
