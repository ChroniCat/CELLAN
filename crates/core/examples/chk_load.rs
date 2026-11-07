use calamine::{open_workbook_auto, Reader};
fn main() {
    let p = std::env::args().nth(1).unwrap();
    let mut r = open_workbook_auto(&p).unwrap();
    let names = r.sheet_names().to_owned();
    println!("表: {names:?}");
    for n in names {
        println!("  开始 {n}"); use std::io::Write; std::io::stdout().flush().ok();
        let t = std::time::Instant::now();
        match r.worksheet_range(&n) {
            Ok(rg) => println!("  {n}: {} 行 {} 列  用时 {:.2}s", rg.height(), rg.width(), t.elapsed().as_secs_f64()),
            Err(e) => println!("  {n}: 失败 {e}"),
        }
    }
}
