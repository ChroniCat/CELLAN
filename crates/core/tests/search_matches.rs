//! UI-05 统一匹配器：稀疏遍历、远端格子、循环下一项、整格/公式模式。

use sheetcore::model::{Cell, Value, Workbook};
use sheetcore::search::{self, Hit, SearchOpts};

fn text(s: &str) -> Cell {
    Cell {
        value: Value::Text(s.into()),
        formula: None,
    }
}

fn opts(whole: bool, formula: bool) -> SearchOpts {
    SearchOpts {
        match_case: false,
        whole_cell: whole,
        in_formula: formula,
    }
}

#[test]
fn sparse_search_reaches_a300_and_the_last_cell_then_wraps() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(299, 0, text("abc")); // A300
    wb.sheets[0].set(1_048_575, 16_383, text("ABC")); // XFD1048576
    let n = search::normalize_needle("abc", &opts(false, false));
    let hits = search::hits(&wb, Some(0), &n, &opts(false, false));
    assert_eq!(
        hits,
        vec![
            Hit { sheet: 0, row: 299, col: 0 },
            Hit { sheet: 0, row: 1_048_575, col: 16_383 },
        ],
        "两张远端格都要命中且按位置排序"
    );
    let (h1, wrapped) = search::next_hit(&hits, Hit { sheet: 0, row: 0, col: 0 }, true).unwrap();
    assert_eq!((h1.row, h1.col, wrapped), (299, 0, false));
    let (h2, wrapped) = search::next_hit(&hits, h1, true).unwrap();
    assert_eq!((h2.row, h2.col, wrapped), (1_048_575, 16_383, false));
    let (h3, wrapped) = search::next_hit(&hits, h2, true).unwrap();
    assert_eq!((h3.row, h3.col, wrapped), (299, 0, true), "一轮后回到第一处并标记回绕");
}

#[test]
fn whole_cell_does_not_match_a_longer_text() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(0, 0, text("abc"));
    wb.sheets[0].set(0, 1, text("xabc"));
    let n = search::normalize_needle("abc", &opts(true, false));
    let hits = search::hits(&wb, Some(0), &n, &opts(true, false));
    assert_eq!(hits.len(), 1);
    assert_eq!((hits[0].row, hits[0].col), (0, 0));
}

#[test]
fn formula_mode_matches_formula_text_value_mode_matches_display() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(
        0,
        2,
        Cell {
            value: Value::Number(3.0),
            formula: Some("=SUM(A1:A2)".into()),
        },
    );
    let f = opts(false, true);
    let n = search::normalize_needle("A1", &f);
    assert_eq!(search::hits(&wb, Some(0), &n, &f).len(), 1, "公式模式查公式文本");
    let v = opts(false, false);
    let nv = search::normalize_needle("3", &v);
    assert_eq!(search::hits(&wb, Some(0), &nv, &v).len(), 1, "值模式查公式结果");
    let nf = search::normalize_needle("SUM", &v);
    assert!(search::hits(&wb, Some(0), &nf, &v).is_empty(), "值模式不看公式文本");
}

#[test]
fn replace_in_text_keeps_surrounding_text_and_case() {
    assert_eq!(
        search::replace_in_text("Abc-abc-ABC", "abc", "X", false),
        "X-X-X"
    );
    assert_eq!(
        search::replace_in_text("Abc-abc", "abc", "X", true),
        "Abc-X"
    );
}

/// 值模式查的是网格显示串：百分比/千分位按屏幕文本命中。
#[test]
fn value_mode_matches_formatted_display_text() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(0, 0, Cell { value: Value::Number(0.5), formula: None });
    let mut st = wb.styles.get(0).clone();
    st.format = "0.00%".into();
    let id = wb.styles.intern(st);
    wb.sheets[0].set_style(0, 0, id);
    let o = opts(false, false);
    let n = search::normalize_needle("50.00%", &o);
    assert_eq!(search::hits(&wb, Some(0), &n, &o).len(), 1, "显示串 50.00% 应能查到");
    let raw = search::normalize_needle("0.5", &o);
    assert!(search::hits(&wb, Some(0), &raw, &o).is_empty(), "屏幕上看不到 0.5 就不该命中");
}

/// Unicode 小写化改变字节长度时，替换不能按小写索引切原串（İabc 查 abc）。
#[test]
fn case_insensitive_replace_handles_unicode_length_change() {
    assert_eq!(search::replace_in_text("İabc", "abc", "X", false), "İX");
    assert_eq!(search::replace_in_text("abcabc", "abc", "X", false), "XX");
    // 中文/多字节文本里做不区分大小写替换也不能切坏 UTF-8
    assert_eq!(search::replace_in_text("表abc格", "abc", "值", false), "表值格");
}
