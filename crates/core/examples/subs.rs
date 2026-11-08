fn main() {
    let p = std::env::args().nth(1).unwrap_or("F:/Finance/work/real_macro.xlsm".into());
    let bytes = sheetcore::vba::read_vba(std::path::Path::new(&p)).expect("no vba");
    let mods = sheetcore::vba_src::read_modules(&bytes).unwrap_or_default();
    for m in mods {
        println!("== {} ({} 行)", m.name, m.lines);
        for line in m.source.lines().take(12) {
            println!("   {line}");
        }
    }
}
