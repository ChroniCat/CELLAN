//! 文本框：drawing 部件往返（新建 → 保存 → 回读 → Excel 可读）。

use sheetcore::layout::Axis;
use sheetcore::model::Workbook;
use sheetcore::textbox::TextBox;

fn tmp(name: &str) -> std::path::PathBuf {
    let p = std::env::temp_dir().join(format!("sheets-native-tb-{}-{name}", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

fn zip_text(path: &std::path::Path, name: &str) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let mut zip = zip::ZipArchive::new(file).ok()?;
    let mut f = zip.by_name(name).ok()?;
    let mut s = String::new();
    use std::io::Read;
    f.read_to_string(&mut s).ok()?;
    Some(s)
}

fn zip_names(path: &std::path::Path) -> Vec<String> {
    let file = std::fs::File::open(path).unwrap();
    let zip = zip::ZipArchive::new(file).unwrap();
    (0..zip.len())
        .map(|i| zip.name_for_index(i).unwrap_or("").to_string())
        .collect()
}

#[test]
fn new_workbook_gets_drawing_part() {
    let mut wb = Workbook::new_blank();
    {
        let sh = &mut wb.sheets[0];
        let mut tb = TextBox {
            text: "这是文本框".into(),
            size: 14.0,
            bold: true,
            color: [0x11, 0x22, 0x33],
            fill: Some([0xFF, 0xF2, 0xCC]),
            ..Default::default()
        };
        let cols = Axis::new(72.0);
        let rows = Axis::new(19.0);
        tb.set_rect_px(&cols, &rows, 100.0, 40.0, 200.0, 60.0);
        sh.textboxes.push(tb);
        sh.textboxes_dirty = true;
        wb.dirty = true;
    }
    let out = tmp("new.xlsx");
    sheetcore::xlsx::save(&out, &wb).unwrap();

    let names = zip_names(&out);
    assert!(
        names.iter().any(|n| n.starts_with("xl/drawings/drawing") && n.ends_with(".xml")),
        "要建 drawing 部件：{names:?}"
    );
    let draw = names
        .iter()
        .find(|n| n.starts_with("xl/drawings/drawing") && n.ends_with(".xml"))
        .cloned()
        .unwrap();
    let xml = zip_text(&out, &draw).unwrap();
    assert!(xml.contains("<xdr:sp "), "形状没写进去：{xml}");
    assert!(xml.contains("这是文本框"));
    assert!(xml.contains("val=\"FFF2CC\""), "填充色：{xml}");
    assert!(xml.contains("sz=\"1400\""), "字号 14 磅：{xml}");
    assert!(xml.contains("b=\"1\""), "粗体：{xml}");

    // 工作表里要有 <drawing r:id>
    let sheet = zip_text(&out, "xl/worksheets/sheet1.xml").unwrap();
    assert!(sheet.contains("<drawing "), "工作表缺 drawing 引用：{sheet}");
    // 关系表里要有一条 drawing 关系
    let rels = zip_text(&out, "xl/worksheets/_rels/sheet1.xml.rels").unwrap();
    assert!(rels.contains("relationships/drawing"), "rels 缺 drawing：{rels}");
    // content types 要有 drawing 的 Override
    let ct = zip_text(&out, "[Content_Types].xml").unwrap();
    assert!(ct.contains("drawing+xml"), "content types 缺 drawing：{ct}");

    // 回读
    let wb2 = sheetcore::xlsx::load(&out).unwrap();
    let list = &wb2.sheet(0).textboxes;
    assert_eq!(list.len(), 1, "回读不到文本框");
    assert_eq!(list[0].text, "这是文本框");
    assert!((list[0].size - 14.0).abs() < 0.01);
    assert!(list[0].bold);
    assert_eq!(list[0].fill, Some([0xFF, 0xF2, 0xCC]));
    assert_eq!(list[0].color, [0x11, 0x22, 0x33]);
    let _ = std::fs::remove_file(&out);
}

#[test]
fn editing_moves_and_rewrites_in_place() {
    let mut wb = Workbook::new_blank();
    {
        let sh = &mut wb.sheets[0];
        sh.textboxes.push(TextBox {
            text: "原文".into(),
            ..Default::default()
        });
        sh.textboxes_dirty = true;
    }
    let out = tmp("edit.xlsx");
    sheetcore::xlsx::save(&out, &wb).unwrap();
    let before = zip_names(&out)
        .into_iter()
        .filter(|n| n.starts_with("xl/drawings/drawing"))
        .count();
    assert_eq!(before, 1, "第一次只建一个 drawing");

    // 改文字 + 移动，再存一次：不能多出 drawing 部件，也不能留下旧形状
    let mut wb2 = sheetcore::xlsx::load(&out).unwrap();
    {
        let sh = &mut wb2.sheets[0];
        let cols = Axis::new(72.0);
        let rows = Axis::new(19.0);
        let tb = &mut sh.textboxes[0];
        tb.text = "改过的文字".into();
        tb.set_rect_px(&cols, &rows, 50.0, 30.0, 160.0, 50.0);
        sh.textboxes_dirty = true;
    }
    sheetcore::xlsx::save(&out, &wb2).unwrap();
    let names = zip_names(&out);
    let drawings = names
        .iter()
        .filter(|n| n.starts_with("xl/drawings/drawing") && n.ends_with(".xml"))
        .count();
    assert_eq!(drawings, 1, "复用原有 drawing：{names:?}");
    let draw = names
        .iter()
        .find(|n| n.starts_with("xl/drawings/drawing") && n.ends_with(".xml"))
        .cloned()
        .unwrap();
    let xml = zip_text(&out, &draw).unwrap();
    assert!(xml.contains("改过的文字"));
    assert!(!xml.contains("原文"), "旧形状没清掉：{xml}");
    assert_eq!(xml.matches("<xdr:sp ").count(), 1);

    let wb3 = sheetcore::xlsx::load(&out).unwrap();
    assert_eq!(wb3.sheet(0).textboxes[0].text, "改过的文字");
    let _ = std::fs::remove_file(&out);
}

#[test]
fn deleting_all_textboxes_removes_the_shape() {
    let mut wb = Workbook::new_blank();
    {
        let sh = &mut wb.sheets[0];
        sh.textboxes.push(TextBox {
            text: "会删掉".into(),
            ..Default::default()
        });
        sh.textboxes_dirty = true;
    }
    let out = tmp("del.xlsx");
    sheetcore::xlsx::save(&out, &wb).unwrap();
    let mut wb2 = sheetcore::xlsx::load(&out).unwrap();
    wb2.sheets[0].textboxes.clear();
    wb2.sheets[0].textboxes_dirty = true;
    sheetcore::xlsx::save(&out, &wb2).unwrap();
    let xml = zip_text(&out, "xl/drawings/drawing1.xml").unwrap_or_default();
    assert!(!xml.contains("<xdr:sp"), "形状应被清空：{xml}");
    let wb3 = sheetcore::xlsx::load(&out).unwrap();
    assert!(wb3.sheet(0).textboxes.is_empty());
    let _ = std::fs::remove_file(&out);
}

#[test]
fn textbox_rect_uses_column_widths() {
    let mut cols = Axis::new(72.0);
    cols.set(1, 150.0);
    let rows = Axis::new(19.0);
    let mut tb = TextBox::default();
    tb.set_rect_px(&cols, &rows, 90.0, 5.0, 120.0, 40.0);
    let (x, y, w, h) = tb.rect_px(&cols, &rows);
    assert!((x - 90.0).abs() < 1.0, "x={x}");
    assert!((w - 120.0).abs() < 1.5, "w={w}");
    assert!((y - 5.0).abs() < 1.0);
    assert!((h - 40.0).abs() < 1.5);
}

#[test]
fn shape_undo_restores_textboxes() {
    use sheetcore::undo::{Action, ShapeEdit};
    let mut wb = Workbook::new_blank();
    wb.sheets[0].textboxes.push(TextBox {
        text: "原来".into(),
        ..Default::default()
    });
    let before = wb.sheets[0].textboxes.clone();
    wb.sheets[0].textboxes[0].text = "改过".into();
    let after = wb.sheets[0].textboxes.clone();
    let mut stack = sheetcore::undo::UndoStack::new();
    stack.push(Action {
        label: "修改文本框".into(),
        merges: None,
        deltas: Vec::new(),
        focus: (0, 0, 0),
        snap_before: None,
        snap_after: None,
        by_site: 0,
        pictures: None,
        shapes: Some(ShapeEdit {
            sheet: 0,
            before,
            after,
        }),
        refresh_sizes: None,
    });
    assert!(stack.can_undo(), "形状动作要能撤销");
    let applied = stack
        .undo(&mut wb.sheets, |_, _, _, _| true)
        .expect("撤销");
    assert_eq!(applied.label, "修改文本框");
    assert_eq!(wb.sheets[0].textboxes[0].text, "原来");
    assert!(wb.sheets[0].textboxes_dirty, "撤销后要标记待写回");
    stack.redo(&mut wb.sheets, |_, _, _, _| true).expect("重做");
    assert_eq!(wb.sheets[0].textboxes[0].text, "改过");
}
