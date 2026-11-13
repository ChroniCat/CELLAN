//! C11.1：整行引用 `1:3` 的解析、求值和规范化边界。

use sheetcore::formula::{eval_formula, eval_formula_reads, FValue};
use sheetcore::normalize_formula;
use sheetcore::model::{Cell, Sheet, Value, Workbook};

fn num(v: FValue) -> f64 {
    match v {
        FValue::Number(n) => n,
        other => panic!("expected number, got {other:?}"),
    }
}

fn book() -> Workbook {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Data".into();
    for (r, c, n) in [(0u32, 0u32, 1.0), (1, 1, 2.0), (2, 2, 3.0), (3, 3, 4.0)] {
        wb.sheets[0].set(r, c, Cell { value: Value::Number(n), formula: None });
    }
    wb
}

#[test]
fn whole_row_reference_is_preserved_for_copy_and_edit_semantics() {
    // C11.1 复核：模型和 xlsx 必须保留 1:3；全宽 A1:XFD3 会让复制/插列
    // 被当成普通区域平移，丢失整行语义。
    assert_eq!(normalize_formula("=SUM(1:3)"), "=SUM(1:3)");
    assert_eq!(normalize_formula("=SUM($1:$3)"), "=SUM($1:$3)");
    assert_eq!(normalize_formula(r#"="1:3""#), r#"="1:3""#);
    assert_eq!(normalize_formula("=SUM(A1:B2)"), "=SUM(A1:B2)");
}

#[test]
fn whole_row_sum_uses_used_range_and_is_sheet_scoped() {
    let mut wb = book();
    assert_eq!(num(eval_formula(&wb, 0, "=SUM(1:3)")), 6.0);
    assert_eq!(num(eval_formula(&wb, 0, "=SUM(1:1)")), 1.0);
    assert_eq!(num(eval_formula(&wb, 0, "=SUM(2:2)")), 2.0);
    assert_eq!(num(eval_formula(&wb, 0, "=SUM($2:$2)")), 2.0);

    let mut other = Sheet::new("Other");
    other.set(0, 0, Cell { value: Value::Number(10.0), formula: None });
    other.set(1, 0, Cell { value: Value::Number(20.0), formula: None });
    wb.sheets.push(other);
    assert_eq!(num(eval_formula(&wb, 0, "=SUM(Other!1:2)")), 30.0);
    assert_eq!(num(eval_formula(&wb, 0, "=SUM(Other!$2:$2)")), 20.0);
}

#[test]
fn whole_row_inside_structured_reference_is_not_rewritten() {
    // 方括号里是列名，不是行区间；解析器不能把它拆成 A1:XFD。
    let out = normalize_formula("=SUM(Sales[1:3])");
    assert_eq!(out, "=SUM(Sales[1:3])");
}

#[test]
fn whole_row_is_a_dependency_and_moves_on_row_insert() {
    let mut wb = book();
    wb.sheets[0].set(
        3,
        0,
        Cell { value: Value::Empty, formula: Some("=SUM(1:3)".into()) },
    );
    let deps = sheetcore::formula::deps::Deps::build(&wb);
    let dirty = deps.closure(&[(0, 1, 1)]);
    assert!(dirty.contains(&(0, 3, 0)), "A4 formula must depend on row 2");

    let mut shift = sheetcore::refs::Shift { axis: sheetcore::refs::Axis::Row, at: 0, n: 1, ..Default::default() };
    shift.target_sheet = "Data".into();
    shift.own_sheet = "Data".into();
    let out = sheetcore::refs::rewrite("=SUM(1:3)", &shift);
    assert_eq!(out, "=SUM(2:4)");
}

#[test]
fn whole_row_copy_and_cf_shift_keep_whole_row_semantics() {
    assert_eq!(sheetcore::cf::shift_formula("=SUM(1:1)", 1, 0), "=SUM(2:2)");
    assert_eq!(sheetcore::cf::shift_formula("=SUM(1:1)", 0, 1), "=SUM(1:1)");
    assert_eq!(sheetcore::cf::shift_formula("=SUM($1:$1)", 1, 0), "=SUM($1:$1)");
    let mut shift = sheetcore::refs::Shift {
        axis: sheetcore::refs::Axis::Row,
        at: 0,
        n: 1,
        translate: Some((1, 0)),
        target_sheet: "Data".into(),
        own_sheet: "Data".into(),
        ..Default::default()
    };
    assert_eq!(
        sheetcore::refs::rewrite("=SUM(1:1)", &shift),
        "=SUM(2:2)"
    );
    shift.translate = Some((0, 1));
    assert_eq!(
        sheetcore::refs::rewrite("=SUM(1:1)", &shift),
        "=SUM(1:1)",
        "横向复制不能把整行改成全宽 A1 区域"
    );
}

#[test]
fn whole_row_sum_reads_only_used_range_not_all_16384_columns() {
    let wb = book();
    let (v, reads) = eval_formula_reads(&wb, 0, "=SUM(1:3)");
    assert_eq!(num(v), 6.0);
    assert!(reads > 0, "必须真的读取区域");
    assert!(
        reads <= 64,
        "整行按实际使用区裁剪，实际读取 {reads} 格，不能逐格扫 16384 列"
    );
    // 直接写全宽区域也必须命中 cached_grid/range_values 的整行裁剪，
    // 而不是因为 3×16384 面积小于 200000 就逐格访问。
    let (v2, reads2) = eval_formula_reads(&wb, 0, "=SUM(A1:XFD3)");
    assert_eq!(num(v2), 6.0);
    assert!(reads2 <= 64, "全宽区域实际读取 {reads2} 格");
}

#[test]
fn whole_row_formula_survives_actual_save_and_reload() {
    let mut wb = book();
    wb.sheets[0].set(
        4,
        1,
        Cell {
            value: Value::Empty,
            formula: Some("=SUM(3:4)".into()),
        },
    );
    let dir = std::env::temp_dir().join("sheets_native_c11_rows");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("whole_row_{}.xlsx", std::process::id()));
    let _ = std::fs::remove_file(&path);
    sheetcore::xlsx::save(&path, &wb).expect("save");
    let back = sheetcore::xlsx::load(&path).expect("load");
    assert_eq!(
        back.sheets[0].get(4, 1).and_then(|c| c.formula.as_deref()),
        Some("=SUM(3:4)"),
        "xlsx 加载不能把整行公式改写成 A1:XFD"
    );
    // 实际加载后的整行引用再走复制/插列，检查左右不变、上下按行平移、
    // 行绝对引用不动，且不会变成 A1:XFD1 / #REF!.
    let raw = back.sheets[0]
        .get(4, 1)
        .and_then(|c| c.formula.as_deref())
        .expect("loaded formula");
    assert_eq!(sheetcore::cf::shift_formula(raw, 0, 1), raw);
    assert_eq!(sheetcore::cf::shift_formula(raw, 1, 0), "=SUM(4:5)");
    assert_eq!(sheetcore::cf::shift_formula("=SUM($3:$4)", 1, 0), "=SUM($3:$4)");
    assert_eq!(
        sheetcore::cf::shift_formula("=SUM(Sales[1:3])", 1, 0),
        "=SUM(Sales[1:3])",
        "结构化引用方括号里的 1:3 是列名，不能被整行分支改写"
    );
    let mut shift = sheetcore::refs::Shift {
        axis: sheetcore::refs::Axis::Row,
        at: 0,
        n: 1,
        translate: Some((1, 0)),
        target_sheet: "Data".into(),
        own_sheet: "Data".into(),
        ..Default::default()
    };
    assert_eq!(sheetcore::refs::rewrite(raw, &shift), "=SUM(4:5)");
    shift.translate = Some((-1, 0));
    assert_eq!(sheetcore::refs::rewrite(raw, &shift), "=SUM(2:3)");
    shift.translate = Some((0, 1));
    assert_eq!(sheetcore::refs::rewrite(raw, &shift), raw);
    shift.translate = None;
    shift.axis = sheetcore::refs::Axis::Col;
    shift.n = 1;
    assert_eq!(
        sheetcore::refs::rewrite(raw, &shift),
        raw,
        "插入列不能把整行引用改成全宽区域"
    );
    let _ = std::fs::remove_file(&path);
}
