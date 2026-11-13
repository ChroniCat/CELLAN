//! 页面设置：读、改、存
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/qa02").join(name)
}

fn load() -> Option<sheetcore::model::Workbook> {
    let p = fixture("page.xlsx");
    assert!(p.exists(), "required test fixture is missing: {}", p.display());
    Some(sheetcore::xlsx::load(&p).expect("读入"))
}

#[test]
fn reads_page_settings() {
    let Some(wb) = load() else { return };
    let ps = &wb.sheet(0).page;
    assert!(ps.landscape, "应为横向");
    assert_eq!(ps.paper, 8, "应为 A3");
    assert_eq!(ps.scale, 85);
    assert_eq!(ps.margins[0], 0.5);
    assert!(ps.gridlines, "应打印网格线");
    assert_eq!(ps.print_area, Some((0, 0, 39, 7)), "打印区域 A1:H40");
    assert_eq!(ps.title_rows, Some((0, 1)), "标题行 1:2");
}

#[test]
fn changes_survive_save() {
    let Some(mut wb) = load() else { return };
    let out = std::env::current_exe().expect("test exe").parent().expect("test dir").join(format!("page_out_{}.xlsx", std::process::id()));
    let out = out.as_path();
    let _ = std::fs::remove_file(out);
    {
        let sh = &mut wb.sheets[0];
        sh.page.landscape = false;
        sh.page.paper = 9;
        sh.page.scale = 100;
        sh.page.fit_to_page = true;
        sh.page.fit_width = 1;
        sh.page.header = "&C月度报表".into();
        sh.page.print_area = Some((0, 0, 19, 5));
        sh.page.title_rows = Some((0, 0));
        sh.page.title_cols = Some((0, 2));
        sh.page.row_breaks = vec![5, 12];
        sh.page.col_breaks = vec![3];
        sh.page.margins[0] = 0.25;
        sh.page_dirty = true;
    }
    sheetcore::xlsx::save(out, &wb).expect("保存");
    let back = sheetcore::xlsx::load(out).expect("重新读入");
    let ps = &back.sheet(0).page;
    assert!(!ps.landscape);
    assert_eq!(ps.paper, 9);
    assert!(ps.fit_to_page);
    assert_eq!(ps.header, "&C月度报表");
    assert_eq!(ps.print_area, Some((0, 0, 19, 5)));
    assert_eq!(ps.title_rows, Some((0, 0)));
    assert_eq!(ps.title_cols, Some((0, 2)), "左侧标题列 A:C");
    assert_eq!(ps.row_breaks, vec![5, 12], "水平分页符");
    assert_eq!(ps.col_breaks, vec![3], "垂直分页符");
    assert!((ps.margins[0] - 0.25).abs() < 0.001);
}

#[test]
fn page_setup_pr_is_inserted_after_outline_pr() {
    let xml = r#"<worksheet><sheetPr><outlinePr summaryBelow="1" summaryRight="1" showOutlineSymbols="1"/></sheetPr><sheetData/></worksheet>"#;
    let out = sheetcore::patch::set_page_setup(xml, &sheetcore::page::PageSetup::default());
    let outline = out.find("<outlinePr").expect("outlinePr");
    let page = out.find("<pageSetUpPr").expect("pageSetUpPr");
    assert!(outline < page, "sheetPr 子元素顺序必须是 outlinePr→pageSetUpPr: {out}");
}

/// UI-26 验收页式：A4 横向、左右 10mm、首行重复、宽 1 页高自动。
/// Excel 365 与 WPS 12 对本夹具读出 2 页；本项目分页和 PDF 也必须 2 页。
#[test]
fn excel_style_page_setup_matches_two_pages() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/c16/print.xlsx");
    assert!(src.exists(), "缺少夹具 {}", src.display());
    let mut wb = sheetcore::xlsx::load(&src).expect("读入");
    {
        let ps = &mut wb.sheet_mut(0).page;
        ps.landscape = true;
        ps.paper = 9;
        ps.margins[0] = 10.0 / 25.4;
        ps.margins[1] = 10.0 / 25.4;
        ps.margins[2] = 0.75;
        ps.margins[3] = 0.75;
        ps.title_rows = Some((0, 0));
        ps.fit_to_page = true;
        ps.fit_width = 1;
        ps.fit_height = 0;
    }
    let layout = sheetcore::print::paginate(wb.sheet(0), &wb.sheet(0).page);
    assert_eq!(layout.pages.len(), 2, "Excel/WPS 基准是 2 页");
    let font = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets/fonts/subset/NotoSansSC-Regular.subset.ttf");
    let out = std::env::current_exe()
        .expect("test exe")
        .parent()
        .expect("test dir")
        .join(format!("ui26_pages_{}.pdf", std::process::id()));
    let _ = std::fs::remove_file(&out);
    let n = sheetcore::print::export_pdf(&wb, 0, &out, &font).expect("导出 PDF");
    assert_eq!(n, 2, "PDF 页数与预览/Excel/WPS 一致");
    let _ = std::fs::remove_file(&out);
}

/// UI-26：水平与垂直手动分页符都要进入分页结果。
#[test]
fn manual_breaks_have_both_directions() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/c16/print.xlsx");
    let mut wb = sheetcore::xlsx::load(&src).expect("读入");
    {
        let ps = &mut wb.sheet_mut(0).page;
        ps.row_breaks = vec![20];
        ps.col_breaks = vec![3];
    }
    let sh = wb.sheet(0);
    let (xs, ys) = sheetcore::page::page_breaks(sh);
    let (r1, c1, _, _) = sh.page.print_area.or_else(|| sh.used_bounds()).expect("已用区域");
    let expect_x = sh.cols.pos(3) - sh.cols.pos(c1);
    let expect_y = sh.rows.pos(20) - sh.rows.pos(r1);
    assert!(xs.iter().any(|v| (*v - expect_x).abs() < 0.5), "缺垂直分页：{xs:?}");
    assert!(ys.iter().any(|v| (*v - expect_y).abs() < 0.5), "缺水平分页：{ys:?}");
}
