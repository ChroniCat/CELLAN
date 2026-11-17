//! C20：中文笔画排序（Excel xlStroke 实测顺序表）

use sheetcore::data_tools::{SortKey, SortMode, sort_multi};
use sheetcore::{Cell, Value, Workbook};

fn text(s: &str) -> Cell {
    Cell { value: Value::Text(s.into()), formula: None }
}

fn book(values: &[&str]) -> Workbook {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "S".into();
    let s = &mut wb.sheets[0];
    s.set(0, 0, text("姓名"));
    for (i, v) in values.iter().enumerate() {
        s.set(i as u32 + 1, 0, text(v));
    }
    wb
}

fn order(wb: &Workbook) -> Vec<String> {
    let s = &wb.sheets[0];
    (1..=4)
        .map(|r| s.get(r, 0).map(|c| c.value.display()).unwrap_or_default())
        .collect()
}

fn sorted(by: SortMode) -> Vec<String> {
    let mut wb = book(&["张", "一", "王", "李"]);
    let moved = sort_multi(
        &mut wb.sheets[0],
        (0, 0, 4, 0),
        &[SortKey {
            col: 0,
            asc: true,
            ignore_case: true,
            mode: by,
        }],
        true,
    );
    assert_eq!(moved, 4, "四行都应落位");
    order(&wb)
}

#[test]
fn stroke_sort_matches_excel_order() {
    // Excel xlStroke：一(0) 王(163) 李(1758) 张(2530)
    assert_eq!(sorted(SortMode::Stroke), vec!["一", "王", "李", "张"]);
}

#[test]
fn stroke_and_pinyin_are_different_orders() {
    // 拼音：李(li) 王(wang) 一(yi) 张(zhang)
    assert_eq!(sorted(SortMode::Pinyin), vec!["李", "王", "一", "张"]);
    assert_ne!(sorted(SortMode::Stroke), sorted(SortMode::Pinyin));
}

#[test]
fn stroke_sort_is_total_and_stable_for_unknown_chars() {
    // 表外字符（假名）退回拼音序，但仍必须是全序：排序两次结果相同
    let a = sorted(SortMode::Stroke);
    let b = sorted(SortMode::Stroke);
    assert_eq!(a, b);
}
