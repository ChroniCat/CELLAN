//! C19.3：高级图种（雷达解析/绘制；瀑布、直方图等 chartex 保留原图并显示占位）

use std::path::{Path, PathBuf};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/c19/advanced.xlsx")
}

#[test]
fn radar_chart_is_parsed_and_drawn() {
    let charts = sheetcore::charts::load_charts(&fixture(), "雷达").expect("加载雷达");
    assert_eq!(charts.len(), 1);
    let ch = &charts[0];
    assert_eq!(ch.kind, sheetcore::charts::ChartKind::Radar);
    assert_eq!(ch.series[0].cats, vec!["速度", "力量", "技巧", "耐力", "意识"]);
    assert_eq!(ch.series[0].vals, vec![8.0, 6.0, 9.0, 7.0, 5.0]);
    let wb = sheetcore::xlsx::load(&fixture()).expect("加载");
    let layout = sheetcore::print::paginate(&wb.sheets[0], &wb.sheets[0].page);
    let out = sheetcore::print::page_ops_with(&wb, 0, &layout, 0, Some(&fixture()));
    let segs = out.lines.iter().filter(|l| l.4 >= 1.0).count();
    assert!(segs >= 5, "雷达图要画出五边形（闭合）：{segs}");
}

#[test]
fn chartex_types_become_visible_placeholders() {
    let charts = sheetcore::charts::load_charts(&fixture(), "瀑布").expect("加载瀑布");
    assert_eq!(charts.len(), 1);
    assert_eq!(charts[0].kind, sheetcore::charts::ChartKind::Unknown);
    assert!(charts[0].part.ends_with("chartEx1.xml"), "{}", charts[0].part);
    let wb = sheetcore::xlsx::load(&fixture()).expect("加载");
    let layout = sheetcore::print::paginate(&wb.sheets[1], &wb.sheets[1].page);
    let out = sheetcore::print::page_ops_with(&wb, 1, &layout, 0, Some(&fixture()));
    let texts: Vec<&str> = out.texts.iter().map(|t| t.text.as_str()).collect();
    assert!(
        texts.iter().any(|t| t.contains("保留原图")),
        "未知图表要给可见占位：{texts:?}"
    );
}

#[test]
fn chartex_parts_survive_a_normal_save() {
    use std::io::Read;
    let mut wb = sheetcore::xlsx::load(&fixture()).expect("加载");
    let sid = wb.sheets[0].style_of(0, 0);
    let mut st = wb.styles.get(sid).clone();
    st.bold = true;
    let id = wb.styles.intern(st);
    wb.sheets[0].set_style(0, 0, id);
    let out = std::env::temp_dir().join("sc_c19_adv_rt.xlsx");
    let _ = std::fs::remove_file(&out);
    sheetcore::xlsx::save(&out, &wb).expect("保存");
    let read = |p: &Path, name: &str| -> Option<Vec<u8>> {
        let f = std::fs::File::open(p).ok()?;
        let mut z = zip::ZipArchive::new(f).ok()?;
        let mut b = Vec::new();
        z.by_name(name).ok()?.read_to_end(&mut b).ok()?;
        Some(b)
    };
    for name in ["xl/charts/chartEx1.xml", "xl/charts/chartEx2.xml"] {
        assert_eq!(
            read(&fixture(), name),
            read(&out, name),
            "{name} 保存后必须逐字节不变"
        );
    }
}

#[test]
fn unsupported_chart_cannot_be_overwritten_by_2d() {
    let work = std::env::temp_dir().join("sc_c19_adv_guard.xlsx");
    let _ = std::fs::remove_file(&work);
    std::fs::copy(fixture(), &work).unwrap();
    let charts = sheetcore::charts::load_charts(&work, "瀑布").expect("加载");
    let ch = charts[0].clone();
    let err = sheetcore::chart_write::replace_chart(&work, &ch.part, &ch);
    assert!(err.is_err(), "未知图种不允许被覆盖写出");
    assert!(format!("{}", err.unwrap_err()).contains("保留原图"));
}
