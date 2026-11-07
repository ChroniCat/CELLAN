fn main() {
    let p = std::path::Path::new("F:/Finance/work/real_macro.xlsm");
    let bytes = sheetcore::vba::read_vba(p).expect("no vba");
    let c = sheetcore::cfb::Compound::open(bytes).expect("ole");
    let m = c.stream_by_path("VBA/Module1").or_else(|| c.stream_by_path("Module1")).unwrap();
    std::fs::write("F:/Finance/work/module1.bin", &m).unwrap();
    println!("写出 {} 字节", m.len());
}
