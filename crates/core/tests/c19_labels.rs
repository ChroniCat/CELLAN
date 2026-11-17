//! C19.1：数据标签逐项选项（值/类别/系列/百分比/图例项/引导线/位置）
//!
//! 写出格式按 OOXML `c:dLbls` 的元素顺序；Excel 回读用
//! `scripts/c19_labels_probe.ps1`（DataLabels.ShowValue/ShowCategoryName/...）。

use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/qa02").join(name)
}

fn labelled(name: &str) -> sheetcore::charts::Chart {
    let src = fixture("table.xlsx");
    let wb = sheetcore::xlsx::load(&src).expect("读入");
    let mut ch = sheetcore::chart_write::chart_from_selection(
        wb.sheet(0),
        "表格",
        (0, 0, 5, 2),
        name,
        sheetcore::charts::ChartKind::Column,
    );
    ch.show_labels = true;
    ch.label_opts.show_cat = true;
    ch.label_opts.show_ser = true;
    ch.label_opts.show_legend_key = true;
    ch.label_opts.leader_lines = true;
    ch.label_opts.position = "outEnd".into();
    ch
}

#[test]
fn dlbls_xml_lists_every_option_in_excel_order() {
    let xml = sheetcore::chart_write::chart_xml_for_test(&labelled("标签测试"));
    assert!(xml.contains(r#"<c:showVal val="1"/>"#), "缺 showVal：\n{xml}");
    assert!(xml.contains(r#"<c:showCatName val="1"/>"#), "缺 showCatName");
    assert!(xml.contains(r#"<c:showSerName val="1"/>"#), "缺 showSerName");
    assert!(xml.contains(r#"<c:showPercent val="0"/>"#), "缺 showPercent");
    assert!(xml.contains(r#"<c:showLegendKey val="1"/>"#), "缺 showLegendKey");
    assert!(xml.contains(r#"<c:showLeaderLines val="1"/>"#), "缺 showLeaderLines");
    assert!(xml.contains(r#"<c:dLblPos val="outEnd"/>"#), "缺 dLblPos");

    // schema 顺序：showVal → showCatName → showSerName → showPercent → showLegendKey
    // → showLeaderLines → dLblPos
    let order = [
        "<c:showVal",
        "<c:showCatName",
        "<c:showSerName",
        "<c:showPercent",
        "<c:showLegendKey",
        "<c:showLeaderLines",
        "<c:dLblPos",
    ];
    let mut last = 0;
    for tag in order {
        let at = xml.find(tag).unwrap_or_else(|| panic!("缺 {tag}"));
        assert!(at >= last, "{tag} 顺序错：{xml}");
        last = at;
    }
}

#[test]
fn label_options_survive_write_and_reload() {
    let src = fixture("table.xlsx");
    let out = std::env::current_exe()
        .expect("test exe")
        .parent()
        .expect("test dir")
        .join(format!("c19_labels_{}.xlsx", std::process::id()));
    let _ = std::fs::remove_file(&out);
    let wb = sheetcore::xlsx::load(&src).expect("读入");
    sheetcore::xlsx::save(&out, &wb).expect("先存一份");
    let ch = labelled("标签往返");
    sheetcore::chart_write::insert_chart(&out, "表格", &ch).expect("插入图表");

    let back = sheetcore::xlsx::load(&out).expect("读回");
    let charts = &back.sheet(0).charts;
    assert_eq!(charts.len(), 1);
    let c = &charts[0];
    assert!(c.show_labels, "showVal 没读回");
    assert!(c.label_opts.show_cat, "showCatName 没读回");
    assert!(c.label_opts.show_ser, "showSerName 没读回");
    assert!(!c.label_opts.show_pct, "showPercent 不该为真");
    assert!(c.label_opts.show_legend_key, "showLegendKey 没读回");
    assert!(c.label_opts.leader_lines, "showLeaderLines 没读回");
    assert_eq!(c.label_opts.position, "outEnd");
    if std::env::var("SC_KEEP").is_ok() {
        println!("KEPT={}", out.display());
    } else {
        let _ = std::fs::remove_file(&out);
    }
}

#[test]
fn pie_percent_and_leader_lines_are_written() {
    let src = fixture("table.xlsx");
    let out = std::env::current_exe()
        .expect("test exe")
        .parent()
        .expect("test dir")
        .join(format!("c19_pie_{}.xlsx", std::process::id()));
    let _ = std::fs::remove_file(&out);
    let wb = sheetcore::xlsx::load(&src).expect("读入");
    sheetcore::xlsx::save(&out, &wb).expect("先存一份");
    let mut ch = sheetcore::chart_write::chart_from_selection(
        wb.sheet(0),
        "表格",
        (0, 0, 5, 1),
        "占比",
        sheetcore::charts::ChartKind::Pie,
    );
    ch.show_labels = true;
    ch.label_opts.show_pct = true;
    ch.label_opts.leader_lines = true;
    ch.label_opts.position = "bestFit".into();
    sheetcore::chart_write::insert_chart(&out, "表格", &ch).expect("插入饼图");

    let xml = {
        use std::io::Read;
        let f = std::fs::File::open(&out).unwrap();
        let mut z = zip::ZipArchive::new(f).unwrap();
        let mut s = String::new();
        z.by_name("xl/charts/chart1.xml").unwrap().read_to_string(&mut s).unwrap();
        s
    };
    assert!(xml.contains(r#"<c:showPercent val="1"/>"#), "饼图应有 showPercent：{xml}");
    assert!(xml.contains(r#"<c:showLeaderLines val="1"/>"#), "应有引导线");
    assert!(xml.contains(r#"<c:dLblPos val="bestFit"/>"#), "位置应为 bestFit");

    let back = sheetcore::xlsx::load(&out).expect("读回");
    let c = &back.sheet(0).charts[0];
    assert!(c.label_opts.show_pct && c.label_opts.leader_lines);
    assert_eq!(c.label_opts.position, "bestFit");
    if std::env::var("SC_KEEP").is_ok() {
        println!("PIE_KEPT={}", out.display());
    } else {
        let _ = std::fs::remove_file(&out);
    }
}
