//! UI-20：图片进内存模型、统一保存、重复保存不增生、未保存不改文件。

use std::io::Read;
use std::path::PathBuf;

fn temp_path(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sc_ui20_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

/// 1×1 透明 PNG（67 字节）
fn tiny_png() -> Vec<u8> {
    const B64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";
    b64_decode(B64)
}

fn b64_decode(s: &str) -> Vec<u8> {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::new();
    let mut buf = 0u32;
    let mut bits = 0u32;
    for ch in s.bytes() {
        if ch == b'=' {
            break;
        }
        let Some(pos) = T.iter().position(|c| *c == ch) else { continue };
        buf = (buf << 6) | pos as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    out
}

fn part(path: &std::path::Path, name: &str) -> Option<Vec<u8>> {
    let file = std::fs::File::open(path).ok()?;
    let mut zip = zip::ZipArchive::new(file).ok()?;
    let mut f = zip.by_name(name).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    Some(buf)
}

fn names(path: &std::path::Path) -> Vec<String> {
    let file = std::fs::File::open(path).unwrap();
    let mut zip = zip::ZipArchive::new(file).unwrap();
    (0..zip.len()).map(|i| zip.by_index(i).unwrap().name().to_string()).collect()
}

fn base_book(path: &std::path::Path) {
    let mut wb = sheetcore::model::Workbook::new_blank();
    wb.sheets[0].set(
        0,
        0,
        sheetcore::model::Cell {
            value: sheetcore::model::Value::Text("图".into()),
            formula: None,
        },
    );
    sheetcore::xlsx::save(path, &wb).unwrap();
}

#[test]
fn picture_inserted_in_memory_only_is_not_written_until_save() {
    let path = temp_path("mem.xlsx");
    let _ = std::fs::remove_file(&path);
    base_book(&path);
    let before = std::fs::read(&path).unwrap();

    // 只改内存：加一张图，不保存
    let mut wb = sheetcore::xlsx::load(&path).unwrap();
    let png = tiny_png();
    wb.sheets[0].pictures.push(sheetcore::pic::Picture {
        rect: (1, 1, 5, 4),
        off: [0; 4],
        part: sheetcore::pic_write::memory_part(&png, "png"),
        data: Some(std::sync::Arc::new(png.clone())),
        ext: "png".into(),
        name: "图片 1".into(),
    });
    wb.sheets[0].pictures_dirty = true;
    drop(wb);

    let after = std::fs::read(&path).unwrap();
    assert_eq!(before, after, "没保存就不该动文件");
    assert!(
        !names(&path).iter().any(|n| n.contains("/media/sc")),
        "没保存就不该出现媒体部件"
    );
}

#[test]
fn saved_picture_has_anchor_and_media_and_survives_a_second_save() {
    let path = temp_path("save.xlsx");
    let _ = std::fs::remove_file(&path);
    base_book(&path);
    let png = tiny_png();

    let mut wb = sheetcore::xlsx::load(&path).unwrap();
    wb.sheets[0].pictures.push(sheetcore::pic::Picture {
        rect: (2, 3, 7, 6),
        off: [0; 4],
        part: sheetcore::pic_write::memory_part(&png, "png"),
        data: Some(std::sync::Arc::new(png.clone())),
        ext: "png".into(),
        name: "图片 1".into(),
    });
    wb.sheets[0].pictures_dirty = true;
    sheetcore::xlsx::save(&path, &wb).unwrap();

    let media: Vec<String> = names(&path)
        .into_iter()
        .filter(|n| n.contains("/media/sc") && n.ends_with(".png"))
        .collect();
    assert_eq!(media.len(), 1, "应写入一个媒体部件：{media:?}");
    assert_eq!(part(&path, &media[0]).unwrap(), png, "媒体字节要原样写回");

    let drawings: Vec<String> = names(&path)
        .into_iter()
        .filter(|n| n.starts_with("xl/drawings/drawing") && n.ends_with(".xml"))
        .collect();
    assert_eq!(drawings.len(), 1, "应有一个 drawing：{drawings:?}");
    let xml = String::from_utf8(part(&path, &drawings[0]).unwrap()).unwrap();
    assert!(xml.contains("<xdr:twoCellAnchor"), "锚点必须是 twoCellAnchor");
    assert!(xml.contains("<xdr:col>3</xdr:col>"), "起始列 3：{xml}");
    assert!(xml.contains("<xdr:row>2</xdr:row>"), "起始行 2：{xml}");
    assert!(xml.contains("<xdr:col>6</xdr:col>"), "结束列 6：{xml}");
    assert!(xml.contains("<xdr:row>7</xdr:row>"), "结束行 7：{xml}");
    assert!(xml.contains("r:embed=\"rIdPic1\""), "图片关系：{xml}");
    let ct = String::from_utf8(part(&path, "[Content_Types].xml").unwrap()).unwrap();
    assert!(ct.contains("Extension=\"png\""), "内容类型要登记 png：{ct}");

    // 再存一次：媒体部件不增生、锚点仍是我们的
    let wb2 = sheetcore::xlsx::load(&path).unwrap();
    let loaded = &wb2.sheets[0].pictures;
    assert_eq!(loaded.len(), 1, "重开应读到一张图");
    assert_eq!(loaded[0].rect, (2, 3, 7, 6), "锚点坐标要一致");
    let mut again = wb2;
    again.sheets[0].pictures_dirty = true;
    sheetcore::xlsx::save(&path, &again).unwrap();
    let media2: Vec<String> = names(&path)
        .into_iter()
        .filter(|n| n.contains("/media/sc") && n.ends_with(".png"))
        .collect();
    assert_eq!(media2.len(), 1, "重复保存不该增生媒体：{media2:?}");
}

#[test]
fn moved_picture_rewrites_anchor_and_deleting_it_removes_anchor() {
    let path = temp_path("move.xlsx");
    let _ = std::fs::remove_file(&path);
    base_book(&path);
    let png = tiny_png();
    let mut wb = sheetcore::xlsx::load(&path).unwrap();
    wb.sheets[0].pictures.push(sheetcore::pic::Picture {
        rect: (1, 1, 3, 3),
        off: [0; 4],
        part: sheetcore::pic_write::memory_part(&png, "png"),
        data: Some(std::sync::Arc::new(png.clone())),
        ext: "png".into(),
        name: "图片 1".into(),
    });
    wb.sheets[0].pictures_dirty = true;
    sheetcore::xlsx::save(&path, &wb).unwrap();

    // 移动：重新打开、改矩形、再存
    let mut wb2 = sheetcore::xlsx::load(&path).unwrap();
    wb2.sheets[0].pictures[0].rect = (10, 2, 14, 8);
    wb2.sheets[0].pictures_dirty = true;
    sheetcore::xlsx::save(&path, &wb2).unwrap();
    let reopen = sheetcore::xlsx::load(&path).unwrap();
    assert_eq!(reopen.sheets[0].pictures[0].rect, (10, 2, 14, 8), "移动后的锚点要落盘");

    // 删除：清空图片再存，drawing 里不该再有 pic
    let mut wb3 = reopen;
    wb3.sheets[0].pictures.clear();
    wb3.sheets[0].pictures_dirty = true;
    sheetcore::xlsx::save(&path, &wb3).unwrap();
    let drawings: Vec<String> = names(&path)
        .into_iter()
        .filter(|n| n.starts_with("xl/drawings/drawing") && n.ends_with(".xml"))
        .collect();
    for d in drawings {
        let xml = String::from_utf8(part(&path, &d).unwrap()).unwrap();
        assert!(!xml.contains("<xdr:pic"), "删图后不该留下 pic 锚点：{xml}");
    }
}
