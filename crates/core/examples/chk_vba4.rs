fn main() {
    let p = std::path::Path::new("F:/Finance/work/real_macro.xlsm");
    let bytes = sheetcore::vba::read_vba(p).expect("no vba");
    let mods = sheetcore::vba_src::read_modules(&bytes).expect("parse");
    for m in mods.iter() {
        if m.name == "Module1" {
            println!("=== {} ({} 行) ===", m.name, m.lines);
            println!("{}", m.source);
        }
    }
}
