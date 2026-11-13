//! 插入图表：生成部件并挂到表上，读回来能认出这张图
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/qa02").join(name)
}

#[test]
fn insert_column_chart() {
    let src = fixture("table.xlsx");
    assert!(src.exists(), "required test fixture is missing: {}", src.display());
    let out = std::env::current_exe().expect("test exe").parent().expect("test dir").join(format!("chart_out_{}.xlsx", std::process::id()));
    let _ = std::fs::remove_file(&out);

    let wb = sheetcore::xlsx::load(&src).expect("读入");
    sheetcore::xlsx::save(&out, &wb).expect("先存一份");

    // 用 A1:C6 造图表：首行是系列名，首列是分类
    let chart = sheetcore::chart_write::chart_from_selection(
        wb.sheet(0),
        "表格",
        (0, 0, 5, 2),
        "部门销售额",
        sheetcore::charts::ChartKind::Column,
    );
    assert_eq!(chart.series.len(), 2, "两列数据 → 两个系列");
    assert_eq!(chart.series[0].cats, vec!["张三", "李四", "王五", "赵六", "钱七"]);
    assert_eq!(chart.series[1].vals, vec![300.0, 100.0, 200.0, 50.0, 400.0]);

    sheetcore::chart_write::insert_chart(&out, "表格", &chart).expect("插入图表");

    // 读回来
    let back = sheetcore::xlsx::load(&out).expect("重新读入");
    let charts = &back.sheet(0).charts;
    assert_eq!(charts.len(), 1, "应读到一张图");
    let c = &charts[0];
    assert_eq!(c.title, "部门销售额");
    assert_eq!(c.series.len(), 2);
    assert_eq!(c.series[0].name, "部门");
    assert_eq!(c.series[1].vals, vec![300.0, 100.0, 200.0, 50.0, 400.0], "金额列应有数值");
    assert!(c.series[0].vals.is_empty(), "部门列是文本，没有数值");
    let _ = std::fs::remove_file(&out);
}
