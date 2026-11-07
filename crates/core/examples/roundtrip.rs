fn main() {
    let src = std::path::Path::new("F:/Finance/work/compat_in.xlsx");
    let out = std::path::Path::new("F:/Finance/work/compat_out.xlsx");
    let _ = std::fs::remove_file(out);
    let t0 = std::time::Instant::now();
    let wb = sheetcore::xlsx::load(src).expect("load");
    println!("读入 {:.2}s 表数={}", t0.elapsed().as_secs_f64(), wb.sheets.len());
    let s = &wb.sheets[0];
    println!("合并={} 表对象={} 图片={} 图表={} 条件格式块={} 验证={} 公式格={}",
        s.merges.len(), s.tables.len(), s.pictures.len(), s.charts.len(),
        s.cf.blocks.len(), s.dv.len(), s.iter().filter(|(_,c)| c.formula.is_some()).count());
    let t1 = std::time::Instant::now();
    sheetcore::xlsx::save(out, &wb).expect("save");
    println!("保存 {:.2}s -> {}", t1.elapsed().as_secs_f64(), out.display());
}
