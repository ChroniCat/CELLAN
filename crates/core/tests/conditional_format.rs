//! 条件格式：从真实 xlsx 读、算叠加、改完存回
use sheetcore::cf::{compute_stats, overlay_at, CfKind, CfSheet, CfStats};
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/qa02_cf_dv")
        .join(name)
}

/// 保存输出写到 test executable 旁边，不再写固定输出目录
fn out_path(tag: &str) -> PathBuf {
    std::env::current_exe()
        .expect("test exe")
        .parent()
        .expect("test dir")
        .join(format!("{}_{}.xlsx", tag, std::process::id()))
}

fn load() -> Option<sheetcore::model::Workbook> {
    let p = fixture("cf.xlsx");
    assert!(p.exists(), "required test fixture is missing: {}", p.display());
    Some(sheetcore::xlsx::load(&p).expect("读入"))
}

#[test]
fn parses_rules_from_file() {
    let Some(wb) = load() else { return };
    let cf = &wb.sheets[0].cf;
    assert!(!cf.blocks.is_empty(), "应解析出条件格式块");
    assert!(!cf.dxfs.is_empty(), "应解析出 dxf 格式表");
    let has_cellis = cf
        .blocks
        .iter()
        .any(|b| b.rules.iter().any(|r| matches!(r.kind, CfKind::CellIs { .. })));
    let has_scale = cf
        .blocks
        .iter()
        .any(|b| b.rules.iter().any(|r| matches!(r.kind, CfKind::Scale { .. })));
    let has_bar = cf
        .blocks
        .iter()
        .any(|b| b.rules.iter().any(|r| matches!(r.kind, CfKind::Bar { .. })));
    let has_expr = cf
        .blocks
        .iter()
        .any(|b| b.rules.iter().any(|r| matches!(r.kind, CfKind::Expr { .. })));
    assert!(has_cellis, "应有大等于规则");
    assert!(has_scale, "应有色阶");
    assert!(has_bar, "应有数据条");
    assert!(has_expr, "应有公式规则");
}

#[test]
fn overlay_matches_values() {
    let Some(wb) = load() else { return };
    let sheet = wb.sheet(0);
    let cf = &sheet.cf;
    let mut st = CfStats::default();
    compute_stats(cf, sheet, &mut st);

    // B2=50 不该命中「大于100」；B3=180 该命中（红底）
    let o50 = overlay_at(cf, &st, sheet, 1, 1, |_, _, _| false);
    let o180 = overlay_at(cf, &st, sheet, 2, 1, |_, _, _| false);
    assert!(o50.fill.is_none() || o50.bar.is_some(), "50 不该被红底命中");
    assert_eq!(o180.fill, Some([255, 199, 206]), "180 应命中红底: {:?}", o180.fill);
    assert_eq!(o180.color, Some([156, 0, 6]), "180 应命中深红字");
    // 数据条：50 是区间最小值，比例应接近 0；320 是最大值，比例 1
    assert!(o50.bar.is_some(), "应有数据条");
    let o320 = overlay_at(cf, &st, sheet, 3, 1, |_, _, _| false);
    assert!((o320.bar.unwrap().0 - 1.0).abs() < 0.01);
}

#[test]
fn expression_rule_uses_block_anchor() {
    let Some(wb) = load() else { return };
    let sheet = wb.sheet(0);
    let cf = &sheet.cf;
    let mut st = CfStats::default();
    compute_stats(cf, sheet, &mut st);
    // D 列公式规则：包含"急"→ 黄底。D3=紧急、D4=很急 命中，D2/D5=正常 不命中
    let mut eval = |f: &str, dr: u32, dc: u32| {
        let shifted = sheetcore::cf::shift_formula(f.trim_start_matches('='), dr, dc);
        match sheetcore::formula::parse(&shifted) {
            Ok(n) => {
                let mut calc = sheetcore::formula::Calc::new(&wb);
                matches!(calc.eval_node(&n, 0), sheetcore::formula::FValue::Bool(true))
            }
            Err(_) => false,
        }
    };
    let o2 = overlay_at(cf, &st, sheet, 1, 3, &mut eval);
    let o3 = overlay_at(cf, &st, sheet, 2, 3, &mut eval);
    let o4 = overlay_at(cf, &st, sheet, 3, 3, &mut eval);
    let o5 = overlay_at(cf, &st, sheet, 4, 3, &mut eval);
    assert!(o2.fill.is_none(), "D2 正常不该命中");
    assert_eq!(o3.fill, Some([255, 235, 156]), "D3 紧急应命中");
    assert_eq!(o4.fill, Some([255, 235, 156]), "D4 很急应命中");
    assert!(o5.fill.is_none(), "D5 正常不该命中");
}

