//! 把一张表导出 PDF，顺便打印分页信息
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let src = args.first().cloned().unwrap_or("F:/Finance/work/table.xlsx".into());
    let out = args.get(1).cloned().unwrap_or("F:/Finance/work/out.pdf".into());
    let wb = sheetcore::xlsx::load(std::path::Path::new(&src)).unwrap();
    let sheet = wb.sheet(0);
    let layout = sheetcore::print::paginate(sheet, &sheet.page);
    println!(
        "纸张 {:?} 可打印区 {:?} 缩放 {:.2} 页数 {}",
        layout.page_size,
        layout.printable,
        layout.scale,
        layout.pages.len()
    );
    for (i, p) in layout.pages.iter().take(6).enumerate() {
        println!("  第{}页 行 {:?} 列 {:?}", i + 1, p.rows, p.cols);
    }
    let font = std::path::Path::new("C:/Windows/Fonts/simhei.ttf");
    match sheetcore::print::export_pdf(&wb, 0, std::path::Path::new(&out), font) {
        Ok(n) => println!("已写出 {out}，{n} 页"),
        Err(e) => println!("失败: {e:#}"),
    }
}
