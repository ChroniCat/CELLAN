//! QA-01.3：改一个形状的文字，不能把组合/连接线/图表拆掉，也不能把矩形写成文本框。

use std::io::Read;

fn read_part(path: &std::path::Path, part: &str) -> Option<Vec<u8>> {
    let file = std::fs::File::open(path).ok()?;
    let mut zip = zip::ZipArchive::new(file).ok()?;
    let mut f = zip.by_name(part).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    Some(buf)
}

#[test]
fn group_children_and_connectors_are_not_parsed_as_textboxes() {
    let src = std::path::Path::new("tests/fixtures/qa01/formats/shapes.xlsx");
    let wb = sheetcore::xlsx::load(src).expect("打开形状夹具失败");
    let names: Vec<String> = wb.sheets[0].textboxes.iter().map(|t| t.name.clone()).collect();
    assert_eq!(names, vec!["BoxB".to_string()], "只应认出独立形状 BoxB，实际 {names:?}");
}

#[test]
fn editing_one_shape_keeps_group_connector_and_chart() {
    let src = std::path::Path::new("tests/fixtures/qa01/formats/shapes.xlsx");
    let out = std::env::temp_dir().join(format!(
        "sc_shapes_edit_{}_{}.xlsx",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut wb = sheetcore::xlsx::load(src).expect("打开夹具失败");
    wb.path = Some(src.to_path_buf());
    {
        let sh = &mut wb.sheets[0];
        let tb = sh
            .textboxes
            .iter_mut()
            .find(|t| t.name == "BoxB")
            .expect("夹具里应该有 BoxB");
        tb.text = "改过的文字".into();
        sh.textboxes_dirty = true;
    }
    sheetcore::xlsx::save(&out, &wb).expect("保存失败");

    let draw = read_part(&out, "xl/drawings/drawing1.xml").expect("drawing 丢了");
    let draw = String::from_utf8_lossy(&draw).to_string();

    assert!(draw.contains("改过的文字"), "文字没写进去");
    assert!(draw.contains("GroupAC"), "组合被拆掉了");
    assert!(draw.contains("<xdr:grpSp"), "组合块必须原样保留");
    assert!(draw.contains("LinkAB"), "连接线丢了");
    assert!(draw.contains("<xdr:cxnSp"), "连接线块必须原样保留");
    assert!(draw.contains("ChartD"), "图表形状丢了");
    assert!(draw.contains("<xdr:graphicFrame"), "图表块必须原样保留");
    assert!(
        !draw.contains("txBox=\"1\""),
        "原地改文字不该把矩形写成文本框：{draw}"
    );
    // 矩形自己的 extLst/prstGeom 也要在
    assert!(draw.contains("prstGeom prst=\"rect\""), "预设几何丢了");
    assert!(
        read_part(&out, "xl/charts/chart1.xml").is_some(),
        "图表部件丢了"
    );
    let _ = std::fs::remove_file(&out);
}
