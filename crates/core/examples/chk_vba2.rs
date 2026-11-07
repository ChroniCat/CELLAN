fn main() {
    let p = std::path::Path::new("F:/Finance/work/real_macro.xlsm");
    let bytes = sheetcore::vba::read_vba(p).expect("no vba");
    let c = sheetcore::cfb::Compound::open(bytes.clone()).expect("ole");
    for name in c.names() {
        let base = name.split('[').next().unwrap_or("").to_string();
        if base.starts_with("Module") || base == "dir" || base == "PROJECT" {
            match c.stream_by_path(&base) {
                Some(s) => {
                    let head: String = s.iter().take(24).map(|b| format!("{b:02x}")).collect();
                    println!("{base}: {} 字节  头 {}", s.len(), head);
                }
                None => println!("{base}: 空"),
            }
        }
    }
}
