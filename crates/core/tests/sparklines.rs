//! 迷你图读写：基线文件由本机 Excel 365 生成（三种类型 + 一个双迷你图组）。

use sheetcore::model::Workbook;
use sheetcore::sparkline::{EmptyAs, SparkKind};

fn base() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data/sparklines.xlsx")
}

fn tmp(name: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("sheets-native-spark-{}-{name}", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

fn part_xml(path: &std::path::Path, part: &str) -> String {
    let targets = sheetcore::patch::sheet_targets(path).unwrap();
    let name = targets.values().next().unwrap().clone();
    let _ = part;
    let bytes = sheetcore::fixups::read_part_file(path, &name).unwrap();
    String::from_utf8(bytes).unwrap()
}

#[test]
fn reads_excel_sparklines() {
    let wb = sheetcore::xlsx::load(&base()).unwrap();
    let sh = wb.sheet(0);
    assert_eq!(sh.sparklines.len(), 5, "Excel 样本里 5 条迷你图");
    let kinds: Vec<SparkKind> = sh.sparklines.iter().map(|s| s.kind).collect();
    assert!(kinds.contains(&SparkKind::Line));
    assert!(kinds.contains(&SparkKind::Column));
    assert!(kinds.contains(&SparkKind::WinLoss));
    // 锚点分别是 F1/F2/F3/G1/G2 中的四个
    let mut anchors: Vec<(u32, u32)> = sh.sparklines.iter().map(|s| (s.r, s.c)).collect();
    anchors.sort();
    assert_eq!(
        anchors,
        vec![(0, 5), (0, 6), (1, 5), (1, 6), (2, 5)],
        "锚点：F1 G1 F2 G2 F3"
    );
    // 数据区都是 A:E 五列
    for s in &sh.sparklines {
        assert_eq!(s.rect.1, 0);
        assert_eq!(s.rect.3, 4);
        assert_eq!(s.point_count(), 5);
        assert_eq!(s.source_sheet.as_deref(), Some("S1"));
    }
}

#[test]
fn edit_and_save_roundtrip() {
    let mut wb = sheetcore::xlsx::load(&base()).unwrap();
    {
        let sh = &mut wb.sheets[0];
        // F1 那条改成绿色柱形 + 高低点 + 空值当零
        let idx = sh
            .sparklines
            .iter()
            .position(|s| (s.r, s.c) == (0, 5))
            .expect("F1");
        let sp = &mut sh.sparklines[idx];
        sp.kind = SparkKind::Column;
        sp.color = [0x00, 0xB0, 0x50];
        sp.high = true;
        sp.low = true;
        sp.empty = EmptyAs::Zero;
        // 新增一条：锚点 H1，数据 A3:E3，盈亏
        let mut add = sp.clone();
        add.r = 0;
        add.c = 7;
        add.rect = (2, 0, 2, 4);
        add.kind = SparkKind::WinLoss;
        sh.sparklines.push(add);
        sh.sparklines_dirty = true;
        wb.dirty = true;
    }
    let out = tmp("edit.xlsx");
    sheetcore::xlsx::save(&out, &wb).unwrap();

    let xml = part_xml(&out, "");
    assert!(xml.contains("x14:sparklineGroups"), "保存后要有迷你图扩展");
    assert!(xml.contains("type=\"column\""), "柱形组");
    assert!(xml.contains("type=\"stacked\""), "盈亏组");
    assert!(xml.contains("high=\"1\""));
    assert!(xml.contains("displayEmptyCellsAs=\"zero\""));
    assert!(xml.contains("FF00B050"), "自定义颜色");
    assert!(xml.contains("<xm:sqref>H1</xm:sqref>"), "新增的锚点");
    // extLst 必须是 worksheet 的最后一个子元素
    let tail = xml.trim_end();
    assert!(tail.ends_with("</worksheet>"));
    let ext_at = xml.rfind("<extLst").unwrap();
    assert!(xml[ext_at..].contains("x14:sparklineGroups"));

    // 用我们自己的加载器回读
    let wb2 = sheetcore::xlsx::load(&out).unwrap();
    let list = &wb2.sheet(0).sparklines;
    assert_eq!(list.len(), 6, "原 5 条 + 新增 H1");
    let h1 = list.iter().find(|s| (s.r, s.c) == (0, 7)).expect("H1");
    assert_eq!(h1.kind, SparkKind::WinLoss);
    assert_eq!(h1.rect, (2, 0, 2, 4));
    let f1 = list.iter().find(|s| (s.r, s.c) == (0, 5)).expect("F1");
    assert_eq!(f1.kind, SparkKind::Column);
    assert_eq!(f1.color, [0x00, 0xB0, 0x50]);
    assert!(f1.high && f1.low);
    let _ = std::fs::remove_file(&out);
}

#[test]
fn clearing_all_removes_the_extension() {
    let mut wb = sheetcore::xlsx::load(&base()).unwrap();
    wb.sheets[0].sparklines.clear();
    wb.sheets[0].sparklines_dirty = true;
    let out = tmp("clear.xlsx");
    sheetcore::xlsx::save(&out, &wb).unwrap();
    let xml = part_xml(&out, "");
    assert!(!xml.contains("sparklineGroups"), "清空后不该留扩展：{xml}");
    assert!(!xml.contains("05C60535"), "扩展 uri 也要摘掉");
    let _ = std::fs::remove_file(&out);
}

#[test]
fn structural_insert_moves_anchor_and_source() {
    let mut wb = sheetcore::xlsx::load(&base()).unwrap();
    // F1 的锚点在第 0 行；上面插一行 → 变 F2，数据区跟着下移
    sheetcore::structural::apply_structural_wb(
        &mut wb,
        0,
        &sheetcore::structural::Structural::InsertRows { at: 0, n: 1 },
    );
    let sh = wb.sheet(0);
    let sp = sh
        .sparklines
        .iter()
        .find(|s| s.c == 5 && s.kind == SparkKind::Line)
        .or_else(|| sh.sparklines.iter().find(|s| s.c == 5))
        .expect("F 列那条还在");
    assert!(sp.r >= 1, "锚点被推到第 2 行及以后：{sp:?}");
    assert!(sp.rect.0 >= 1, "数据区一起下移：{sp:?}");
}

#[test]
fn delete_anchor_row_drops_the_sparkline() {
    let mut wb = sheetcore::xlsx::load(&base()).unwrap();
    let before = wb.sheet(0).sparklines.len();
    // 删掉第 3 行（F3 的锚点）
    sheetcore::structural::apply_structural_wb(
        &mut wb,
        0,
        &sheetcore::structural::Structural::DeleteRows { at: 2, n: 1 },
    );
    let sh = wb.sheet(0);
    assert_eq!(sh.sparklines.len(), before - 1, "锚点被删的那条要移除");
    assert!(!sh.sparklines.iter().any(|s| (s.r, s.c) == (2, 5)));
}
