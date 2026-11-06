fn main() {
    let src = std::path::Path::new("F:/Finance/work/page.xlsx");
    let mut wb = sheetcore::xlsx::load(src).unwrap();
    wb.sheets[0].page.landscape = false;
    wb.sheets[0].page_dirty = true;
    for i in 0..3 {
        let out = std::path::PathBuf::from(format!("F:/Finance/work/fresh{i}.xlsx"));
        let _ = std::fs::remove_file(&out);
        match sheetcore::xlsx::save(&out, &wb) {
            Ok(()) => println!("保存成功 {}", out.display()),
            Err(e) => println!("失败 {}: {e}", out.display()),
        }
    }
}
