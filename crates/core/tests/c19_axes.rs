//! C19.1：系列范围 / 轴 / 标签
//!
//! 夹具 `axes.xlsx`（Excel 16.0）：
//! - 进度：完成率 15%/40%/65%/90%，数值轴 TickLabels 数字格式 `0%`；
//! - 散点：X = 1/2/10/50/100（非等距），Y 一组，XY 散点图。

use std::path::{Path, PathBuf};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/c19/axes.xlsx")
}

#[test]
fn percentage_axis_format_is_read_from_val_ax() {
    let charts = sheetcore::charts::load_charts(&fixture(), "进度").expect("加载");
    assert_eq!(charts.len(), 1);
    assert_eq!(charts[0].y_fmt, "0%", "数值轴格式应取 valAx 的 numFmt");
    assert_eq!(charts[0].series[0].vals, vec![0.15, 0.4, 0.65, 0.9]);
}

#[test]
fn percent_ticks_are_drawn_with_the_axis_format() {
    let wb = sheetcore::xlsx::load(&fixture()).expect("加载");
    let layout = sheetcore::print::paginate(&wb.sheets[0], &wb.sheets[0].page);
    let out = sheetcore::print::page_ops_with(&wb, 0, &layout, 0, Some(&fixture()));
    let texts: Vec<&str> = out.texts.iter().map(|t| t.text.as_str()).collect();
    assert!(texts.iter().any(|t| t.ends_with('%')), "刻度应显示百分比：{texts:?}");
    // Excel COM 实测：MaximumScale=1 MajorUnit=0.1 → 0/10%/…/100%
    assert!(texts.contains(&"10%"), "0.9 的百分比轴应按 10% 一格：{texts:?}");
    assert!(texts.contains(&"100%"), "上限 1.0 应有 100% 刻度：{texts:?}");
}

#[test]
fn scatter_series_keep_non_uniform_x_values() {
    let charts = sheetcore::charts::load_charts(&fixture(), "散点").expect("加载");
    assert_eq!(charts.len(), 1);
    let ch = &charts[0];
    assert_eq!(ch.kind, sheetcore::charts::ChartKind::Scatter);
    assert_eq!(ch.series[0].xv, vec![1.0, 2.0, 10.0, 50.0, 100.0]);
    assert_eq!(ch.series[0].vals, vec![2.0, 4.5, 5.0, 9.0, 12.0]);
}

#[test]
fn scatter_points_follow_x_scale_not_index() {
    let wb = sheetcore::xlsx::load(&fixture()).expect("加载");
    let sh = &wb.sheets[1];
    let layout = sheetcore::print::paginate(sh, &sh.page);
    let out = sheetcore::print::page_ops_with(&wb, 1, &layout, 0, Some(&fixture()));
    let pts: Vec<(f32, f32)> = out
        .polys
        .iter()
        .flat_map(|(p, _)| p.clone())
        .collect();
    assert!(pts.len() >= 5, "散点要画出来：{:?}", pts.len());
    // 非等距：相邻点的水平间距不相等（1→2 近，2→10 远）
    let mut xs: Vec<f32> = pts.iter().map(|p| p.0).collect();
    xs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    xs.dedup_by(|a, b| (*a - *b).abs() < 0.01);
    let gaps: Vec<f32> = xs.windows(2).map(|w| w[1] - w[0]).collect();
    assert!(gaps.len() >= 3, "至少 4 个不同 X 位置：{gaps:?}");
    let mn = gaps.iter().cloned().fold(f32::MAX, f32::min);
    let mx = gaps.iter().cloned().fold(0.0f32, f32::max);
    assert!(mx > mn * 2.0, "X 应按真实数值分布而不是等距：{gaps:?}");
}

#[test]
fn negative_blank_and_error_values_keep_their_slots() {
    let charts = sheetcore::charts::load_charts(&fixture(), "异常").expect("加载");
    let vals = &charts[0].series[0].vals;
    assert_eq!(vals.len(), 4, "空值/#N/A 不能把后面的数值左移：{vals:?}");
    assert_eq!(vals[0], 5.0);
    assert_eq!(vals[1], -3.0);
    assert!(vals[2].is_nan(), "空单元格应为 NaN");
    assert!(vals[3].is_nan(), "#N/A 应为 NaN");
}

#[test]
fn axis_format_and_scatter_x_survive_a_normal_save() {
    let mut wb = sheetcore::xlsx::load(&fixture()).expect("加载");
    let sid = wb.sheets[0].style_of(1, 1);
    let mut st = wb.styles.get(sid).clone();
    st.bold = true;
    let id = wb.styles.intern(st);
    wb.sheets[0].set_style(1, 1, id);
    let out = std::env::temp_dir().join("sc_c19_axes_rt.xlsx");
    let _ = std::fs::remove_file(&out);
    sheetcore::xlsx::save(&out, &wb).expect("保存");
    let pct = sheetcore::charts::load_charts(&out, "进度").expect("读回进度");
    assert_eq!(pct[0].y_fmt, "0%", "保存不能丢轴的百分比格式");
    let sc = sheetcore::charts::load_charts(&out, "散点").expect("读回散点");
    assert_eq!(sc[0].series[0].xv, vec![1.0, 2.0, 10.0, 50.0, 100.0]);
}

#[test]
fn writer_keeps_axis_number_format() {
    let work = std::env::temp_dir().join("sc_c19_axes_write.xlsx");
    let _ = std::fs::remove_file(&work);
    std::fs::copy(fixture(), &work).unwrap();
    let wb = sheetcore::xlsx::load(&work).expect("加载");
    let ch = wb.sheets[0].charts[0].clone();
    assert_eq!(ch.y_fmt, "0%");
    sheetcore::chart_write::replace_chart(&work, &ch.part, &ch).expect("改写");
    let back = sheetcore::charts::load_charts(&work, "进度").expect("读回");
    assert_eq!(back[0].y_fmt, "0%", "写出去的数值轴应带 numFmt 0%");
}
