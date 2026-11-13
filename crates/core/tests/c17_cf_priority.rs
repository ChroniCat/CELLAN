//! C17：条件格式的 priority、阈值类型（percent/percentile）、stopIfTrue 跨范围与差异边框。
//!
//! 夹具由 Excel 16.0 生成（`crates/core/tests/fixtures/c17/priority.xlsx`）：
//! - A 列三点色阶中点 `<cfvo type="percent" val="50"/>`
//! - B 列三点色阶中点 `<cfvo type="percentile" val="50"/>`
//! - C 列两条同条件规则，XML 顺序第一条 priority=8、第二条 priority=5（逆序）
//! - D 列第一条 stopIfTrue 覆盖三格，第二条本应上色但被停
use sheetcore::cf::{overlay_at, percentile_inc, CfKind, Cfvo, CfvoKind};

fn fixture() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/c17/priority.xlsx")
}

fn overlays(wb: &sheetcore::model::Workbook) -> Vec<(u32, u32, Option<[u8; 3]>)> {
    let sh = wb.sheet(0);
    let mut st = sheetcore::cf::CfStats::default();
    sheetcore::cf::compute_stats(&sh.cf, sh, &mut st);
    let mut calc = sheetcore::formula::Calc::new(wb);
    let mut out = Vec::new();
    for r in 0..3u32 {
        for c in 0..4u32 {
            let ov = overlay_at(&sh.cf, &st, sh, r, c, |f, r0, c0| {
                let shifted = sheetcore::cf::shift_formula(f.trim_start_matches('='), r0, c0);
                sheetcore::formula::parse(&shifted)
                    .map(|node| matches!(calc.eval_node(&node, 0), sheetcore::formula::FValue::Bool(true)))
                    .unwrap_or(false)
            });
            out.push((r, c, ov.fill));
        }
    }
    out
}

#[test]
fn parse_keeps_priority_and_threshold_kinds() {
    let wb = sheetcore::xlsx::load(&fixture()).expect("加载 C17 夹具");
    let cf = &wb.sheet(0).cf;
    assert_eq!(cf.blocks.len(), 4, "四段 conditionalFormatting");
    let scale = |bi: usize| match &cf.blocks[bi].rules[0].kind {
        CfKind::Scale { points } => points.clone(),
        other => panic!("期望色阶，得到 {other:?}"),
    };
    let a = scale(0);
    assert_eq!(a.len(), 3);
    assert_eq!(a[0].0.kind, CfvoKind::Min);
    assert_eq!(a[1].0.kind, CfvoKind::Percent, "A 列中点是 percent");
    assert_eq!(a[1].0.val, 50.0);
    assert_eq!(a[2].0.kind, CfvoKind::Max);
    let b = scale(1);
    assert_eq!(b[1].0.kind, CfvoKind::Percentile, "B 列中点是 percentile");
    // priority 原样读进来
    assert_eq!(cf.blocks[0].rules[0].priority, 3);
    assert_eq!(cf.blocks[1].rules[0].priority, 4);
    let c = &cf.blocks[2];
    assert_eq!(c.rules[0].priority, 8, "C 第一条 priority=8");
    assert_eq!(c.rules[1].priority, 5, "C 第二条 priority=5（XML 顺序与 priority 逆序）");
    // dxf 差异填充
    assert_eq!(cf.dxfs.len(), 4);
}

#[test]
fn percent_and_percentile_pick_different_thresholds() {
    // 1000 / 1100 / 100000：50% = 50500，50 百分位 = 1100
    assert_eq!(percentile_inc(&[1000.0, 1100.0, 100000.0], 0.5), 1100.0);
    let p = Cfvo::new(CfvoKind::Percent, 50.0).resolve(1000.0, 100000.0, &[1000.0, 1100.0, 100000.0]);
    assert_eq!(p, 50500.0);
    let b = Cfvo::new(CfvoKind::Percentile, 50.0).resolve(1000.0, 100000.0, &[1000.0, 1100.0, 100000.0]);
    assert_eq!(b, 1100.0);

    let wb = sheetcore::xlsx::load(&fixture()).expect("加载夹具");
    let ov = overlays(&wb);
    let fill = |r: u32, c: u32| ov.iter().find(|x| x.0 == r && x.1 == c).unwrap().2;
    // A2（1100，percent 中点 50500）仍在红端；B2（1100）正好落在 percentile 中点 → 黄
    assert_eq!(fill(1, 0), Some([255, 1, 0]), "A2 应接近红色");
    assert_eq!(fill(1, 1), Some([255, 255, 0]), "B2 应为黄色中点");
    assert_eq!(fill(0, 0), Some([255, 0, 0]));
    assert_eq!(fill(0, 1), Some([255, 0, 0]));
    assert_eq!(fill(2, 0), Some([0, 255, 0]));
    assert_eq!(fill(2, 1), Some([0, 255, 0]));
}

