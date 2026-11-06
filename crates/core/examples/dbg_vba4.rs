fn main() {
    let data = std::fs::read("F:/Finance/work/vba_fixture2.bin").unwrap();
    let c = sheetcore::cfb::Compound::open(data).unwrap();
    let e = c.entries.iter().find(|e| e.name == "PROJECT").unwrap();
    let v = c.read_stream(e);
    println!("PROJECT 读出 {} 字节: {:?}", v.len(), String::from_utf8_lossy(&v[..v.len().min(60)]));
}
