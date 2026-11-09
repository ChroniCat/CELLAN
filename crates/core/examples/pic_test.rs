use std::path::Path;
fn main() {
    let src = std::env::args().nth(1).unwrap();
    let out = std::env::args().nth(2).unwrap();
    std::fs::copy(&src, &out).unwrap();
    // 1x1 红点 PNG
    let png: Vec<u8> = std::fs::read("F:/Finance/work/red.png").unwrap();
    let name = "S";
    match sheetcore::pic_write::insert_picture(Path::new(&out), name, &png, "png", 2, 2) {
        Ok(_) => println!("插入成功"),
        Err(e) => println!("插入失败: {e:#}"),
    }
    for (s, v) in sheetcore::pic::load(Path::new(&out)) {
        println!("{s} 图片 {} 张", v.len());
    }
}