#[test]
fn priority_wins_over_xml_order_and_stop_if_true_is_global() {
    let wb = sheetcore::xlsx::load(&fixture()).expect("加载夹具");
    let ov = overlays(&wb);
    let fill = |r: u32, c: u32| ov.iter().find(|x| x.0 == r && x.1 == c).unwrap().2;
    // C 列：XML 第一条 priority=8（红），第二条 priority=5（蓝）→ 蓝赢
    assert_eq!(fill(0, 2), Some([0, 0, 255]), "priority 小的规则胜出");
    assert_eq!(fill(1, 2), Some([0, 0, 255]));
    // D 列：第一条 stopIfTrue，第二条不该上色
    assert_eq!(fill(0, 3), Some([255, 0, 0]));
    assert_eq!(fill(1, 3), Some([255, 0, 0]));
}

#[test]
fn save_roundtrip_keeps_priority_and_cfvo_types() {
    let tmp = std::env::temp_dir().join("sc_c17_roundtrip.xlsx");
    let _ = std::fs::remove_file(&tmp);
    let wb = sheetcore::xlsx::load(&fixture()).expect("加载夹具");
    sheetcore::xlsx::save(&tmp, &wb).expect("保存");
    let re = sheetcore::xlsx::load(&tmp).expect("重新加载");
    let cf = &re.sheet(0).cf;
    assert_eq!(cf.blocks.len(), 4);
    let a = match &cf.blocks[0].rules[0].kind {
        CfKind::Scale { points } => points.clone(),
        other => panic!("期望色阶 {other:?}"),
    };
    assert_eq!(a[1].0.kind, CfvoKind::Percent, "保存后仍是 percent");
    assert_eq!(a[1].0.val, 50.0);
    let b = match &cf.blocks[1].rules[0].kind {
        CfKind::Scale { points } => points.clone(),
        other => panic!("期望色阶 {other:?}"),
    };
    assert_eq!(b[1].0.kind, CfvoKind::Percentile, "保存后仍是 percentile");
    assert_eq!(cf.blocks[0].rules[0].priority, 3);
    assert_eq!(cf.blocks[1].rules[0].priority, 4);
    assert_eq!(cf.blocks[2].rules[0].priority, 8);
    assert_eq!(cf.blocks[2].rules[1].priority, 5);
    assert_eq!(cf.blocks[3].rules[0].stop, true, "stopIfTrue 保住");
    // 用重新加载的工作簿重算一遍，颜色应与原始一致
    let ov = overlays(&re);
    let fill = |r: u32, c: u32| ov.iter().find(|x| x.0 == r && x.1 == c).unwrap().2;
    assert_eq!(fill(1, 0), Some([255, 1, 0]));
    assert_eq!(fill(1, 1), Some([255, 255, 0]));
    assert_eq!(fill(0, 2), Some([0, 0, 255]));
    assert_eq!(fill(0, 3), Some([255, 0, 0]));
    let _ = std::fs::remove_file(&tmp);
}

#[test]
fn x14_extension_is_detected_and_flagged() {
    let plain = sheetcore::xlsx::load(&fixture()).expect("加载 C17 夹具");
    assert!(!plain.sheet(0).cf.unknown_ext, "经典条件格式不应误报扩展");
    let ext = sheetcore::xlsx::load(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/c17/x14bar.xlsx"),
    )
    .expect("加载 x14 夹具");
    assert!(ext.sheet(0).cf.unknown_ext, "Excel 数据条写在 x14:conditionalFormatting 里");
    // 数据条的经典部分仍要能读出来（<cfRule type=dataBar>）
    let bars = ext
        .sheet(0)
        .cf
        .blocks
        .iter()
        .flat_map(|b| b.rules.iter())
        .filter(|r| matches!(r.kind, CfKind::Bar { .. }))
        .count();
    assert_eq!(bars, 1, "经典 dataBar 规则应解析");
    assert!(sheetcore::cf::xml_has_cf_ext("<worksheet><extLst><x14:conditionalFormatting/></extLst></worksheet>"));
    assert!(!sheetcore::cf::xml_has_cf_ext("<worksheet><conditionalFormatting sqref=\"A1\"/></worksheet>"));
}
