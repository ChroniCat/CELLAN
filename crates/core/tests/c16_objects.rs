//! C16.3：图片、图表、文本框进入打印/PDF。
//!
//! 夹具由 Excel 2016 生成（`scripts/c16_objects_fixture.ps1`）：
//! A1:B5 数据、图表（簇状柱形，锚 C1 附近）、A10 附近的 PNG、A20 附近的文本框。

use sheetcore::print;
use std::path::{Path, PathBuf};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/c16/objects.xlsx")
}

fn font() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts/subset/NotoSansSC-Regular.subset.ttf")
}

#[test]
fn objects_are_parsed_from_the_excel_fixture() {
    let wb = sheetcore::xlsx::load(&fixture()).expect("加载对象夹具");
    let sh = &wb.sheets[0];
    assert_eq!(sh.charts.len(), 1, "图表");
    assert_eq!(sh.pictures.len(), 1, "图片");
    assert_eq!(sh.textboxes.len(), 1, "文本框");
    assert_eq!(sh.charts[0].title, "季度销量");
    assert_eq!(sh.charts[0].anchor, (0, 2, 14, 8), "图表锚点 C1:I15");
    assert_eq!(sh.pictures[0].ext.to_ascii_lowercase(), "png");
    assert!(sh.textboxes[0].text.contains("打印文本框"));
}

#[test]
fn page_ops_emit_picture_chart_and_textbox() {
    let wb = sheetcore::xlsx::load(&fixture()).expect("加载对象夹具");
    let sh = &wb.sheets[0];
    let layout = print::paginate(sh, &sh.page);
    let out = print::page_ops_with(&wb, 0, &layout, 0, Some(&fixture()));
    assert_eq!(out.images.len(), 1, "图片要进绘制指令");
    assert!(out.images[0].6.eq_ignore_ascii_case("png"));
    assert!(!out.images[0].5.is_empty(), "图片字节要读出来");
    assert!(
        !out.rects.is_empty() && out.lines.len() > 4,
        "图表要有柱形与坐标轴：rects={} lines={}",
        out.rects.len(),
        out.lines.len()
    );
    assert!(
        out.texts.iter().any(|t| t.text == "季度销量"),
        "图表标题：{:?}",
        out.texts.iter().map(|t| &t.text).collect::<Vec<_>>()
    );
    assert!(
        out.texts.iter().any(|t| t.text.contains("打印文本框")),
        "文本框内容"
    );
}

#[test]
fn pdf_embeds_images_and_object_text() {
    let wb = sheetcore::xlsx::load(&fixture()).expect("加载对象夹具");
    let out_pdf = std::env::temp_dir().join("sc_c16_objects.pdf");
    let _ = std::fs::remove_file(&out_pdf);
    let n = print::export_pdf_from(&wb, 0, &out_pdf, &font(), Some(&fixture())).expect("导出");
    assert!(n >= 1);
    let bytes = std::fs::read(&out_pdf).expect("读回 PDF");
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("/Subtype /Image"), "图片 XObject 缺失");
    assert!(
        text.contains("/DCTDecode") || text.contains("/FlateDecode"),
        "图片过滤器缺失"
    );
    assert!(text.contains("/XObject"), "页面资源缺 XObject");
    assert!(bytes.len() > 8000, "PDF 太小：{}", bytes.len());
}

#[test]
fn value_axis_ticks_use_excel_nice_steps() {
    // Excel 对这份夹具的数值轴是 0/5/10/15/20/25（不是五等分的 0/6.25/12.5/18.75/25）
    let wb = sheetcore::xlsx::load(&fixture()).expect("加载对象夹具");
    let sh = &wb.sheets[0];
    let layout = print::paginate(sh, &sh.page);
    let out = print::page_ops_with(&wb, 0, &layout, 0, Some(&fixture()));
    let texts: Vec<&str> = out.texts.iter().map(|t| t.text.as_str()).collect();
    for want in ["0", "5", "10", "15", "20", "25"] {
        assert!(texts.contains(&want), "缺刻度 {want}：{texts:?}");
    }
    assert!(!texts.contains(&"6.25"), "不应再出现五等分刻度：{texts:?}");
    assert_eq!(texts.iter().filter(|t| **t == "季度销量").count(), 1, "图表标题只画一次");
}
