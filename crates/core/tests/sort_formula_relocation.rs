use sheetcore::{Cell, Value, Workbook, formula::{self, deps::Deps}, xlsx};
use sheetcore::data_tools::{self, SortKey};
use sheetcore::sort_filter;
use std::path::PathBuf;

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let seq = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("native_sort_formula_{}_{seq}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    fn path(&self) -> PathBuf { self.0.join("sort.xlsx") }
}
impl Drop for Fixture {
    fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); }
}

fn number(n: f64) -> Cell { Cell { value: Value::Number(n), formula: None } }
fn text(s: &str) -> Cell { Cell { value: Value::Text(s.into()), formula: None } }
fn set_formula(wb: &mut Workbook, r: u32, c: u32, f: &str) {
    wb.sheets[0].set(r, c, Cell { value: Value::Empty, formula: Some(f.into()) });
}
fn formula_at(wb: &Workbook, r: u32, c: u32) -> &str {
    wb.sheets[0].get(r, c).unwrap().formula.as_deref().unwrap()
}

#[test]
fn quick_and_multikey_sorts_move_relative_refs_and_preserve_absolute_refs_on_save() {
    for multi in [false, true] {
        let fixture = Fixture::new();
        let path = fixture.path();
        let mut wb = Workbook::new_blank();
        for (c, label) in ["key", "amount", "mixed", "record"].iter().enumerate() {
            wb.sheets[0].set(0, c as u32, text(label));
        }
        wb.sheets[0].set(0, 4, number(10.0));
        for (r, key, label) in [(1, 2.0, "second"), (2, 1.0, "first")] {
            wb.sheets[0].set(r, 0, number(key));
            wb.sheets[0].set(r, 3, text(label));
            set_formula(&mut wb, r, 1, &format!("=A{}*10+$E$1", r + 1));
            set_formula(&mut wb, r, 2, &format!("=$A{}*2+E$1", r + 1));
        }
        let bold = wb.styles.intern(sheetcore::style::CellStyle { bold: true, ..Default::default() });
        wb.sheets[0].set_style(2, 1, bold);
        formula::recalc_all(&mut wb);
        xlsx::save_atomic(&path, &wb).unwrap();
        let mut wb = xlsx::load(&path).unwrap();
        if multi {
            data_tools::sort_multi(&mut wb.sheets[0], (0, 0, 2, 3), &[
                SortKey { col: 0, asc: true, ignore_case: true },
                SortKey { col: 3, asc: true, ignore_case: true },
            ], true);
        } else {
            sort_filter::sort_region(&mut wb.sheets[0], (0, 0, 2, 3), 0, true).unwrap();
        }
        assert_eq!(formula_at(&wb, 1, 1), "=A2*10+$E$1");
        assert_eq!(formula_at(&wb, 2, 1), "=A3*10+$E$1");
        assert_eq!(formula_at(&wb, 1, 2), "=$A2*2+E$1");
        assert_eq!(formula_at(&wb, 2, 2), "=$A3*2+E$1");
        // 与 app 的排序完成处理一致：重建依赖图并重算。
        let deps = Deps::build(&wb);
        formula::recalc_all(&mut wb);
        assert_eq!(wb.sheets[0].value(1, 1), Value::Number(20.0));
        assert_eq!(wb.sheets[0].value(2, 1), Value::Number(30.0));
        wb.sheets[0].set(1, 0, number(3.0));
        formula::recalc_dirty(&mut wb, &deps, &[(0, 1, 0)]);
        assert_eq!(wb.sheets[0].value(1, 1), Value::Number(40.0));
        assert_eq!(wb.sheets[0].value(1, 2), Value::Number(16.0));
        assert_eq!(wb.sheets[0].value(2, 1), Value::Number(30.0));
        for _ in 0..2 {
            xlsx::save_atomic(&path, &wb).unwrap();
            let mut back = xlsx::load(&path).unwrap();
            formula::recalc_all(&mut back);
            assert_eq!(back.sheets[0].value(1, 1), Value::Number(40.0));
            assert_eq!(back.sheets[0].value(2, 1), Value::Number(30.0));
            assert_eq!(formula_at(&back, 1, 2), "=$A2*2+E$1");
            assert_eq!(back.sheets[0].value(1, 3), Value::Text("first".into()));
            assert!(back.styles.get(back.sheets[0].style_of(1, 1)).bold);
            assert!(!back.styles.get(back.sheets[0].style_of(2, 1)).bold);
        }
    }
}

