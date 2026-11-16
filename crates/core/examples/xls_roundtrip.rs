//! 读入 .xls，按同一扩展名另存（走项目的 xls 写出路径）。
fn main() {
    let mut args = std::env::args().skip(1);
    let input = std::path::PathBuf::from(args.next().expect("缺少输入"));
    let output = std::path::PathBuf::from(args.next().expect("缺少输出"));
    let mut wb = sheetcore::xlsx::load(&input).expect("打开 .xls 失败");
    println!("loaded sheets={} first={}", wb.sheets.len(), wb.sheets[0].name);
    wb.path = Some(input);
    sheetcore::xlsx::save(&output, &wb).expect("另存 .xls 失败");
    println!("saved {}", output.display());
}
