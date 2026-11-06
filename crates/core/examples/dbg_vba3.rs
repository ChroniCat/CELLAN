fn main() {
    let data = std::fs::read("F:/Finance/work/vba_fixture2.bin").unwrap();
    let c = sheetcore::cfb::Compound::open(data.clone()).unwrap();
    println!("mini_stream 长度={} 条目数={}", c.debug_mini_len(), c.entries.len());
    for e in c.entries.iter() {
        println!("  {} kind={} start={} size={}", e.name, e.kind, e.start, e.size);
    }
}
