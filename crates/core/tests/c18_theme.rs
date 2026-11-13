//! C18.1：主题色 + tint 的解析与保存保真。
//!
//! Excel 实测（`scripts/c18_theme_fixture.ps1`，COM 的 Color 是 BGR）：
//! A1 字体「蓝色，着色 1，淡色 40%」→ RGB #8EA9DB（theme=4 tint=0.39997558519241921）
//! A1 填充「橙色，着色 2，深色 25%」→ RGB #C65911（theme=5 tint=-0.249977111117893）

use std::io::Read;
use std::path::{Path, PathBuf};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/c18/theme.xlsx")
}

#[test]
fn theme_color_and_tint_resolve_like_excel() {
    let wb = sheetcore::xlsx::load(&fixture()).expect("加载主题夹具");
    let sh = &wb.sheets[0];
    let st = wb.styles.get(sh.style_of(0, 0));
    // Excel 的 HSL tint 与逐通道换算存在 ±1 的舍入差（#8EA9DB / #C65911），
    // 这里按 ±2 比对；主题索引与 tint 值必须精确。
    assert!(
        (0..3).all(|i| (st.color[i] as i16 - [0x8E, 0xA9, 0xDB][i] as i16).abs() <= 2),
        "主题字体色 {:?}，Excel #8EA9DB",
        st.color
    );
    let fill = st.fill.expect("有填充");
    assert!(
        (0..3).all(|i| (fill[i] as i16 - [0xC6, 0x59, 0x11][i] as i16).abs() <= 2),
        "主题填充色 {fill:?}，Excel #C65911"
    );
    let id = sh.style_of(0, 0);
    let t = wb.styles.theme_of(id);
    assert_eq!(t.font.map(|f| f.idx), Some(4), "字体 theme 索引");
    assert!((t.font.unwrap().tint - 0.3999756).abs() < 1e-4);
    assert_eq!(t.fill.map(|f| f.idx), Some(5), "填充 theme 索引");
    assert!((t.fill.unwrap().tint + 0.2499771).abs() < 1e-4);
}

#[test]
fn theme_refs_survive_save_and_reload() {
    let wb = sheetcore::xlsx::load(&fixture()).expect("加载主题夹具");
    let tmp = std::env::temp_dir().join("sc_c18_theme_rt.xlsx");
    let _ = std::fs::remove_file(&tmp);
    sheetcore::xlsx::save(&tmp, &wb).expect("保存");

    // 解包看 styles.xml：必须还是 theme，而不是降级成 rgb
    let file = std::fs::File::open(&tmp).unwrap();
    let mut zip = zip::ZipArchive::new(file).unwrap();
    let mut styles = String::new();
    zip.by_name("xl/styles.xml").unwrap().read_to_string(&mut styles).unwrap();
    assert!(styles.contains("theme=\"4\""), "字体主题索引丢了：{styles}");
    assert!(styles.contains("theme=\"5\""), "填充主题索引丢了：{styles}");
    assert!(styles.contains("tint=\"0.399975585192419"), "字体 tint 丢了");
    assert!(
        styles.contains("tint=\"-0.249977111117893\""),
        "填充 tint 丢了：{styles}"
    );

    // 重开颜色不变
    let again = sheetcore::xlsx::load(&tmp).expect("重开");
    let sh = &again.sheets[0];
    let st = again.styles.get(sh.style_of(0, 0));
    assert_eq!(st.color, [0x8F, 0xAA, 0xDC]);
    assert_eq!(st.fill, Some([0xC5, 0x5A, 0x11]));
}

#[test]
fn editing_another_property_does_not_drop_theme() {
    let mut wb = sheetcore::xlsx::load(&fixture()).expect("加载主题夹具");
    // 改 A1 的粗体，颜色主题不动
    let sid = wb.sheets[0].style_of(0, 0);
    let mut st = wb.styles.get(sid).clone();
    st.bold = true;
    let new_id = wb.styles.intern(st);
    wb.sheets[0].set_style(0, 0, new_id);
    wb.styles.set_theme(new_id, wb.styles.theme_of(sid));
    let tmp = std::env::temp_dir().join("sc_c18_theme_edit.xlsx");
    let _ = std::fs::remove_file(&tmp);
    sheetcore::xlsx::save(&tmp, &wb).expect("保存");

    let file = std::fs::File::open(&tmp).unwrap();
    let mut zip = zip::ZipArchive::new(file).unwrap();
    let mut styles = String::new();
    zip.by_name("xl/styles.xml").unwrap().read_to_string(&mut styles).unwrap();
    assert!(styles.contains("theme=\"4\""), "改粗体后字体主题丢了");
    assert!(styles.contains("theme=\"5\""), "改粗体后填充主题丢了");

    let again = sheetcore::xlsx::load(&tmp).expect("重开");
    let sh = &again.sheets[0];
    let st = again.styles.get(sh.style_of(0, 0));
    assert!(st.bold);
    assert_eq!(st.color, [0x8F, 0xAA, 0xDC]);
    assert_eq!(st.fill, Some([0xC5, 0x5A, 0x11]));
}
