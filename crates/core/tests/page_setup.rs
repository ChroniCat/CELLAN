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
        sh.page.margins[0] = 0.25;
        sh.page_dirty = true;
    }
    sheetcore::xlsx::save(out, &wb).expect("保存");
    let _ = std::fs::remove_file("tests/test-work/page_edit2.xlsx");
    let back = sheetcore::xlsx::load(out).expect("重新读入");
    let ps = &back.sheet(0).page;
    assert!(!ps.landscape);
    assert_eq!(ps.paper, 9);
    assert!(ps.fit_to_page);
    assert_eq!(ps.header, "&C月度报表");
    assert_eq!(ps.print_area, Some((0, 0, 19, 5)));
    assert_eq!(ps.title_rows, Some((0, 0)));
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
