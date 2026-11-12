//! 页面设置：读、改、存
use std::path::Path;

fn load() -> Option<sheetcore::model::Workbook> {
    let p = Path::new("tests/test-work/page.xlsx");
    assert!(p.exists(), "required test fixture is missing: {}", p.display());
    Some(sheetcore::xlsx::load(p).expect("读入"))
}

#[test]
#[ignore = "requires optional synthetic fixtures under tests/test-work"]
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
#[ignore = "requires optional synthetic fixtures under tests/test-work"]
fn changes_survive_save() {
    let Some(mut wb) = load() else { return };
    let out = std::path::PathBuf::from(format!("tests/test-work/page_out_{}.xlsx", std::process::id()));
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
