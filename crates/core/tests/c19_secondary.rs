//! C19.2：次坐标轴刻度取整 + 散点 x/y 建模
//!
//! - 次轴：Excel 在右侧另画一套「整齐」刻度。夹具里主系列 80/100（主轴 0/20/…/100），
//!   次系列 2.0/3.7（nice_max → 5，步长 1 → 0/1/2/3/4/5）。断言右侧出现 4、5。
//! - 散点：`c19/axes.xlsx` 的「散点」表 X=1/2/10/50/100；从选区建图必须把这列读成
//!   `Series.xv`（成对、X 非数值整行丢弃），写出 `xVal` 用 xv 而不是序号。

use std::path::{Path, PathBuf};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/c19/axes.xlsx")
}

fn series(name: &str, vals: Vec<f64>, secondary: bool) -> sheetcore::charts::Series {
    let cats = vec!["甲".to_string(), "乙".to_string()];
    sheetcore::charts::Series {
        name: name.to_string(),
        cats,
        vals,
        name_ref: format!("'进度'!$B$1"),
        cat_ref: "'进度'!$A$2:$A$3".into(),
        val_ref: "'进度'!$B$2:$B$3".into(),
        color: None,
        xv: Vec::new(),
        kind: None,
        secondary,
    }
}

#[test]
fn secondary_axis_gets_its_own_nice_ticks() {
    let mut wb = sheetcore::xlsx::load(&fixture()).expect("加载");
    let ch = sheetcore::charts::Chart {
        kind: sheetcore::charts::ChartKind::Column,
        title: "次轴".into(),
        series: vec![
            series("主", vec![80.0, 100.0], false),
            series("次", vec![2.0, 3.7], true),
        ],
        legend: true,
        anchor: (0, 0, 4, 1),
        legend_pos: "r".into(),
        part: String::new(),
        drawing: String::new(),
        show_labels: false,
        label_opts: Default::default(),
        radar_style: String::new(),
        ref_fixes: Vec::new(),
        axis_x: String::new(),
        axis_y: String::new(),
        y_fmt: String::new(),
        y_min: None,
        y_max: None,
    };
    wb.sheets[0].charts.push(ch);
    let layout = sheetcore::print::paginate(&wb.sheets[0], &wb.sheets[0].page);
    let out = sheetcore::print::page_ops_with(&wb, 0, &layout, 0, Some(&fixture()));
    let texts: Vec<&str> = out.texts.iter().map(|t| t.text.as_str()).collect();

    // 主轴 0/20/40/60/80/100；次轴取整到 0..5，所以 4、5 只可能来自次轴
    assert!(texts.contains(&"80"), "主轴应有 80：{texts:?}");
    assert!(texts.contains(&"100"), "主轴应有 100：{texts:?}");
    assert!(texts.contains(&"4"), "次轴应有 4：{texts:?}");
    assert!(texts.contains(&"5"), "次轴应有 5（2.0/3.7 取整到 5）：{texts:?}");
}

#[test]
fn scatter_selection_models_x_and_y_pairs() {
    let wb = sheetcore::xlsx::load(&fixture()).expect("加载");
    let sh = &wb.sheets[1];
    let ch = sheetcore::chart_write::chart_from_selection(
        sh,
        "散点",
        (0, 0, 5, 1),
        "x/y",
        sheetcore::charts::ChartKind::Scatter,
    );
    let se = &ch.series[0];
    assert_eq!(se.xv, vec![1.0, 2.0, 10.0, 50.0, 100.0], "X 列要建模进 xv");
    assert_eq!(se.vals, vec![2.0, 4.5, 5.0, 9.0, 12.0]);
    assert_eq!(se.xv.len(), se.vals.len(), "x/y 必须成对");

    let xml = sheetcore::chart_write::chart_xml_for_test(&ch);
    assert!(xml.contains(r#"<c:pt idx="2"><c:v>10</c:v></c:pt>"#), "xVal 应写 10 而不是序号 3：\n{xml}");
    assert!(xml.contains(r#"<c:pt idx="4"><c:v>100</c:v></c:pt>"#), "xVal 应写 100：\n{xml}");
}

#[test]
fn scatter_pairs_survive_write_and_reload() {
    let out = std::env::current_exe()
        .expect("test exe")
        .parent()
        .expect("test dir")
        .join(format!("c19_scatter_{}.xlsx", std::process::id()));
    let _ = std::fs::remove_file(&out);
    let wb = sheetcore::xlsx::load(&fixture()).expect("加载");
    sheetcore::xlsx::save(&out, &wb).expect("先存");
    let ch = sheetcore::chart_write::chart_from_selection(
        &wb.sheets[1],
        "散点",
        (0, 0, 5, 1),
        "x/y",
        sheetcore::charts::ChartKind::Scatter,
    );
    sheetcore::chart_write::insert_chart(&out, "散点", &ch).expect("插入散点图");

    let back = sheetcore::xlsx::load(&out).expect("读回");
    let c = &back.sheets[1].charts[0];
    assert_eq!(c.series[0].xv, vec![1.0, 2.0, 10.0, 50.0, 100.0]);
    assert_eq!(c.series[0].vals, vec![2.0, 4.5, 5.0, 9.0, 12.0]);
    if std::env::var("SC_KEEP").is_ok() {
        println!("SCATTER_KEPT={}", out.display());
    } else {
        let _ = std::fs::remove_file(&out);
    }
}
