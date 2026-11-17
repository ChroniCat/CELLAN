//! C19.3：雷达样式（standard / marker / filled）的解析、写出、绘制与 Excel 保真

use std::path::{Path, PathBuf};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/c19/axes.xlsx")
}

fn radar(style: &str) -> sheetcore::charts::Chart {
    let wb = sheetcore::xlsx::load(&fixture()).expect("加载");
    let mut ch = sheetcore::chart_write::chart_from_selection(
        wb.sheet(1),
        "散点",
        (0, 0, 4, 1),
        "雷达",
        sheetcore::charts::ChartKind::Radar,
    );
    ch.radar_style = style.into();
    ch
}

#[test]
fn radar_style_is_written_verbatim() {
    let xml = sheetcore::chart_write::chart_xml_for_test(&radar("filled"));
    assert!(
        xml.contains(r#"<c:radarStyle val="filled"/>"#),
        "filled 没写进 radarStyle：\n{xml}"
    );
    let xml2 = sheetcore::chart_write::chart_xml_for_test(&radar("standard"));
    assert!(xml2.contains(r#"<c:radarStyle val="standard"/>"#));
    // 空/非法值退回 marker（Excel 默认）
    let xml3 = sheetcore::chart_write::chart_xml_for_test(&radar(""));
    assert!(xml3.contains(r#"<c:radarStyle val="marker"/>"#));
}

#[test]
fn radar_style_survives_write_and_reload() {
    let out = std::env::current_exe()
        .expect("exe")
        .parent()
        .expect("dir")
        .join(format!("c19_radar_{}.xlsx", std::process::id()));
    let _ = std::fs::remove_file(&out);
    let wb = sheetcore::xlsx::load(&fixture()).expect("加载");
    sheetcore::xlsx::save(&out, &wb).expect("先存");
    let ch = radar("filled");
    sheetcore::chart_write::insert_chart(&out, "散点", &ch).expect("插入雷达图");
    let back = sheetcore::xlsx::load(&out).expect("读回");
    let rc = back
        .sheet(1)
        .charts
        .iter()
        .find(|c| c.kind == sheetcore::charts::ChartKind::Radar)
        .expect("应有雷达图");
    assert_eq!(rc.radar_style, "filled");
    if std::env::var("SC_KEEP").is_ok() {
        println!("RADAR_KEPT={}", out.display());
    } else {
        let _ = std::fs::remove_file(&out);
    }
}

#[test]
fn filled_radar_paints_a_face_and_no_markers() {
    let mut wb = sheetcore::xlsx::load(&fixture()).expect("加载");
    let mut ch = radar("filled");
    ch.anchor = (0, 0, 4, 1);
    wb.sheets[1].charts.push(ch.clone());
    let layout = sheetcore::print::paginate(&wb.sheets[1], &wb.sheets[1].page);
    let out = sheetcore::print::page_ops_with(&wb, 1, &layout, 0, Some(&fixture()));
    // 散点/折线本身也会 polys，用「50% 白混色」这一雷达填充特征色判定：
    // 默认调色板首色 4472C4 → (68+255)/2=161, (114+255)/2=184, (196+255)/2=225
    let fill = (161u8, 184u8, 225u8);
        let has_face = out
        .polys
        .iter()
        .any(|(p, c)| p.len() >= 3 && (c[0], c[1], c[2]) == fill);
    assert!(has_face, "填充雷达应有混色面 {fill:?}");

    // 对照：marker 样式没有这个面
    let mut wb2 = sheetcore::xlsx::load(&fixture()).expect("加载");
    let mut ch2 = radar("marker");
    ch2.anchor = (0, 0, 4, 1);
    wb2.sheets[1].charts.push(ch2);
    let layout2 = sheetcore::print::paginate(&wb2.sheets[1], &wb2.sheets[1].page);
    let out2 = sheetcore::print::page_ops_with(&wb2, 1, &layout2, 0, Some(&fixture()));
    assert!(
        !out2
            .polys
            .iter()
            .any(|(p, c)| p.len() >= 3 && (c[0], c[1], c[2]) == fill),
        "marker 样式的雷达不该有填充面"
    );
}
