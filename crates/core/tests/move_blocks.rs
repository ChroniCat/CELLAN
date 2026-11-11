//! 拖动行号/列标移动整行整列（`Sheet::move_rows` / `move_cols`）。
use sheetcore::model::{Cell, CellRef, Sheet, Value};

fn sheet_with_rows() -> Sheet {
    let mut s = Sheet::new("T");
    for (i, txt) in ["a", "b", "c", "d"].iter().enumerate() {
        s.set(
            i as u32,
            0,
            Cell {
                value: Value::Text((*txt).into()),
                formula: None,
            },
        );
    }
    // 第 3 行（索引 2）高一点，带批注
    s.rows.set(2, 40.0);
    s.notes.insert(
        CellRef::new(2, 0),
        sheetcore::notes::Note {
            r: 2,
            c: 0,
            text: "note-c".into(),
            author: "me".into(),
        },
    );
    s
}

fn col_a(s: &Sheet) -> Vec<String> {
    (0..4)
        .map(|r| s.get(r, 0).map(|c| c.value.display()).unwrap_or_default())
        .collect()
}

#[test]
fn moves_row_block_down() {
    let mut s = sheet_with_rows();
    // 把第 1 行移到第 4 行下面：to = 4（删除后坐标）
    s.move_rows(0, 1, 4);
    assert_eq!(col_a(&s), vec!["b", "c", "d", "a"]);
}

#[test]
fn moves_row_block_up_keeps_height_and_note() {
    let mut s = sheet_with_rows();
    // 把第 3 行（带行高和批注）拖到最上面
    s.move_rows(2, 1, 0);
    assert_eq!(col_a(&s), vec!["c", "a", "b", "d"]);
    assert_eq!(s.rows.size(0), 40.0, "行高要跟着走");
    assert!(s.notes.contains_key(&CellRef::new(0, 0)), "批注要跟着走");
    assert!(!s.notes.contains_key(&CellRef::new(2, 0)));
}

#[test]
fn moves_two_rows_at_once() {
    let mut s = sheet_with_rows();
    s.move_rows(1, 2, 0);
    assert_eq!(col_a(&s), vec!["b", "c", "a", "d"]);
}

#[test]
fn moves_col_block() {
    let mut s = Sheet::new("T");
    for (i, txt) in ["x", "y", "z"].iter().enumerate() {
        s.set(
            0,
            i as u32,
            Cell {
                value: Value::Text((*txt).into()),
                formula: None,
            },
        );
    }
    s.cols.set(1, 150.0);
    s.move_cols(1, 1, 0);
    let got: Vec<String> = (0..3)
        .map(|c| s.get(0, c).map(|x| x.value.display()).unwrap_or_default())
        .collect();
    assert_eq!(got, vec!["y", "x", "z"]);
    assert_eq!(s.cols.size(0), 150.0, "列宽要跟着走");
}

#[test]
fn move_to_same_place_is_noop() {
    let mut s = sheet_with_rows();
    s.move_rows(1, 1, 1);
    assert_eq!(col_a(&s), vec!["a", "b", "c", "d"]);
}

#[test]
fn removed_cell_stays_empty_after_save() {
    use sheetcore::xlsx;
    let dir = std::env::temp_dir().join(format!("sn_rm_save_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let base = dir.join("base.xlsx");
    let out = dir.join("out.xlsx");
    let mut wb = sheetcore::Workbook::default();
    wb.sheets.push(sheetcore::model::Sheet::new("S"));
    wb.sheets[0].set(
        0,
        0,
        Cell {
            value: Value::Text("MOVE".into()),
            formula: None,
        },
    );
    xlsx::save_atomic(&base, &wb).unwrap();

    let mut wb2 = xlsx::load(&base).unwrap();
    let si = wb2.sheets.iter().position(|x| x.len() > 0).unwrap();
    wb2.sheets[si].remove(0, 0);
    wb2.path = Some(out.clone());
    xlsx::save_atomic(&out, &wb2).unwrap();

    let back = xlsx::load(&out).unwrap();
    assert!(
        back.sheets.iter().all(|x| x.get(0, 0).is_none()),
        "删掉的单元格不该在存盘后又冒出来"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
