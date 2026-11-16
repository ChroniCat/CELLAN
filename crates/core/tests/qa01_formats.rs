//! QA-01.3 输入格式矩阵：.xlsm 基准另存 .xlsx、.xls 打开再另存、图片/形状/外链往返。

use std::io::Read;

fn read_part(path: &std::path::Path, part: &str) -> Option<Vec<u8>> {
    let file = std::fs::File::open(path).ok()?;
    let mut zip = zip::ZipArchive::new(file).ok()?;
    let mut f = zip.by_name(part).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    Some(buf)
}

fn tmp(tag: &str, ext: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "sc_qa01fmt_{tag}_{}_{}.{ext}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

#[test]
fn xlsm_base_saved_as_xlsx_declares_non_macro_content_type() {
    let src = std::path::Path::new("tests/fixtures/qa01/formats/features.xlsm");
    let out = tmp("from_xlsm", "xlsx");
    let wb = sheetcore::xlsx::load(src).expect("打开 .xlsm 失败");
    sheetcore::xlsx::save(&out, &wb).expect("另存 .xlsx 失败");

    let ct = read_part(&out, "[Content_Types].xml").expect("没有 [Content_Types].xml");
    let ct = String::from_utf8_lossy(&ct);
    assert!(
        ct.contains("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"),
        "workbook 必须是普通 xlsx 主类型，否则 Excel 报「文件格式或文件扩展名无效」：{ct}"
    );
    assert!(
        !ct.contains("macroEnabled"),
        "没有 VBA 的产物不能声明宏类型：{ct}"
    );
    // 夹具里的特性不能因为换格式就丢
    let sheet = read_part(&out, "xl/worksheets/sheet1.xml")
        .or_else(|| {
            let file = std::fs::File::open(&out).ok()?;
            let mut zip = zip::ZipArchive::new(file).ok()?;
            let name = zip
                .file_names()
                .find(|n| n.starts_with("xl/worksheets/") && n.ends_with(".xml") && !n.contains("_rels"))
                .map(|s| s.to_string())?;
            let mut f = zip.by_name(&name).ok()?;
            let mut buf = Vec::new();
            f.read_to_end(&mut buf).ok()?;
            Some(buf)
        })
        .expect("没有工作表部件");
    let sheet = String::from_utf8_lossy(&sheet);
    assert!(sheet.contains("<conditionalFormatting"), "条件格式丢了");
    assert!(sheet.contains("<dataValidations"), "数据验证丢了");
    assert!(sheet.contains("<tableParts"), "表格对象丢了");
    let _ = std::fs::remove_file(&out);
}

#[test]
fn xls_input_loads_and_saves_as_xlsx() {
    let src = std::path::Path::new("tests/fixtures/qa01/formats/features.xls");
    let out = tmp("from_xls", "xlsx");
    let wb = sheetcore::xlsx::load(src).expect("打开 .xls 失败");
    assert_eq!(wb.sheets.len(), 1, "夹具只有一张表");
    assert_eq!(wb.sheets[0].name, "Feat");
    // 值要读进来：A2=甲、B2=10
    let a2 = wb.sheets[0].get(1, 0).map(|c| c.value.clone());
    let b2 = wb.sheets[0].get(1, 1).map(|c| c.value.clone());
    assert!(
        matches!(a2, Some(sheetcore::Value::Text(ref t)) if &**t == "甲"),
        "A2 应该是文本「甲」，实际 {a2:?}"
    );
    assert!(
        matches!(b2, Some(sheetcore::Value::Number(n)) if (n - 10.0).abs() < 1e-9),
        "B2 应该是 10，实际 {b2:?}"
    );
    sheetcore::xlsx::save(&out, &wb).expect(".xls 打开后另存 .xlsx 失败");
    let ct = read_part(&out, "[Content_Types].xml").expect("没有 [Content_Types].xml");
    let ct = String::from_utf8_lossy(&ct);
    assert!(ct.contains("sheet.main+xml"), "{ct}");
    assert!(!ct.contains("macroEnabled"), "{ct}");
    let _ = std::fs::remove_file(&out);
}

#[test]
fn media_workbook_keeps_picture_shape_and_external_link() {
    let src = std::path::Path::new("tests/fixtures/qa01/formats/media.xlsx");
    let out = tmp("media", "xlsx");
    let wb = sheetcore::xlsx::load(src).expect("打开带图片的夹具失败");
    sheetcore::xlsx::save(&out, &wb).expect("保存失败");

    let draw = read_part(src, "xl/drawings/drawing1.xml").expect("原文件没有 drawing1.xml");
    let draw_after = read_part(&out, "xl/drawings/drawing1.xml").expect("保存后 drawing 丢了");
    assert_eq!(draw, draw_after, "drawing1.xml 必须逐字节一致（形状几何/样式不能重写）");

    assert!(
        read_part(&out, "xl/media/image1.png").is_some(),
        "内嵌图片丢了"
    );
    let link = read_part(&out, "xl/externalLinks/externalLink1.xml");
    assert!(link.is_some(), "外链部件丢了");
    // 目标文件名在 externalLink 部件的关系表里（externalLink1.xml 只有 rId）
    let rels = read_part(&out, "xl/externalLinks/_rels/externalLink1.xml.rels")
        .expect("外链关系表丢了");
    let rels = String::from_utf8_lossy(&rels).to_string();
    assert!(rels.contains("ext-src.xlsx"), "外链目标名丢了：{rels}");
    let link = String::from_utf8_lossy(&link.unwrap()).to_string();
    assert!(link.contains("<externalBook"), "外链部件结构丢了：{link}");

    // 公式与缓存值都在
    let ws_name = {
        let file = std::fs::File::open(&out).unwrap();
        let zip = zip::ZipArchive::new(file).unwrap();
        zip.file_names()
            .find(|n| n.starts_with("xl/worksheets/") && n.ends_with(".xml") && !n.contains("_rels"))
            .map(|s| s.to_string())
            .unwrap()
    };
    let sheet = read_part(&out, &ws_name).unwrap();
    let sheet = String::from_utf8_lossy(&sheet);
    // 文件里的外链公式用书索引：[1]Sheet1!$A$1
    assert!(
        sheet.contains("[1]Sheet1!$A$1") && sheet.contains("[1]Sheet1!$B$1"),
        "工作表里的外链公式丢了"
    );
    let _ = std::fs::remove_file(&out);
}
