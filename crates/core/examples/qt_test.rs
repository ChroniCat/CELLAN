use std::path::Path;
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (src, out, csv) = (&a[1], &a[2], &a[3]);
    let rows: u32 = a.get(4).and_then(|x| x.parse().ok()).unwrap_or(3);
    let cols: u32 = a.get(5).and_then(|x| x.parse().ok()).unwrap_or(3);
    let hdr: bool = a.get(6).map(|x| x != "0").unwrap_or(true);
    let _ = std::fs::remove_file(out);
    let mut wb = sheetcore::xlsx::load(Path::new(src)).expect("load");
    let sheet = wb.sheets[0].name.clone();
    sheetcore::external::import_csv(&mut wb.sheets[0], csv, ',', true, (0, 0)).expect("csv");
    wb.dirty = true;
    sheetcore::xlsx::save(Path::new(out), &wb).expect("save");
    match sheetcore::qtable::attach_csv(Path::new(out), &sheet, (0, 0), rows, cols, Path::new(csv), hdr, ',') {
        Ok(_) => println!("挂上查询表"),
        Err(e) => println!("失败: {e:#}"),
    }
}
