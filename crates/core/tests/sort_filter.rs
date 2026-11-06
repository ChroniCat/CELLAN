//! 排序筛选：读、排序、筛选、保存
use sheetcore::sort_filter::{current_region, sort_region, Filter};
use std::collections::HashSet;
use std::path::Path;

fn load() -> Option<sheetcore::model::Workbook> {
    let p = Path::new("F:/Finance/work/table.xlsx");
    if !p.exists() {
        eprintln!("跳过：缺少 {}", p.display());
        return None;
    }
    Some(sheetcore::xlsx::load(p).expect("读入"))
}

fn names(wb: &sheetcore::model::Workbook) -> Vec<String> {
    (1..=5)
        .map(|r| wb.sheet(0).get(r, 0).map(|c| c.value.display()).unwrap_or_default())
        .collect()
}

#[test]
fn sort_by_amount() {
    let Some(mut wb) = load() else { return };
    let region = current_region(wb.sheet(0), 2, 2);
    assert_eq!(region, (0, 0, 5, 2), "当前区域应覆盖整张表");
    assert!(sheetcore::sort_filter::has_header(wb.sheet(0), 0, 0, 5, 2));
    sort_region(wb.sheet_mut(0), region, 2, true).unwrap();
    assert_eq!(names(&wb), vec!["赵六", "李四", "王五", "张三", "钱七"], "按金额升序");
    sort_region(wb.sheet_mut(0), region, 2, false).unwrap();
    assert_eq!(names(&wb), vec!["钱七", "张三", "王五", "李四", "赵六"], "降序");
}

#[test]
fn filter_and_save() {
    let Some(mut wb) = load() else { return };
    let out = Path::new("F:/Finance/work/table_filtered.xlsx");
    let _ = std::fs::remove_file(out);

    let mut f = Filter::new(0, 0, 5, 2);
    let mut allow = HashSet::new();
    allow.insert("研发".to_string());
    f.allows.insert(1, allow);
    wb.sheets[0].hidden_rows = f.hidden_rows(wb.sheet(0));
    wb.sheets[0].filter = Some(f);
    wb.sheets[0].filter_dirty = true;
    assert_eq!(wb.sheets[0].hidden_rows.len(), 3, "销售2 + 行政1 应被隐藏");

    sheetcore::xlsx::save(out, &wb).expect("保存");
    let back = sheetcore::xlsx::load(out).expect("重新读入");
    let sh = back.sheet(0);
    let f2 = sh.filter.as_ref().expect("autoFilter 应保存");
    assert_eq!(f2.range, (0, 0, 5, 2));
    assert!(f2.allows.get(&1).map(|s| s.contains("研发")).unwrap_or(false));
    assert!(sh.hidden_rows.len() >= 3, "隐藏行应写回: {:?}", sh.hidden_rows);
}
