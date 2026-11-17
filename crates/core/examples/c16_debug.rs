fn main() -> anyhow::Result<()> {
    let p = std::env::args().nth(1).unwrap();
    let wb = sheetcore::xlsx::load(std::path::Path::new(&p))?;
    let sh = &wb.sheets[0];
    let lay = sheetcore::print::paginate(sh, &sh.page);
    println!("scale={} page={:?}", lay.scale, lay.page_size);
    for r in 0..5 { println!("row{} h={}", r, sh.rows.size(r)); }
    for c in 0..6 { println!("col{} w={}", c, sh.cols.size(c)); }
    println!("title_rows={:?} title_cols={:?}", sh.page.title_rows, sh.page.title_cols);
    println!("default_row={:?}", sheetcore::layout::DEFAULT_ROW_H);
    Ok(())
}
