//! QA-01.3 往返：drawing（图片/形状/文本框）必须逐字节保留。
//!
//! 背景：加载时把 textboxes 标脏，会让 `textbox::apply` 用我们的文本框模型重写整个
//! drawing —— 实测 Excel 的矩形被写成 `txBox="1"`、`a:off/a:ext` 归零、`xdr:style`
//! 整段丢失，Excel 重新打开后形状类型从 msoAutoShape 变成 msoTextBox。
//! 只读快照不应该标脏。

use std::io::Read;

fn read_part(path: &std::path::Path, part: &str) -> Option<Vec<u8>> {
    let file = std::fs::File::open(path).ok()?;
    let mut zip = zip::ZipArchive::new(file).ok()?;
    let mut f = zip.by_name(part).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    Some(buf)
}

fn tmp(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir();
    dir.join(format!(
        "sc_qa01_{tag}_{}_{}.xlsx",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

#[test]
fn loading_textboxes_does_not_mark_them_dirty() {
    let src = std::path::Path::new("tests/fixtures/c16/objects.xlsx");
    let wb = sheetcore::xlsx::load(src).expect("打开带形状的夹具失败");
    for sh in &wb.sheets {
        assert!(
            !sh.textboxes_dirty,
            "表 {} 加载后 textboxes_dirty 必须为 false，否则保存时会重写 drawing",
            sh.name
        );
    }
}

#[test]
fn drawing_part_survives_load_save_byte_for_byte() {
    let src = std::path::Path::new("tests/fixtures/c16/objects.xlsx");
    let out = tmp("draw");
    let wb = sheetcore::xlsx::load(src).expect("打开夹具失败");
    sheetcore::xlsx::save(&out, &wb).expect("保存失败");

    let before = read_part(src, "xl/drawings/drawing1.xml").expect("原文件没有 drawing1.xml");
    let after = read_part(&out, "xl/drawings/drawing1.xml").expect("保存后 drawing1.xml 丢了");
    assert_eq!(before, after, "drawing1.xml 必须逐字节一致");

    let img_before = read_part(src, "xl/media/image1.png").expect("原文件没有 image1.png");
    let img_after = read_part(&out, "xl/media/image1.png").expect("保存后 image1.png 丢了");
    assert_eq!(img_before, img_after, "内嵌图片必须逐字节一致");

    let _ = std::fs::remove_file(&out);
}
