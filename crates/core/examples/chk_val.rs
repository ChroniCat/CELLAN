fn main() {
    let wb = sheetcore::xlsx::load(std::path::Path::new("F:/Finance/work/table.xlsx")).unwrap();
    let s = wb.sheet(0);
    for r in 0..6u32 {
        for c in 0..3u32 {
            if let Some(cell) = s.get(r, c) {
                println!("({r},{c}) = {:?}", cell.value);
            }
        }
    }
}
