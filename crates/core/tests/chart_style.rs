//! D3：图表的颜色 / 坐标轴标题 / Y 轴范围能写出去、也能读回来

use sheetcore::charts::{Chart, ChartKind, Series};

fn sample() -> Chart {
    Chart {
        kind: ChartKind::Column,
        title: "销量".into(),
        series: vec![
            Series {
                name: "一季".into(),
                cats: vec!["甲".into(), "乙".into()],
                vals: vec![1.0, 2.0],
                name_ref: "Sheet1!$B$1".into(),
                cat_ref: "Sheet1!$A$2:$A$3".into(),
                val_ref: "Sheet1!$B$2:$B$3".into(),
                color: Some([0xE0, 0x4A, 0x2F]),
                kind: None,
                secondary: false,
            },
            Series {
                name: "二季".into(),
                cats: vec!["甲".into(), "乙".into()],
                vals: vec![3.0, 4.0],
                name_ref: "Sheet1!$C$1".into(),
                cat_ref: "Sheet1!$A$2:$A$3".into(),
                val_ref: "Sheet1!$C$2:$C$3".into(),
                color: Some([0x2E, 0x86, 0xC1]),
                kind: None,
                secondary: false,
            },
        ],
        legend: true,
        anchor: (0, 0, 8, 6),
        legend_pos: "r".into(),
        part: String::new(),
        drawing: String::new(),
        show_labels: false,
        ref_fixes: Vec::new(),
        axis_x: "月份".into(),
        axis_y: "件数".into(),
        y_min: Some(0.0),
        y_max: Some(100.0),
    }
}

#[test]
fn chart_xml_carries_color_and_axes() {
    let xml = sheetcore::chart_write::chart_xml_for_test(&sample());
    assert!(xml.contains("E04A2F"), "系列颜色没写进去:\n{xml}");
    assert!(xml.contains("2E86C1"), "第二个系列颜色没写进去");
    assert!(xml.contains("月份"), "X 轴标题没写进去");
    assert!(xml.contains("件数"), "Y 轴标题没写进去");
    assert!(xml.contains("max val=\"100\""), "Y 轴上限没写进去");
}

#[test]
fn parse_reads_back_color_and_axes() {
    let xml = sheetcore::chart_write::chart_xml_for_test(&sample());
    let ch = sheetcore::charts::parse_chart(&xml).expect("解析失败");
    assert_eq!(ch.series.len(), 2);
    assert_eq!(ch.series[0].color, Some([0xE0, 0x4A, 0x2F]), "颜色没读回来");
    assert_eq!(ch.series[1].color, Some([0x2E, 0x86, 0xC1]));
    assert_eq!(ch.axis_x, "月份");
    assert_eq!(ch.axis_y, "件数");
    assert_eq!(ch.y_max, Some(100.0));
}