#[test]
fn multikey_tie_break_moves_each_formula_by_its_actual_source_row() {
    let mut wb = Workbook::new_blank();
    for (r, primary, secondary) in [(0, 2.0, 3.0), (1, 1.0, 2.0), (2, 1.0, 1.0)] {
        wb.sheets[0].set(r, 0, number(primary));
        wb.sheets[0].set(r, 1, number(secondary));
        set_formula(&mut wb, r, 2, &format!("=A{}*10+B{}", r + 1, r + 1));
    }
    formula::recalc_all(&mut wb);
    let moved = data_tools::sort_multi(&mut wb.sheets[0], (0, 0, 2, 2), &[
        SortKey { col: 0, asc: true, ignore_case: true },
        SortKey { col: 1, asc: true, ignore_case: true },
    ], false);
    assert_eq!(moved, 2);
    formula::recalc_all(&mut wb);
    for (r, expected) in [(0, 11.0), (1, 12.0), (2, 23.0)] {
        assert_eq!(wb.sheets[0].value(r, 2), Value::Number(expected));
        assert_eq!(formula_at(&wb, r, 2), format!("=A{}*10+B{}", r + 1, r + 1));
    }
}

#[test]
fn sorting_moves_sheet_qualified_relative_rows_but_keeps_absolute_rows() {
    let mut wb = Workbook::new_blank();
    wb.sheets.push(sheetcore::Sheet::new("Source Data"));
    for (r, key) in [(0, 2.0), (1, 1.0)] {
        wb.sheets[0].set(r, 0, number(key));
        wb.sheets[1].set(r, 0, number((r + 1) as f64));
        set_formula(&mut wb, r, 1, &format!("='Source Data'!A{}+'Source Data'!$A$1", r + 1));
    }
    formula::recalc_all(&mut wb);
    sort_filter::sort_range(&mut wb.sheets[0], 0, 0, 1, 1, 0, true).unwrap();
    formula::recalc_all(&mut wb);
    assert_eq!(formula_at(&wb, 0, 1), "='Source Data'!A1+'Source Data'!$A$1");
    assert_eq!(formula_at(&wb, 1, 1), "='Source Data'!A2+'Source Data'!$A$1");
    assert_eq!(wb.sheets[0].value(0, 1), Value::Number(2.0));
    assert_eq!(wb.sheets[0].value(1, 1), Value::Number(3.0));
}

#[test]
fn both_sort_paths_leave_array_anchors_and_spill_ownership_intact() {
    for cse in [false, true] {
        let mut wb = Workbook::new_blank();
        wb.sheets[0].set(0, 0, number(2.0));
        wb.sheets[0].set(1, 0, number(1.0));
        set_formula(&mut wb, 1, 2, "=SEQUENCE(2)");
        if cse { wb.sheets[0].arrays.push((sheetcore::CellRef::new(1, 2), (1, 2, 2, 2))); }
        formula::recalc_all(&mut wb);
        let spilled = wb.sheets[0].spilled.clone();
        // 只选中锚点而不含尾格，也不能搬走数组公式。
        assert!(sort_filter::sort_range(&mut wb.sheets[0], 0, 0, 1, 2, 0, true).is_err());
        assert_eq!(data_tools::sort_multi(&mut wb.sheets[0], (0, 0, 1, 2), &[
            SortKey { col: 0, asc: true, ignore_case: true },
        ], false), 0);
        assert_eq!(wb.sheets[0].value(0, 0), Value::Number(2.0));
        assert_eq!(wb.sheets[0].value(1, 0), Value::Number(1.0));
        assert_eq!(formula_at(&wb, 1, 2), "=SEQUENCE(2)");
        assert_eq!(wb.sheets[0].spilled, spilled);
        // 同一张表上不接触数组的选区仍可正常排序。
        sort_filter::sort_range(&mut wb.sheets[0], 0, 0, 1, 0, 0, true).unwrap();
        assert_eq!(wb.sheets[0].value(0, 0), Value::Number(1.0));
        assert_eq!(wb.sheets[0].spilled, spilled);
    }
}
