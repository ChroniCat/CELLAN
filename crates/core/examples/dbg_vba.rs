fn main() {
    let p = std::env::args().nth(1).unwrap_or("F:/Finance/work/vba_fixture.bin".into());
    let data = std::fs::read(&p).unwrap();
    println!("读入 {} 字节", data.len());
    match sheetcore::vba_src::read_modules(&data) {
        Ok(mods) => {
            println!("模块数 {}", mods.len());
            for m in mods {
                println!("--- {} [{}] {} 行 ---", m.name, m.kind, m.lines);
                for line in m.source.lines().take(6) {
                    println!("    {line}");
                }
            }
        }
        Err(e) => println!("失败: {e:#}"),
    }
}
