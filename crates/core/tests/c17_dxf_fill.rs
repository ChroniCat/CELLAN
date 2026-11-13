//! C17 差异格式解析：未建模边框方向（对角等）的颜色不能污染填充。

use sheetcore::cf::parse_dxfs;
use sheetcore::xlsx;

#[test]
fn diagonal_color_does_not_become_fill() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/c17/borders.xlsx");
    let wb = xlsx::load(&path).expect("load borders fixture");
    let styles = &wb.sheet(0).cf;
    assert_eq!(styles.dxfs.len(), 2, "夹具应有两条 dxf");
    let four = &styles.dxfs[0];
    assert!(four.fill.is_none(), "四边框 dxf 没有填充，不能凭空造出填充");
    let b = four.border.as_ref().expect("四边框");
    assert_eq!(b.sides[0].as_ref().unwrap().0, "thick");
    assert_eq!(b.sides[1].as_ref().unwrap().0, "thin");
    assert_eq!(b.sides[2].as_ref().unwrap().0, "medium");
    assert_eq!(b.sides[3].as_ref().unwrap().0, "double");

    let diag = &styles.dxfs[1];
    assert!(
        diag.fill.is_none(),
        "第二条 dxf 只有黑色对角边框，黑色不能被读成填充：{:?}",
        diag.fill
    );
    assert!(
        diag.border.as_ref().map(|x| x.sides.iter().all(|s| s.is_none())).unwrap_or(true),
        "对角不是四边，不能塞进上/下/左/右"
    );
    assert!(sheetcore::cf::dxfs_unsupported(
        &{
            let f = std::fs::File::open(&path).unwrap();
            let mut z = zip::ZipArchive::new(f).unwrap();
            let mut s = String::new();
            use std::io::Read;
            z.by_name("xl/styles.xml").unwrap().read_to_string(&mut s).unwrap();
            s
        }
    ), "含对角边框应提示不可无损编辑");
}

#[test]
fn real_fill_still_parses() {
    let xml = r#"<dxfs count="1"><dxf><font><b/></font><fill><patternFill><bgColor rgb="FFEAEAEA"/></patternFill></fill></dxf></dxfs>"#;
    let d = parse_dxfs(xml);
    assert_eq!(d.len(), 1);
    assert!(d[0].bold);
    assert_eq!(d[0].fill, Some([0xEA, 0xEA, 0xEA]), "真正的填充仍要读出来");
}

#[test]
fn saving_does_not_turn_diagonal_into_fill() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/c17/borders.xlsx");
    let wb = xlsx::load(&path).expect("load");
    let mut out = std::env::temp_dir();
    out.push(format!("sc_c17_dxfsave_{}.xlsx", std::process::id()));
    let _ = std::fs::remove_file(&out);
    xlsx::save(&out, &wb).expect("save");
    let back = xlsx::load(&out).expect("reload");
    let styles = &back.sheet(0).cf;
    for (i, d) in styles.dxfs.iter().enumerate() {
        assert!(d.fill.is_none(), "保存后第 {i} 条 dxf 出现了假填充 {:?}", d.fill);
    }
    let _ = std::fs::remove_file(&out);
}
