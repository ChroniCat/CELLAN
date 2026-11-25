use sheetcore::model::{Cell, Sheet, SheetVisibility, Value, Workbook};
use sheetcore::protection::{ProtectedRange, SheetProtection, WorkbookProtection};
use sheetcore::revision::{self, Change, Entry, Snap};
use sheetcore::sync::{Cursor, Msg, SheetMetaKind, SheetTabSync};
use sheetcore::undo::UndoStack;
use sheetcore::{formula, xlsx};

fn tmp_xlsx(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join("cellan_gap7_tests").join(tag);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir.join("book.xlsx")
}

#[test]
fn sheet_visibility_very_hidden_view_state_and_protection_ooxml_roundtrip() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "总览".into();
    wb.sheets[0].zoom_scale = 135;
    wb.sheets[0].show_gridlines = false;
    wb.sheets[0].show_headings = false;
    wb.sheets[0].view_dirty = true;
    wb.sheets[0].protection = Some(SheetProtection::new_with_password("sh123"));
    wb.sheets[0].protect_dirty = true;
    wb.sheets[0].protected_ranges.push(ProtectedRange::new(
        "预算填报区",
        vec![(1, 1, 5, 3)],
        "edit99",
    ));
    wb.sheets[0].protected_ranges.push(ProtectedRange::new(
        "公开备注区",
        vec![(6, 0, 8, 2)],
        "",
    ));
    wb.sheets[0].protected_ranges_dirty = true;

    let mut sh2 = Sheet::new("普通隐藏");
    sh2.set_visibility(SheetVisibility::Hidden);
    wb.sheets.push(sh2);

    let mut sh3 = Sheet::new("深度隐藏底稿");
    sh3.set_visibility(SheetVisibility::VeryHidden);
    wb.sheets.push(sh3);

    wb.protection = Some(WorkbookProtection::new_with_password("wb777", true, false));
    wb.protection_dirty = true;

    let path = tmp_xlsx("roundtrip");
    xlsx::save(&path, &wb).expect("save xlsx");

    let loaded = xlsx::load(&path).expect("reload xlsx");
    assert_eq!(loaded.sheets.len(), 3);
    assert_eq!(loaded.sheets[0].zoom_scale, 135);
    assert!(!loaded.sheets[0].show_gridlines);
    assert!(!loaded.sheets[0].show_headings);
    assert!(loaded.sheets[0].protection.as_ref().unwrap().verify("sh123"));
    assert_eq!(loaded.sheets[0].protected_ranges.len(), 2);
    assert_eq!(loaded.sheets[0].protected_ranges[0].title, "预算填报区");
    assert!(loaded.sheets[0].protected_ranges[0].contains(2, 2));
    assert!(loaded.sheets[0].protected_ranges[0].verify("edit99"));
    assert!(!loaded.sheets[0].protected_ranges[0].verify("wrong"));
    assert!(!loaded.sheets[0].protected_ranges[1].has_password());
    assert!(loaded.sheets[0].cell_in_protected_range(7, 1).is_some());

    assert_eq!(loaded.sheets[0].visibility(), SheetVisibility::Visible);
    assert_eq!(loaded.sheets[1].visibility(), SheetVisibility::Hidden);
    assert_eq!(loaded.sheets[2].visibility(), SheetVisibility::VeryHidden);
    assert!(loaded.sheets[2].very_hidden && loaded.sheets[2].is_hidden());

    let wp = loaded.protection.as_ref().expect("workbookProtection loaded");
    assert!(wp.lock_structure);
    assert!(!wp.lock_windows);
    assert!(wp.verify("wb777"));
    assert!(!wp.verify("bad"));
}

