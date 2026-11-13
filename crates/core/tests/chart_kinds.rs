//! 八种图表类型 + 删除
use sheetcore::charts::ChartKind;
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/qa02").join(name)
}

fn kinds() -> Vec<ChartKind> {
    vec![
        ChartKind::Column,
        ChartKind::Bar,
        ChartKind::BarStacked,
        ChartKind::Line,
        ChartKind::Area,
        ChartKind::Pie,
        ChartKind::Doughnut,
        ChartKind::Scatter,
    ]
}

#[test]
fn every_kind_inserts_and_deletes() {
    let src = fixture("table.xlsx");
    assert!(src.exists(), "required test fixture is missing: {}", src.display());
    for k in kinds() {
        let mut wb = sheetcore::xlsx::load(&src).expect("读入");
        let name = wb.sheet(0).name.clone();
        let region = sheetcore::sort_filter::current_region(wb.sheet(0), 1, 1);
        let out = std::env::current_exe().expect("test exe").parent().expect("test dir").join(format!("ck_{:?}_{}.xlsx", k, std::process::id()));
        let _ = std::fs::remove_file(&out);
        sheetcore::xlsx::save(&out, &wb).expect("保存");
        let ch = sheetcore::chart_write::chart_from_selection(
            wb.sheet(0),
            &name,
            region,
            &format!("{k:?} 图"),
            k,
        );
        let mut ch = ch;
        ch.show_labels = true;
        sheetcore::chart_write::insert_chart(&out, &name, &ch).unwrap_or_else(|e| panic!("{k:?} 插入失败: {e}"));

        let list = sheetcore::charts::load_charts(&out, &name).expect("读图表");
        assert_eq!(list.len(), 1, "{k:?} 应有 1 张图");
        assert_eq!(list[0].kind, k, "{k:?} 读回来的类型不对: {:?}", list[0].kind);
        assert!(!list[0].part.is_empty(), "没记下部件路径");
        assert!(list[0].show_labels, "{k:?} 数据标签没读回来");

        if std::env::var("KEEP_CHARTS").is_ok() {
            continue;
        }
        // 删除
        sheetcore::chart_write::delete_chart(&out, &list[0].drawing, &list[0].part)
            .unwrap_or_else(|e| panic!("{k:?} 删除失败: {e}"));
        let after = sheetcore::charts::load_charts(&out, &name).expect("再读");
        assert!(after.is_empty(), "{k:?} 删除后还剩 {} 张", after.len());
        // 文件还能正常读
        let back = sheetcore::xlsx::load(&out).expect("删完还能打开");
        assert_eq!(back.sheet(0).get(0, 0).unwrap().value.display(), "姓名");
    }
}
