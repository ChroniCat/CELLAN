//! 打开→另存的保真回归：样式、条件格式四种类型、数据验证、批注、表格、
//! 图表关系、图片、冻结、跨表公式，一样不能少。
//!
//! 这份 compat_in.xlsx 是用 openpyxl 造的「日常复杂表」；
//! 其中图标集/数据条/色阶/单元格规则各一条，专门盯 umya 写坏条件格式的问题。

use std::io::Read;
use std::path::{Path, PathBuf};

fn fixture() -> PathBuf {
    Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/compat_in.xlsx")).to_path_buf()
}

fn entry(path: &Path, name: &str) -> Option<String> {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(path).ok()?).ok()?;
    let mut f = zip.by_name(name).ok()?;
    let mut s = String::new();
    f.read_to_string(&mut s).ok()?;
    Some(s)
}

fn names(path: &Path) -> Vec<String> {
    let mut zip = zip::ZipArchive::new(std::fs::File::open(path).unwrap()).unwrap();
    (0..zip.len())
        .filter_map(|i| zip.by_index(i).ok().map(|e| e.name().to_string()))
        .collect()
}

#[test]
fn roundtrip_keeps_everything() {
    let src = fixture();
    let out = std::env::temp_dir().join("compat_roundtrip.xlsx");
    let _ = std::fs::remove_file(&out);

    let wb = sheetcore::xlsx::load(&src).expect("加载失败");
    assert_eq!(wb.sheets.len(), 2, "两张表");
    sheetcore::xlsx::save(&out, &wb).expect("保存失败");

    let sheet = entry(&out, "xl/worksheets/sheet1.xml").expect("没有 sheet1");
    // 条件格式四种类型都要在
    assert_eq!(sheet.matches("<conditionalFormatting").count(), 3, "条件格式块数");
    assert_eq!(sheet.matches("<cfRule").count(), 4, "规则条数");
    assert_eq!(sheet.matches("<dataBar").count(), 1, "数据条");
    assert_eq!(sheet.matches("colorScale").count(), 3, "色阶（开+关标签）");
    assert_eq!(sheet.matches("<iconSet").count(), 1, "图标集不能变成数据条");
    assert_eq!(sheet.matches("type=\"cellIs\"").count(), 1, "单元格规则");
    // 数据验证与提示
    assert_eq!(sheet.matches("<dataValidation ").count(), 1, "数据验证");
    assert!(sheet.contains("promptTitle"), "输入提示丢了");
    // 冻结
    assert!(sheet.contains("<pane"), "冻结窗格丢了");

    // 部件：表格/批注/图片/图表 各一个，不能有孤儿
    let all = names(&out);
    assert_eq!(all.iter().filter(|n| n.starts_with("xl/tables/")).count(), 1, "表格部件重复或丢失");
    assert_eq!(all.iter().filter(|n| n.starts_with("xl/comments")).count(), 1, "批注部件重复");
    assert_eq!(all.iter().filter(|n| n.starts_with("xl/media/")).count(), 1, "图片");
    assert_eq!(all.iter().filter(|n| n.starts_with("xl/charts/chart")).count(), 1, "图表");

    // 绘图关系：图表与图片各一条（umya 会把图表那条删掉）
    let rels: String = all
        .iter()
        .filter(|n| n.starts_with("xl/drawings/_rels/"))
        .filter_map(|n| entry(&out, n))
        .collect();
    assert_eq!(rels.matches("/chart\"").count(), 1, "图表关系丢了");
    assert_eq!(rels.matches("/image\"").count(), 1, "图片关系丢了");

    // 跨表公式
    let sheet2 = entry(&out, "xl/worksheets/sheet2.xml").expect("没有 sheet2");
    assert!(sheet2.contains("月度!B2:D2"), "跨表引用丢了");

    let _ = std::fs::remove_file(&out);
}