#[test]
fn sheet_delete_move_copy_are_undoable_and_preserve_cross_sheet_formulas() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "汇总".into();
    let mut sh2 = Sheet::new("明细");
    sh2.set(0, 0, Cell { value: Value::Number(42.0), formula: None });
    wb.sheets.push(sh2);
    wb.sheets[0].set(0, 0, Cell { value: Value::Empty, formula: Some("=明细!A1*2".into()) });
    formula::recalc_all(&mut wb);
    assert_eq!(wb.sheets[0].value(0, 0), Value::Number(84.0));

    let mut undo = UndoStack::new();
    // Move sheet 1 -> 0, undo, redo
    assert!(undo.move_sheet(&mut wb, 1, 0, (1, 0, 0)));
    assert_eq!(wb.sheets[0].name, "明细");
    assert_eq!(wb.sheets[1].name, "汇总");
    assert!(undo.undo_workbook(&mut wb, |_, _, _, _| true).is_some());
    assert_eq!(wb.sheets[0].name, "汇总");
    assert_eq!(wb.sheets[1].name, "明细");
    assert!(undo.redo_workbook(&mut wb, |_, _, _, _| true).is_some());
    assert_eq!(wb.sheets[0].name, "明细");
    assert!(undo.undo_workbook(&mut wb, |_, _, _, _| true).is_some());

    // Delete sheet 1 ("明细"): formula becomes #REF!, then Undo restores sheet + original formula!
    assert!(undo.delete_sheet(&mut wb, 1, (1, 0, 0)));
    assert_eq!(wb.sheets.len(), 1);
    assert!(wb.sheets[0].formula_of(0, 0).unwrap().contains("#REF!"));
    assert!(undo.undo_workbook(&mut wb, |_, _, _, _| true).is_some());
    assert_eq!(wb.sheets.len(), 2);
    assert_eq!(wb.sheets[1].name, "明细");
    assert_eq!(wb.sheets[1].value(0, 0), Value::Number(42.0));
    assert_eq!(wb.sheets[0].formula_of(0, 0), Some("=明细!A1*2"));
    formula::recalc_all(&mut wb);
    assert_eq!(wb.sheets[0].value(0, 0), Value::Number(84.0));

    // Redo delete removes sheet 1 again
    assert!(undo.redo_workbook(&mut wb, |_, _, _, _| true).is_some());
    assert_eq!(wb.sheets.len(), 1);
}

#[test]
fn structural_shifts_protected_ranges_and_collab_meta_wire_roundtrip() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].protected_ranges.push(ProtectedRange::new(
        "填报区",
        vec![(2, 1, 5, 3)],
        "",
    ));
    wb.sheets[0].insert_rows(1, 2);
    assert_eq!(wb.sheets[0].protected_ranges[0].sqref, vec![(4, 1, 7, 3)]);
    wb.sheets[0].delete_cols(2, 1);
    assert_eq!(wb.sheets[0].protected_ranges[0].sqref, vec![(4, 1, 7, 2)]);

    // Multi-cell cursor & SheetMetaOp wire roundtrip
    let cur = Msg::CursorMsg(Cursor {
        site: 3,
        name: "李四".into(),
        color: 1,
        sheet: 0,
        row: 1,
        col: 2,
        end_row: Some(5),
        end_col: Some(4),
    });
    assert!(cur.is_within_limits());
    let decoded = sheetcore::sync::decode_msg_compact(&sheetcore::sync::encode_msg_compact(&cur))
        .expect("decode cursor");
    match decoded {
        Msg::CursorMsg(c) => {
            assert_eq!((c.row, c.col, c.end_row, c.end_col), (1, 2, Some(5), Some(4)));
        }
        other => panic!("unexpected {other:?}"),
    }

    let meta = Msg::SheetMetaOp {
        site: 2,
        sheet: 0,
        ts: 99,
        kind: SheetMetaKind::SheetTab(SheetTabSync::Rename {
            index: 0,
            name: "Q1预算".into(),
        }),
    };
    assert!(meta.is_within_limits());
    let back: Msg = serde_json::from_str(&serde_json::to_string(&meta).unwrap()).unwrap();
    assert!(back.is_within_limits());
}

#[test]
fn revision_audit_filter_csv_export_and_isolated_storage() {
    let mut e1 = Entry::new("张三", 1, "编辑单元格", "总览");
    e1.cells.push(Change {
        r: 0,
        c: 0,
        before: Snap::default(),
        after: Snap { v: "100".into(), f: None, t: "n".into() },
        before_style: None,
        after_style: None,
    });
    let mut e2 = Entry::new("李四", 2, "设置格式", "明细");
    e2.note = "加粗标题".into();
    e2.remote = true;

    let entries = vec![e1.clone(), e2.clone()];
    assert_eq!(revision::filter_entries(&entries, "张", "", "").len(), 1);
    assert_eq!(revision::filter_entries(&entries, "", "明细", "加粗").len(), 1);
    let csv = revision::export_csv(&entries);
    assert!(csv.starts_with('\u{FEFF}'));
    assert!(csv.contains("张三") && csv.contains("远端协同") && csv.contains("加粗标题"));
}
