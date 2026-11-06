fn main() {
    let data = std::fs::read("F:/Finance/work/vba_fixture.bin").unwrap();
    match sheetcore::cfb::Compound::open(data) {
        Ok(c) => {
            println!("目录项: {:?}", c.names());
            println!("PROJECT: {:?}", c.stream_by_path("PROJECT").map(|v| v.len()));
            println!("dir: {:?}", c.stream_by_path("dir").map(|v| v.len()));
            println!("Module1: {:?}", c.stream_by_path("Module1").map(|v| v.len()));
        }
        Err(e) => println!("打开失败: {e:#}"),
    }
}
