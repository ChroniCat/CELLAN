//! C19.1：逐系列引用 —— 写盘前按引用重读缓存、拒绝坏引用

use std::path::{Path, PathBuf};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/c19/refresh.xlsx")
}

fn chart() -> sheetcore::charts::Chart {
    let wb = sheetcore::xlsx::load(&fixture()).expect("加载");
    sheetcore::chart_write::chart_from_selection(
        &wb.sheets[0],
        "数据",
        (0, 0, 4, 1),
        "引用",
        sheetcore::charts::ChartKind::Column,
    )
}

#[test]
fn changing_the_value_ref_rereads_the_cache() {
    let wb = sheetcore::xlsx::load(&fixture()).expect("加载");
    let mut ch = chart();
    assert_eq!(ch.series[0].vals, vec![10.0, 20.0, 30.0, 40.0]);
    ch.series[0].val_ref = "'数据'!$C$2:$C$5".into();
    sheetcore::chart_write::refresh_series_from_refs(&wb, "数据", &mut ch).expect("刷新");
    assert_eq!(ch.series[0].vals, vec![1.0, 2.0, 3.0, 4.0]);
    assert_eq!(ch.series[0].cats, vec!["甲", "乙", "丙", "丁"]);
}

#[test]
fn bad_or_empty_refs_are_rejected() {
    let wb = sheetcore::xlsx::load(&fixture()).expect("加载");
    let mut ch = chart();
    ch.series[0].val_ref = "数据!$Z$1:$Z$3".into();
    let e = sheetcore::chart_write::refresh_series_from_refs(&wb, "数据", &mut ch).unwrap_err();
    assert!(
        format!("{e}").contains("取不到任何数字"),
        "越界区域应报取不到数：{e}"
    );

    ch.series[0].val_ref = "不是引用".into();
    let e2 = sheetcore::chart_write::refresh_series_from_refs(&wb, "数据", &mut ch).unwrap_err();
    assert!(format!("{e2}").contains("无效"), "语法坏应报无效：{e2}");
}

#[test]
fn scatter_refresh_keeps_x_y_in_pairs() {
    let wb = sheetcore::xlsx::load(&fixture()).expect("加载");
    let mut ch = chart();
    ch.kind = sheetcore::charts::ChartKind::Scatter;
    ch.series[0].cat_ref = "'数据'!$D$2:$D$5".into();
    ch.series[0].val_ref = "'数据'!$B$2:$B$5".into();
    sheetcore::chart_write::refresh_series_from_refs(&wb, "数据", &mut ch).expect("刷新");
    assert_eq!(ch.series[0].xv, vec![1.5, 2.5, 10.0, 50.0]);
    assert_eq!(ch.series[0].vals, vec![10.0, 20.0, 30.0, 40.0]);

    // X 列是文本 → 整行丢弃 → 取不到数
    ch.series[0].cat_ref = "'数据'!$E$2:$E$5".into();
    let e = sheetcore::chart_write::refresh_series_from_refs(&wb, "数据", &mut ch).unwrap_err();
    assert!(format!("{e}").contains("取不到任何数字"), "{e}");
}
