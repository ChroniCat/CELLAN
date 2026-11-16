//! C12.4 保真探针：读入一份（Excel 存过的）带透视表的工作簿，再按我们的保存链路写出去。
//!
//! 用法：`cargo run -p sheetcore --example pivot_roundtrip -- <in.xlsx> <out.xlsx>`

fn main() {
    let mut args = std::env::args().skip(1);
    let input = args.next().expect("缺少输入文件");
    let output = args.next().expect("缺少输出文件");
    let input = std::path::PathBuf::from(input);
    let output = std::path::PathBuf::from(output);

    let mut wb = sheetcore::xlsx::load(&input).expect("打开失败");
    println!(
        "loaded sheets={} pivots_pending={} path={:?}",
        wb.sheets.len(),
        wb.pivots_pending.len(),
        wb.path.as_ref().map(|p| p.display().to_string())
    );
    wb.path = Some(input.clone());
    sheetcore::xlsx::save(&output, &wb).expect("保存失败");
    println!("saved {}", output.display());
}
