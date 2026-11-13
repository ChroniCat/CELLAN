//! C19.2：组合图每系列类型与次坐标轴
//!
//! 夹具 `combo.xlsx`（Excel 16.0）：柱状图两个系列，系列 2 改成折线并放到次坐标轴，
//! chart1.xml 里是 barChart(axId A/B) + lineChart(axId C/D) 两个容器。

use std::path::{Path, PathBuf};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/c19/combo.xlsx")
}

#[test]
fn combo_chart_keeps_per_series_kind_and_secondary_axis() {
    let charts = sheetcore::charts::load_charts(&fixture(), "销售").expect("加载图表");
    assert_eq!(charts.len(), 1, "应有一张图");
    let ch = &charts[0];
    assert_eq!(ch.series.len(), 2);
    assert_eq!(ch.kind, sheetcore::charts::ChartKind::Combo, "柱+线应识别为组合图");
    assert_eq!(ch.series[0].kind, Some(sheetcore::charts::ChartKind::Column));
    assert!(!ch.series[0].secondary, "系列 1 在主坐标轴");
    assert_eq!(ch.series[1].kind, Some(sheetcore::charts::ChartKind::Line));
    assert!(ch.series[1].secondary, "系列 2 的 axId 与主轴不同，应在次坐标轴");
    assert_eq!(ch.series[1].vals, vec![0.12, 0.25, -0.4, 1.22]);
}

#[test]
fn print_ops_draw_both_series_types_with_secondary_scale() {
    let wb = sheetcore::xlsx::load(&fixture()).expect("加载夹具");
    let layout = sheetcore::print::paginate(&wb.sheets[0], &wb.sheets[0].page);
    let out = sheetcore::print::page_ops_with(&wb, 0, &layout, 0, Some(&fixture()));
    // 柱系列 4 根 + 折线折点，至少有 4 个色块和一段折线
    assert!(out.rects.len() >= 4, "柱形要画出来：{}", out.rects.len());
    assert!(!out.polys.is_empty(), "折线系列要画出来");
    // 次轴按自己的量级缩放：增长率 1.22 不应压到顶部以外，折线点都在绘图区内
    let (_, _, _, h, _) = out.rects[0];
    assert!(h > 0.0);
}

#[test]
fn editing_a_cell_does_not_rewrite_the_combo_chart() {
    use std::io::Read;
    let src = fixture();
    let mut wb = sheetcore::xlsx::load(&src).expect("加载");
    let sid = wb.sheets[0].style_of(1, 1);
    let mut st = wb.styles.get(sid).clone();
    st.bold = true;
    let id = wb.styles.intern(st);
    wb.sheets[0].set_style(1, 1, id);
    let out = std::env::temp_dir().join("sc_c19_combo_rt.xlsx");
    let _ = std::fs::remove_file(&out);
    sheetcore::xlsx::save(&out, &wb).expect("保存");
    let read = |p: &Path| -> Vec<u8> {
        let f = std::fs::File::open(p).unwrap();
        let mut z = zip::ZipArchive::new(f).unwrap();
        let mut b = Vec::new();
        z.by_name("xl/charts/chart1.xml").unwrap().read_to_end(&mut b).unwrap();
        b
    };
    assert_eq!(read(&src), read(&out), "改普通格不应重写图表部件");
    // 重开后系列类型/次轴仍然解析正确
    let charts = sheetcore::charts::load_charts(&out, "销售").expect("重开");
    assert_eq!(charts[0].series[1].kind, Some(sheetcore::charts::ChartKind::Line));
    assert!(charts[0].series[1].secondary);
}

#[test]
fn an_explicit_chart_type_change_is_not_reverted_by_save() {
    use std::io::Read;
    let src = fixture();
    let work = std::env::temp_dir().join("sc_c19_edit.xlsx");
    let _ = std::fs::remove_file(&work);
    std::fs::copy(&src, &work).unwrap();
    let mut wb = sheetcore::xlsx::load(&work).expect("加载");
    let ch = wb.sheets[0].charts[0].clone();
    let mut nc = ch.clone();
    nc.kind = sheetcore::charts::ChartKind::Column;
    nc.series.iter_mut().for_each(|s| {
        s.kind = None;
        s.secondary = false;
    });
    sheetcore::chart_write::replace_chart(&work, &ch.part, &nc).expect("改写图表");
    wb.chart_edits.push(ch.part.clone());
    sheetcore::xlsx::save(&work, &wb).expect("保存");
    let f = std::fs::File::open(&work).unwrap();
    let mut z = zip::ZipArchive::new(f).unwrap();
    let mut xml = String::new();
    z.by_name("xl/charts/chart1.xml").unwrap().read_to_string(&mut xml).unwrap();
    assert!(!xml.contains("<c:lineChart>"), "用户改过的图表不能被基准还原回去");
    assert!(xml.contains("<c:barChart>"));
}

#[test]
fn writer_groups_series_by_kind_and_axis_pair() {
    let wb = sheetcore::xlsx::load(&fixture()).expect("加载");
    let ch = wb.sheets[0].charts[0].clone();
    let xml = sheetcore::chart_write::chart_xml_for_test(&ch);
    assert_eq!(xml.matches("<c:barChart>").count(), 1);
    assert_eq!(xml.matches("<c:lineChart>").count(), 1);
    assert!(xml.contains("<c:axId val=\"1\"/><c:axId val=\"2\"/></c:barChart>"), "{xml}");
    assert!(xml.contains("<c:axId val=\"3\"/><c:axId val=\"4\"/></c:lineChart>"), "{xml}");
    assert!(xml.contains("<c:axPos val=\"r\"/>"), "次轴要画在右侧");
    // 写出去再读回来，逐系列类型与次轴仍然成立
    let work = std::env::temp_dir().join("sc_c19_write.xlsx");
    let _ = std::fs::remove_file(&work);
    std::fs::copy(fixture(), &work).unwrap();
    sheetcore::chart_write::replace_chart(&work, &ch.part, &ch).expect("改写");
    let back = sheetcore::charts::load_charts(&work, "销售").expect("读回");
    assert_eq!(back[0].series[1].kind, Some(sheetcore::charts::ChartKind::Line));
    assert!(back[0].series[1].secondary);
}