#[test]
fn write_new_rules_and_save() {
    let Some(mut wb) = load() else { return };
    let out = out_path("qa02_cf_edit");
    let _ = std::fs::remove_file(&out);

    // 追加一条：A 列包含"乙" → 加粗
    {
        let dxf_idx = wb.sheets[0].cf.dxfs.len();
        wb.sheets[0]
            .cf
            .dxfs
            .push(sheetcore::cf::Dxf { bold: true, color: None, fill: Some([198, 239, 206]), italic: false, font: None, num_fmt: None, border: None });
        let sheet = &mut wb.sheets[0];
        sheet.cf.blocks.push(sheetcore::cf::CfBlock {
            ranges: vec![(1, 0, 4, 0)],
            rules: vec![sheetcore::cf::CfRule {
                kind: CfKind::Text { contains: "乙".into(), dxf: dxf_idx },
                stop: false,
             priority: 1, }],
        });
        sheet.cf_dirty = true;
    }
    sheetcore::xlsx::save(&out, &wb).expect("保存");

    // 读回来：新规则还在
    let back = sheetcore::xlsx::load(&out).expect("重新读入");
    let cf: &CfSheet = &back.sheets[0].cf;
    let found = cf.blocks.iter().any(|b| {
        b.rules.iter().any(|r| match &r.kind {
            CfKind::Text { contains, .. } => contains == "乙",
            _ => false,
        })
    });
    assert!(found, "新加的文本规则应保存成功: {:?}", cf.blocks.len());
    // 原有的规则也不能丢
    assert!(cf
        .blocks
        .iter()
        .any(|b| b.rules.iter().any(|r| matches!(r.kind, CfKind::CellIs { .. }))));
}

#[test]
fn icon_sets_resolve_levels() {
    let p = fixture("icons.xlsx");
    assert!(p.exists(), "required test fixture is missing: {}", p.display());
    let wb = sheetcore::xlsx::load(&p).expect("读入");
    let sheet = wb.sheet(0);
    let cf = &sheet.cf;
    let mut st = CfStats::default();
    compute_stats(cf, sheet, &mut st);
    // A 列是红绿灯，B 列是箭头
    let a10 = overlay_at(cf, &st, sheet, 1, 0, |_, _, _| false).icon.expect("A2 应有图标");
    assert_eq!(a10.style, sheetcore::cf::IconStyle::TrafficLights);
    assert_eq!(a10.idx, 0);
    let a55 = overlay_at(cf, &st, sheet, 2, 0, |_, _, _| false).icon.unwrap();
    assert_eq!(a55.idx, 1);
    let a90 = overlay_at(cf, &st, sheet, 3, 0, |_, _, _| false).icon.unwrap();
    assert_eq!(a90.idx, 2);
    let b10 = overlay_at(cf, &st, sheet, 1, 1, |_, _, _| false).icon.unwrap();
    assert_eq!(b10.style, sheetcore::cf::IconStyle::Arrows);
    assert_eq!(b10.idx, 0);
    let b90 = overlay_at(cf, &st, sheet, 3, 1, |_, _, _| false).icon.unwrap();
    assert_eq!(b90.idx, 2);
}

#[test]
fn icon_set_names_parse() {
    use sheetcore::cf::{IconMark, IconStyle};
    assert_eq!(IconMark::from_set("3TrafficLights1"), Some((IconStyle::TrafficLights, 3)));
    assert_eq!(IconMark::from_set("5Arrows"), Some((IconStyle::Arrows, 5)));
    assert_eq!(IconMark::from_set("4Rating"), Some((IconStyle::Rating, 4)));
    assert_eq!(IconMark::from_set("3Flags"), Some((IconStyle::Flags, 3)));
    assert_eq!(IconMark::from_set("5Quarters"), Some((IconStyle::Quarters, 5)));
    assert_eq!(IconMark::from_set("3Symbols"), Some((IconStyle::Symbols, 3)));
}
