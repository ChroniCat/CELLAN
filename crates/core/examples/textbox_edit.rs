//! 改一个文本框的文字后再保存：组合/连接线/图表不能被拆掉。
//! 用法：cargo run -p sheetcore --example textbox_edit -- <in.xlsx> <out.xlsx> <名字> <新文本>
fn main() {
    let mut a = std::env::args().skip(1);
    let input = std::path::PathBuf::from(a.next().expect("缺少输入"));
    let output = std::path::PathBuf::from(a.next().expect("缺少输出"));
    let name = a.next().expect("缺少形状名");
    let text = a.next().expect("缺少新文本");
    let mut wb = sheetcore::xlsx::load(&input).expect("打开失败");
    wb.path = Some(input);
    let sh = &mut wb.sheets[0];
    let before = sh.textboxes.len();
    let mut hit = false;
    for tb in sh.textboxes.iter_mut() {
        if tb.name == name {
            tb.text = text.clone();
            hit = true;
        }
    }
    assert!(hit, "找不到形状 {name}（现有 {} 个）", before);
    sh.textboxes_dirty = true;
    sheetcore::xlsx::save(&output, &wb).expect("保存失败");
    println!("textboxes={before} edited={name} -> saved {}", output.display());
}
