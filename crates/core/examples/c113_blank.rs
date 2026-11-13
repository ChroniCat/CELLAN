fn main() {
    let out = std::env::args().nth(1).expect("output path");
    let wb = sheetcore::model::Workbook::new_blank();
    sheetcore::xlsx::save(std::path::Path::new(&out), &wb).expect("save blank");
    println!("{out}");
}
