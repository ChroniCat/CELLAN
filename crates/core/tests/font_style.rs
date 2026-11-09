//! 字体三层的第一层：样式携带字体名、xlsx 双向映射、条件格式 dxf 字体。
//!
//! 这几条都是「选了字体 → 存盘 → 重开还在」的硬约束，缺一条就是半成品。

use sheetcore::model::{Cell, Value, Workbook};
use sheetcore::style::{font_class, normalize_font, CellStyle, FontClass};

fn part(path: &std::path::Path, name: &str) -> String {
    let bytes = sheetcore::fixups::read_part_file(path, name)
        .unwrap_or_else(|| panic!("包里没有 {name}"));
    String::from_utf8(bytes).expect("utf-8")
}

fn tmp(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join("sheet_native_font");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    let _ = std::fs::remove_file(&p);
    p
}

fn set_text(wb: &mut Workbook, r: u32, c: u32, text: &str, font: &str) {
    wb.sheets[0].set(
        r,
        c,
        Cell {
            value: Value::Text(text.into()),
            formula: None,
        },
    );
    let id = wb.styles.intern(CellStyle {
        font: font.into(),
        size: 11.0,
        ..Default::default()
    });
    wb.sheets[0].set_style(r, c, id);
}

#[test]
fn save_writes_font_name_and_groups_by_name() {
    let path = tmp("font_write.xlsx");
    let mut wb = Workbook::new_blank();
    set_text(&mut wb, 0, 0, "宋体一", "宋体");
    set_text(&mut wb, 1, 0, "宋体二", "宋体");
    set_text(&mut wb, 2, 0, "黑体", "黑体");
    sheetcore::xlsx::save(&path, &wb).expect("保存");

    let styles = part(&path, "xl/styles.xml");
    assert!(
        styles.contains("<name val=\"宋体\"/>"),
        "styles.xml 里没有宋体字体名：{styles}"
    );
    assert!(styles.contains("<name val=\"黑体\"/>"), "styles.xml 里没有黑体字体名");
    // 同名只留一条字体条目（按名字分组）
    assert_eq!(
        styles.matches("<name val=\"宋体\"/>").count(),
        1,
        "宋体应该合并成一条 font：{styles}"
    );
}

#[test]
fn font_survives_save_close_reopen() {
    let path = tmp("font_roundtrip.xlsx");
    let mut wb = Workbook::new_blank();
    set_text(&mut wb, 0, 0, "宋体正文", "宋体");
    set_text(&mut wb, 1, 0, "思源宋体", "思源宋体");
    sheetcore::xlsx::save(&path, &wb).expect("保存");

    // 关闭 → 重开
    let back = sheetcore::xlsx::load(&path).expect("重新读入");
    let st0 = back.styles.get(back.sheets[0].style_of(0, 0));
    assert_eq!(st0.font, "宋体", "重开后 A1 的字体丢了");
    let st1 = back.styles.get(back.sheets[0].style_of(1, 0));
    assert_eq!(st1.font, "思源宋体", "重开后 A2 的字体丢了");
    assert_eq!(font_class(&st0.font), FontClass::Serif);
}

#[test]
fn changing_bold_keeps_font() {
    // 只改粗体时不能把原字体冲掉（"能显示但保存丢" 的经典路径）
    let first = tmp("font_bold_1.xlsx");
    let second = tmp("font_bold_2.xlsx");
    let mut wb = Workbook::new_blank();
    set_text(&mut wb, 0, 0, "宋体加粗", "宋体");
    sheetcore::xlsx::save(&first, &wb).expect("首存");

    let mut wb2 = sheetcore::xlsx::load(&first).expect("读入");
    let mut st = wb2.styles.get(wb2.sheets[0].style_of(0, 0)).clone();
    assert_eq!(st.font, "宋体");
    st.bold = true;
    let id = wb2.styles.intern(st);
    wb2.sheets[0].set_style(0, 0, id);
    sheetcore::xlsx::save(&second, &wb2).expect("另存");

    let back = sheetcore::xlsx::load(&second).expect("再读入");
    let st = back.styles.get(back.sheets[0].style_of(0, 0));
    assert!(st.bold, "粗体没存进去");
    assert_eq!(st.font, "宋体", "改粗体把字体冲掉了");
    let styles = part(&second, "xl/styles.xml");
    assert!(styles.contains("<name val=\"宋体\"/>"), "{styles}");
}

#[test]
fn unknown_font_is_kept_not_rewritten() {
    // 认不出来的字体名原样保留（渲染回退默认，但写盘不改用户选择）
    let path = tmp("font_unknown.xlsx");
    let mut wb = Workbook::new_blank();
    set_text(&mut wb, 0, 0, "等线", "等线");
    sheetcore::xlsx::save(&path, &wb).expect("保存");
    let back = sheetcore::xlsx::load(&path).expect("读入");
    assert_eq!(back.styles.get(back.sheets[0].style_of(0, 0)).font, "等线");
    assert_eq!(font_class("等线"), FontClass::Sans);
    assert_eq!(normalize_font("DengXian"), "等线");
}

#[test]
fn dxf_font_roundtrip() {
    use sheetcore::cf::parse_dxfs;
    let xml = r#"<styleSheet><dxfs count="2"><dxf><font><b/><name val="微软雅黑"/></font></dxf><dxf><font><color rgb="FF9C0006"/></font><fill><patternFill patternType="solid"><bgColor rgb="FFFFC7CE"/></patternFill></fill></dxf></dxfs></styleSheet>"#;
    let dxfs = parse_dxfs(xml);
    assert_eq!(dxfs.len(), 2);
    assert_eq!(dxfs[0].font.as_deref(), Some("微软雅黑"), "dxf 字体没读出来");
    assert!(dxfs[0].bold);
    assert_eq!(dxfs[1].font, None, "没写字体的 dxf 不该编一个");

    // 写回再解析，字体名不丢
    let out = sheetcore::patch::replace_dxfs(xml, &dxfs);
    assert!(out.contains("<name val=\"微软雅黑\"/>"), "{out}");
    let again = parse_dxfs(&out);
    assert_eq!(again[0].font.as_deref(), Some("微软雅黑"));
    assert_eq!(
        out.matches("<dxfs").count(),
        1,
        "dxfs 段应整段替换，不该重复：{out}"
    );
}

#[test]
fn conditional_overlay_carries_font() {
    // 条件格式命中时，Overlay 里带着 dxf 的字体名，渲染层据此换 family
    use sheetcore::cf::{compute_stats, overlay_at, CfBlock, CfKind, CfSheet, CfStats, Dxf};
    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(0, 0, Cell { value: Value::Number(5.0), formula: None });
    let sheet = &mut wb.sheets[0];
    sheet.cf.dxfs.push(Dxf { bold: true, font: Some("宋体".into()), ..Default::default() });
    sheet.cf.blocks.push(CfBlock {
        ranges: vec![(0, 0, 0, 0)],
        rules: vec![sheetcore::cf::CfRule {
            kind: CfKind::CellIs {
                op: sheetcore::cf::CfOp::Greater,
                a: "1".into(),
                b: String::new(),
                dxf: 0,
            },
            stop: false,
        }],
    });
    let sheet = &wb.sheets[0];
    let mut stats = CfStats::default();
    compute_stats(&sheet.cf, sheet, &mut stats);
    let ov = overlay_at(&sheet.cf, &stats, sheet, 0, 0, |_, _, _| false);
    assert_eq!(ov.font.as_deref(), Some("宋体"), "条件格式的字体没传到 Overlay");
    assert!(!ov.is_empty());
    let _ = CfSheet::default();
}
