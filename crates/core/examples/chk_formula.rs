fn main() {
    let mut wb = sheetcore::xlsx::load(std::path::Path::new("F:/Finance/work/formula.xlsx")).unwrap();
    println!("B1 公式 = {:?}", wb.sheet(0).get(0, 1).and_then(|c| c.formula.clone()));
    sheetcore::structural::apply_structural_wb(
        &mut wb,
        0,
        &sheetcore::structural::Structural::InsertRows { at: 0, n: 2 },
    );
    println!("插入后:");
    for r in 0..6u32 {
        for c in 0..3u32 {
            if let Some(cell) = wb.sheet(0).get(r, c) {
                println!("  ({r},{c}) {:?} 公式={:?}", cell.value.display(), cell.formula);
            }
        }
    }
    println!("开始重算…");
    sheetcore::formula::recalc_all(&mut wb);
    println!("重算完成");
    for r in 0..6u32 {
        for c in 0..3u32 {
            if let Some(cell) = wb.sheet(0).get(r, c) {
                println!("  ({r},{c}) = {:?}", cell.value.display());
            }
        }
    }
}
