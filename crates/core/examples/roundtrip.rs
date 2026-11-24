fn main() {
    let args: Vec<String> = std::env::args().collect();
    let src = std::path::PathBuf::from(args.get(1).cloned().unwrap_or_else(|| "F:/Finance/work/compat_in.xlsx".into()));
    let out = std::path::PathBuf::from(args.get(2).cloned().unwrap_or_else(|| "F:/Finance/work/compat_out.xlsx".into()));
    let _ = std::fs::remove_file(&out);

    let t0 = std::time::Instant::now();
    let mut wb = sheetcore::xlsx::load(&src).expect("load");
    if let Ok(spec) = std::env::var("SC_EDIT") {
        // 形如 "表名!A1=值"
        if let Some((left, val)) = spec.split_once('=') {
            if let Some((sheet, addr)) = left.split_once('!') {
                let idx = wb.sheets.iter().position(|s| s.name == sheet).unwrap_or(0);
                let rc = sheetcore::sparse::parse_ref(addr.trim()).expect("地址");
                let mut cell = wb.sheets[idx].get(rc.0, rc.1).map(|c| c.into_owned()).unwrap_or_default();
                cell.value = sheetcore::model::Value::Text(val.trim().into());
                cell.formula = None;
                wb.sheets[idx].set(rc.0, rc.1, cell);
                eprintln!("[SC_EDIT] {sheet}!{} = {}", addr.trim(), val.trim());
            }
        }
    }
    if let Ok(spec) = std::env::var("SC_ONLY") {
        let keep: Vec<String> = spec.split(',').map(|x| x.trim().to_string()).collect();
        let before = wb.sheets.len();
        wb.sheets.retain(|sh| keep.iter().any(|k| sh.name.contains(k.as_str())));
        eprintln!("[SC_ONLY] 保留 {} / {} 张表", wb.sheets.len(), before);
    }
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
