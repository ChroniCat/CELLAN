use sheetcore::{cell_name, Workbook};

fn main() {
    let path = std::env::args().nth(1).expect("用法: dump <file.xlsx>");
    let mut wb: Workbook = sheetcore::xlsx::load(std::path::Path::new(&path)).expect("加载失败");
    println!("sheets: {}", wb.sheets.len());
    for (si, s) in wb.sheets.iter().enumerate() {
        println!("[{si}] {} : {} cells", s.name, s.cells.len());
        let mut keys: Vec<_> = s.cells.iter().map(|(k, v)| (*k, v.clone())).collect();
        keys.sort_by_key(|(k, _)| (k.row, k.col));
        for (k, cell) in keys.iter().take(40) {
            println!(
                "  {} = {:?} formula={:?}",
                cell_name(k.row, k.col),
                cell.value,
                cell.formula
            );
        }
    }
    sheetcore::formula::recalc_all(&mut wb);
    println!("--- after recalc ---");
    let s = &wb.sheets[0];
    let mut keys: Vec<_> = s.cells.iter().map(|(k, v)| (*k, v.clone())).collect();
    keys.sort_by_key(|(k, _)| (k.row, k.col));
    for (k, cell) in keys.iter().take(40) {
        if cell.formula.is_some() {
            println!("  {} => {:?}", cell_name(k.row, k.col), cell.value);
        }
    }
}
