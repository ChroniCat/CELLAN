// 调试：打印样式表与合并区域
fn main() {
    let path = std::env::args().nth(1).expect("用法: dbg_style <xlsx>");
    let wb = sheetcore::xlsx::load(std::path::Path::new(&path)).expect("打开失败");
    println!("样式表 {} 条", wb.styles.len());
    for si in 0..wb.styles.len() {
        let s = wb.styles.get(si as u16);
        println!("  #{si}: bold={} italic={} size={} color={:?} fill={:?} halign={} valign={} fmt={:?}",
            s.bold, s.italic, s.size, s.color, s.fill, s.halign, s.valign, s.format);
    }
    for sh in &wb.sheets {
        println!("表 {} 合并 {} 处 有样式格 {}", sh.name, sh.merges.len(), sh.styles.len());
        for m in sh.merges.iter().take(8) {
            println!("   merge {:?}", m);
        }
        let mut keys: Vec<_> = sh.styles.iter().collect();
        keys.sort_by_key(|(k, _)| (k.row, k.col));
        for (k, v) in keys.iter().take(14) {
            println!("   格 ({},{}) → 样式 #{}", k.row, k.col, v);
        }
    }
}
