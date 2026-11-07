fn main() {
    let p = std::path::Path::new("F:/Finance/work/real_macro.xlsm");
    let bytes = sheetcore::vba::read_vba(p).expect("no vba");
    let c = sheetcore::cfb::Compound::open(bytes).expect("ole");
    let m = c.stream_by_path("VBA/Module1").or_else(|| c.stream_by_path("Module1")).expect("module stream");
    println!("Module1 长度 {}", m.len());
    // 找所有「0x01 + header 的 sig==3」的候选起点
    let mut hits = Vec::new();
    for i in 0..m.len().saturating_sub(2) {
        if m[i] == 0x01 {
            let h = u16::from_le_bytes([m[i + 1], m[i + 2]]);
            let sig = (h >> 12) & 0x7;
            if sig == 3 {
                hits.push((i, h & 0x0FFF, (h >> 15) & 1));
            }
        }
    }
    println!("候选压缩块起点 {:?}", &hits[..hits.len().min(5)]);
    for (i, size, comp) in hits.iter().take(2) {
        let start = i + 3;
        let end = (start + *size as usize + 3).min(m.len());
        let mut n = [0u8; 64];
        let take = (end - start).min(64);
        n[..take].copy_from_slice(&m[start..start + take]);
        println!("起点 {i}: size={size} 压缩位={comp} 数据头 {:02x?}", &n[..take.min(16)]);
    }
}
