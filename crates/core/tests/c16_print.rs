//! C16 打印/PDF：显示模型、分页、标题重复、页眉页脚。
//!
//! 预览与 PDF 共用 `print::page_ops`，所以这里的断言同时守住两条路径。

use sheetcore::display;
use sheetcore::print;
use std::path::{Path, PathBuf};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/c16/print.xlsx")
}

fn texts_of(wb: &sheetcore::model::Workbook, layout: &print::Layout, page: usize) -> Vec<String> {
    print::page_ops(wb, 0, layout, page)
        .texts
        .into_iter()
        .map(|t| t.text)
        .collect()
}

#[test]
fn display_model_resolves_alignment_formats_and_colors() {
    let wb = sheetcore::xlsx::load(&fixture()).expect("加载打印夹具");
    let sheet = &wb.sheets[0];

    let text = display::view_at(&wb, sheet, 4, 5, None).unwrap(); // F5 文本
    assert_eq!(text.halign, 0, "General 文本左对齐");
    let num = display::view_at(&wb, sheet, 2, 5, None).unwrap(); // F3 数字
    assert_eq!(num.halign, 2, "General 数字右对齐");
    let boolean = display::view_at(&wb, sheet, 3, 5, None).unwrap(); // F4 TRUE
    assert_eq!(boolean.halign, 1, "General 布尔居中");

    let pct = display::view_at(&wb, sheet, 5, 5, None).unwrap(); // F6 0.256
    assert_eq!(pct.text, "25.6%", "百分比按格式显示");
    let red = display::view_at(&wb, sheet, 6, 5, None).unwrap(); // F7 -987.654
    assert_eq!(red.text, "--987.65", "Excel 单段格式先套绝对值再补负号：--987.65");
    assert_eq!(red.color, [255, 0, 0], "[Red] 负数用红色");
    let money = display::view_at(&wb, sheet, 7, 5, None).unwrap(); // F8
    assert!(money.text.contains("1,234,567.89"), "货币格式：{}", money.text);

    let merged = display::view_at(&wb, sheet, 0, 0, None).unwrap();
    assert_eq!(merged.merge, Some((0, 0, 0, 3)), "合并范围要带出来");
    assert_eq!(merged.halign, 1);
    assert!(display::merge_covered(sheet, 0, 2), "被盖住的格子打印时跳过");
    assert!(!display::merge_covered(sheet, 0, 0));

    let wrap = display::view_at(&wb, sheet, 7, 0, None).unwrap();
    assert!(wrap.wrap);
    assert_eq!(wrap.valign, 1, "垂直居中");
    assert_eq!(wrap.indent, 1);
    let lines = display::wrap_lines(&wrap.text, 200.0, 10.0);
    assert!(lines.len() >= 2 && lines.len() <= 4, "应折成 2~4 行：{lines:?}");
}

#[test]
fn pagination_repeats_titles_and_starts_body_after_them() {
    let wb = sheetcore::xlsx::load(&fixture()).expect("加载打印夹具");
    let sheet = &wb.sheets[0];
    let layout = print::paginate(sheet, &sheet.page);
    assert!(layout.pages.len() >= 2, "60 行应分成多页：{}", layout.pages.len());
    assert_eq!(layout.pages[0].rows.0, 2, "正文从标题行之后（0 基第 2 行）开始");

    for (i, span) in layout.pages.iter().enumerate() {
        assert!(span.rows.0 >= 2, "第 {i} 页不应再把标题行算进正文");
        let texts = texts_of(&wb, &layout, i);
        let title_count = texts.iter().filter(|t| t.as_str() == "2026 年第一季度销售汇总").count();
        assert_eq!(title_count, 1, "第 {i} 页标题必须恰好出现一次：{texts:?}");
        assert!(texts.iter().any(|t| t.contains("数量")), "第 {i} 页缺表头");
        assert!(texts.iter().any(|t| t.contains(&format!("{}/{}", i + 1, layout.pages.len()))),
            "第 {i} 页页眉页码不对：{texts:?}");
        assert!(texts.iter().any(|t| t == "内部资料"), "页脚左段");
    }
    // 每一行只出现在一页
    let mut seen = std::collections::HashSet::new();
    for (i, span) in layout.pages.iter().enumerate() {
        for r in span.rows.0..=span.rows.1 {
            assert!(seen.insert(r), "第 {r} 行被分到多页（第 {i} 页）");
        }
    }
}

#[test]
fn body_cells_are_not_duplicated_in_continuation_pages() {
    let wb = sheetcore::xlsx::load(&fixture()).expect("加载打印夹具");
    let sheet = &wb.sheets[0];
    let layout = print::paginate(sheet, &sheet.page);
    let last = layout.pages.len() - 1;
    let first = texts_of(&wb, &layout, 0);
    let tail = texts_of(&wb, &layout, last);
    assert!(first.iter().any(|t| t == "行10"), "首页应含正文第一行：{first:?}");
    assert!(!tail.iter().any(|t| t == "行10"), "末页不该重复正文第一行：{tail:?}");
    assert!(tail.iter().any(|t| t == "行60"), "末页应有最后一行：{tail:?}");
}

#[test]
fn csv_of_pages_matches_expected_row_split() {
    let wb = sheetcore::xlsx::load(&fixture()).expect("加载打印夹具");
    let sheet = &wb.sheets[0];
    let layout = print::paginate(sheet, &sheet.page);
    // 每页容量由行高决定，但必须连续、不重不漏
    let mut expect = 2u32;
    for span in &layout.pages {
        assert_eq!(span.rows.0, expect, "分页必须连续");
        expect = span.rows.1 + 1;
    }
    assert_eq!(expect, 60, "最后一行（0 基 59）必须分到某一页");
}

#[test]
fn empty_sheet_still_produces_one_page() {
    let wb = sheetcore::model::Workbook::new_blank();
    let layout = print::paginate(&wb.sheets[0], &wb.sheets[0].page);
    assert_eq!(layout.pages.len(), 1);
}

#[test]
fn pdf_export_writes_every_page() {
    let wb = sheetcore::xlsx::load(&fixture()).expect("加载打印夹具");
    let font = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets/fonts/subset/NotoSansSC-Regular.subset.ttf");
    let out = std::env::temp_dir().join("sc_c16_print.pdf");
    let _ = std::fs::remove_file(&out);
    let n = print::export_pdf(&wb, 0, &out, &font).expect("导出 PDF");
    assert!(n >= 2);
    let bytes = std::fs::read(&out).expect("读回 PDF");
    assert!(bytes.starts_with(b"%PDF-1."), "PDF 头");
    let text = String::from_utf8_lossy(&bytes);
    assert_eq!(text.matches("/Type /Page\n").count() + text.matches("/Type /Page ").count(), n, "页对象数应与页数一致");
    assert!(bytes.len() > 5000, "PDF 太小：{}", bytes.len());
}
