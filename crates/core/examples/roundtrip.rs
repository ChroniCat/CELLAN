fn main() {
    let args: Vec<String> = std::env::args().collect();
    let src = std::path::PathBuf::from(args.get(1).cloned().unwrap_or_else(|| "F:/Finance/work/compat_in.xlsx".into()));
    let out = std::path::PathBuf::from(args.get(2).cloned().unwrap_or_else(|| "F:/Finance/work/compat_out.xlsx".into()));
    let _ = std::fs::remove_file(&out);

    let t0 = std::time::Instant::now();
    let wb = sheetcore::xlsx::load(&src).expect("load");
    let load = t0.elapsed().as_secs_f64();

    let mut cells = 0usize;
    let mut formulas = 0usize;
    for s in wb.sheets.iter() {
        for (_, c) in s.iter() {
            cells += 1;
            if c.formula.is_some() { formulas += 1 }
        }
    }
    println!("读入 {load:.2}s  表 {}  非空格 {cells}  公式 {formulas}", wb.sheets.len());
    for s in wb.sheets.iter().take(8) {
        println!("  {} 图表{} 图片{} 表对象{} 条件格式{} 验证{}",
            s.name, s.charts.len(), s.pictures.len(), s.tables.len(), s.cf.blocks.len(), s.dv.len());
    }

    let t1 = std::time::Instant::now();
    sheetcore::xlsx::save(&out, &wb).expect("save");
    println!("保存 {:.2}s -> {} ({} MB)", t1.elapsed().as_secs_f64(), out.display(),
        std::fs::metadata(&out).map(|m| m.len()/1048576).unwrap_or(0));
}
