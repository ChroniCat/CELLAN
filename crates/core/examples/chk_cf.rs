fn main() {
    let p = std::env::args().nth(1).unwrap_or_else(|| "F:/Finance/work/compat_in.xlsx".into());
    let wb = sheetcore::xlsx::load(std::path::Path::new(&p)).expect("load");
    for s in wb.sheets.iter() {
        if s.cf.blocks.is_empty() { continue; }
        println!("表 {} 共 {} 块", s.name, s.cf.blocks.len());
        for b in s.cf.blocks.iter() {
            for r in b.rules.iter() {
                println!("  {:?} ranges={:?}", r.kind, b.ranges);
            }
        }
    }
}
