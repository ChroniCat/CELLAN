fn main() {
    let arg = std::env::args().nth(1).unwrap_or_else(|| "F:/Finance/work/macro.xlsm".into());
    let p = std::path::Path::new(&arg);
    let bytes = match sheetcore::vba::read_vba(p) {
        Some(b) => b,
        None => { println!("没有宏部件"); return; }
    };
    println!("vbaProject.bin {} 字节", bytes.len());
    match sheetcore::cfb::Compound::open(bytes.clone()) {
        Ok(c) => println!("OLE 流: {:?}", c.names()),
        Err(e) => println!("OLE 解析失败: {e:#}"),
    }
    match sheetcore::vba_src::read_modules(&bytes) {
        Ok(list) => {
            println!("解出 {} 个模块", list.len());
            for m in list.iter().take(3) {
                println!("  {} ({} 行) 首行: {:?}", m.name, m.lines, m.source.lines().next().unwrap_or(""));
            }
        }
        Err(e) => println!("源码解析失败: {e:#}"),
    }
}
