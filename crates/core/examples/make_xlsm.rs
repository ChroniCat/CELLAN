//! 把测试用的 vbaProject.bin 塞进一个 xlsm，供界面验证
fn main() {
    let src = std::path::Path::new("F:/Finance/work/table.xlsx");
    let out = std::path::Path::new("F:/Finance/work/macro.xlsm");
    let wb = sheetcore::xlsx::load(src).unwrap();
    let _ = std::fs::remove_file(out);
    sheetcore::xlsx::save(out, &wb).unwrap();
    let vba = std::fs::read("F:/Finance/work/vba_fixture2.bin").unwrap();
    sheetcore::vba::restore_vba(out, &vba).unwrap();
    println!("已生成 {}，宏部件 {} 字节", out.display(), vba.len());
    println!("检测 has_macros = {}", sheetcore::vba::has_macros(out));
}
