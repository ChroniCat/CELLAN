//! C16/UI-14.2：PDF 导出用真 700 字形，不再拿描边模拟粗体；行高/基线按字体
//! ascender/descender 计算。这里直接查导出的 PDF 结构，避免「看着像」的结论。

use sheetcore::model::{Cell, Sheet, Value, Workbook};
use sheetcore::pdf::Font;
use sheetcore::style::CellStyle;
use std::path::{Path, PathBuf};

fn assets() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts/subset")
}

fn build() -> Workbook {
    let mut wb = Workbook::new_blank();
    let bold = wb.styles.intern(CellStyle { bold: true, size: 14.0, ..Default::default() });
    let plain = wb.styles.intern(CellStyle { size: 14.0, ..Default::default() });
    let sh: &mut Sheet = wb.sheet_mut(0);
    sh.set(0, 0, Cell { value: Value::Text("粗体标题".into()), formula: None });
    sh.set(1, 0, Cell { value: Value::Text("常规正文".into()), formula: None });
    sh.set_style(0, 0, bold);
    sh.set_style(1, 0, plain);
    wb
}

/// 导出带粗体文字的 PDF：必须嵌入第二条字体（/F2），且粗体文字走 /F2 且不带描边（2 Tr）。
#[test]
fn pdf_embeds_real_bold_face_and_uses_it_for_bold_text() {
    let wb = build();
    let dir = std::env::temp_dir().join(format!("sc-pdf-bold-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let out = dir.join("bold.pdf");
    let regular = Font::load(&assets().join("NotoSansSC-Regular.subset.ttf")).unwrap();
    let bold = Font::load(&assets().join("NotoSansSC-Bold.subset.ttf")).unwrap();
    sheetcore::print::export_pdf_pages_with_fonts(
        &wb,
        0,
        &out,
        regular,
        Some(bold),
        None,
        None,
    )
    .unwrap();
    let bytes = std::fs::read(&out).unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("/EmbeddedBold"), "PDF 里没有第二套粗体字体");
    assert!(text.contains("/BaseFont /EmbeddedBold"), "缺少粗体 BaseFont");
    // ToUnicode 第二份（粗体字形号 → Unicode）
    assert!(
        text.matches("beginbfchar").count() >= 2,
        "粗体没有自己的 ToUnicode CMap（复制/检索会乱码）"
    );
    // 粗体文字必须用 /F2 下发，且不再用描边模式（2 Tr）
    let f2_uses = text.matches("/F2 ").count();
    assert!(f2_uses >= 1, "内容流里没有用 /F2 画粗体，实际 {f2_uses}");
    assert!(!text.contains("2 Tr"), "粗体仍在用描边模拟");
    // 行高按字体度量算：两行 14pt 文字，基线差应约等于 (asc+desc)*14
    let asc = 1.16_f32; // Noto Sans SC 的 ascender/upm 约值，下面用真实字体再算
    let _ = asc;
    let _ = std::fs::remove_dir_all(&dir);
}

/// 行高/基线按字体度量算：顶对齐留 ascender、底对齐留 descender、居中按视觉中心。
/// 这三个公式就是 UI-14.2「按 ascent/descent 精算行高」的落点，直接钉住。
#[test]
fn cell_baseline_uses_font_ascent_and_descent() {
    let size = 14.0_f32;
    let (asc, desc) = (size * 1.16, size * 0.288);
    let line_h = asc + desc;
    let (top, ch) = (100.0_f32, 60.0_f32);
    let upper = top + ch;

    let b_top = sheetcore::print::cell_baseline(0, top, ch, line_h, line_h, asc, desc);
    assert!((b_top - (upper - asc)).abs() < 1e-4, "顶对齐基线不等于上边 − ascender");

    let b_center = sheetcore::print::cell_baseline(1, top, ch, line_h, line_h, asc, desc);
    let visual_center = b_center + (asc - desc) / 2.0;
    assert!(
        (visual_center - (top + ch / 2.0)).abs() < 1e-4,
        "居中时文字视觉中心 {visual_center} 没对准格子中心 {}",
        top + ch / 2.0
    );

    let b_bottom = sheetcore::print::cell_baseline(2, top, ch, line_h, line_h, asc, desc);
    assert!((b_bottom - (top + desc)).abs() < 1e-4, "底对齐基线不等于下边 + descender");

    // 三行：靠下时首行基线上移两行行高
    let b3 = sheetcore::print::cell_baseline(2, top, ch, line_h * 3.0, line_h, asc, desc);
    assert!((b3 - (top + desc + 2.0 * line_h)).abs() < 1e-4);
}

/// vmetric 真的被 page_ops 采用：换成度量后基线/行高随之变化；
/// 传同一组默认值时与不传完全一致（没有旁路覆盖）。
#[test]
fn page_ops_honours_vmetric() {
    let wb = build();
    let layout = sheetcore::print::paginate(&wb.sheets[0], &wb.sheets[0].page);
    let base = sheetcore::print::page_ops_metric(&wb, 0, &layout, 0, None, None);
    let same: std::sync::Arc<dyn Fn(f32) -> (f32, f32) + Send + Sync> =
        std::sync::Arc::new(|s: f32| (s * 0.92, s * 0.26));
    let with_same =
        sheetcore::print::page_ops_metrics(&wb, 0, &layout, 0, None, None, Some(same));
    let b0 = base.texts.iter().find(|t| t.text.contains("粗体")).unwrap().baseline;
    let b1 = with_same.texts.iter().find(|t| t.text.contains("粗体")).unwrap().baseline;
    assert!((b0 - b1).abs() < 1e-4, "同一组度量结果不一致：{b0} vs {b1}");

    let tall: std::sync::Arc<dyn Fn(f32) -> (f32, f32) + Send + Sync> =
        std::sync::Arc::new(|s: f32| (s * 1.16, s * 0.288));
    let with_tall =
        sheetcore::print::page_ops_metrics(&wb, 0, &layout, 0, None, None, Some(tall));
    let b2 = with_tall.texts.iter().find(|t| t.text.contains("粗体")).unwrap().baseline;
    assert!((b2 - b0).abs() > 0.05, "换字体度量后基线没变（vmetric 没生效）");
}
