//! C01 错误值模型：真正的错误必须是一种**类型**，不是一段文本。
//!
//! 旧实现里 `from_fvalue` 把 `FValue::Error` 转成 `Value::Text("#DIV/0!")`，
//! 于是「公式算出来的错误」和「用户手打的文本 #DIV/0!」在模型里完全一样：
//! 增量重算把它当文本缓存，IFERROR 抓不到，SUM 也不会传播。

use sheetcore::formula::{self, ErrKind};
use sheetcore::{Cell, Value, Workbook};

fn wb_with(pairs: &[(u32, u32, &str)]) -> Workbook {
    let mut wb = Workbook::new_blank();
    for (r, c, f) in pairs {
        let v = if let Some(body) = f.strip_prefix('=') {
            let fv = formula::eval_formula(&wb, 0, &format!("={body}"));
            formula::from_fvalue(&fv)
        } else {
            Value::parse_input(f)
        };
        wb.sheets[0].set(*r, *c, Cell {
            value: v,
            formula: f.starts_with('=').then(|| f.to_string().into()),
        });
    }
    wb
}

// ---------------------------------------------------------------- 类型

#[test]
fn a_computed_error_is_an_error_value_not_text() {
    let wb = wb_with(&[(0, 0, "=1/0")]);
    let v = wb.sheets[0].value(0, 0);
    assert_eq!(v, Value::Error(ErrKind::Div0), "1/0 必须是错误值，不是文本");
    assert_eq!(v.display(), "#DIV/0!");
}

#[test]
fn typed_text_that_looks_like_an_error_stays_text() {
    // 直接输入（不是公式）的 `#DIV/0!` 仍是文本——不能靠「以 # 开头」判错
    let wb = wb_with(&[(0, 0, "#DIV/0!"), (0, 1, "'#N/A")]);
    assert!(matches!(wb.sheets[0].value(0, 0), Value::Text(_)), "手打 #DIV/0! 应是文本");
    assert!(matches!(wb.sheets[0].value(0, 1), Value::Text(_)), "前置单引号强制文本");
    // 文本不会被 IFERROR 当成错误吞掉
    let got = formula::eval_formula(&wb, 0, "=IFERROR(A1,\"兜底\")");
    assert_eq!(got.display(), "#DIV/0!", "文本 #DIV/0! 被 IFERROR 当成错误了");
    // 而真正的错误值会被抓
    let wb2 = wb_with(&[(0, 0, "=1/0")]);
    assert_eq!(formula::eval_formula(&wb2, 0, "=IFERROR(A1,\"兜底\")").display(), "兜底");
}

#[test]
fn all_traditional_errors_keep_their_kind() {
    let cases = [
        ("=1/0", ErrKind::Div0, "#DIV/0!"),
        ("=1+", ErrKind::Value, "#VALUE!"),
        ("=NOPE()", ErrKind::Name, "#NAME?"),
        ("=SQRT(-1)", ErrKind::Num, "#NUM!"),
        ("=NA()", ErrKind::Na, "#N/A"),
        ("=#NULL!", ErrKind::Null, "#NULL!"),
        ("=#REF!", ErrKind::Ref, "#REF!"),
    ];
    for (f, kind, text) in cases {
        let wb = wb_with(&[(0, 0, f)]);
        let v = wb.sheets[0].value(0, 0);
        assert_eq!(v, Value::Error(kind), "{f} 的类型不对");
        assert_eq!(v.display(), text, "{f} 的显示文本不对");
    }
}

// ---------------------------------------------------------------- 传播

#[test]
fn iferror_catches_a_real_error() {
    let wb = wb_with(&[(0, 0, "=1/0")]);
    let got = formula::eval_formula(&wb, 0, "=IFERROR(A1,\"兜底\")");
    assert_eq!(got.display(), "兜底", "IFERROR 必须抓到真正的错误值");
}

#[test]
fn sum_propagates_a_real_error_instead_of_skipping_it() {
    let wb = wb_with(&[(0, 0, "=1/0"), (1, 0, "=1"), (2, 0, "=2")]);
    let got = formula::eval_formula(&wb, 0, "=SUM(A1:A3)");
    assert_eq!(got.display(), "#DIV/0!", "SUM 不能把真错误当文本跳过");
}

#[test]
fn a_reference_to_the_same_sheet_keeps_the_error_type() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "数据".into();
    wb.sheets[0].set(0, 0, Cell { value: Value::Error(ErrKind::Div0), formula: None });
    let got = formula::eval_formula(&wb, 0, "=数据!A1&\"\"");
    assert_eq!(got.display(), "#DIV/0!");
}

// ---------------------------------------------------------------- 增量重算

#[test]
fn full_recalc_keeps_the_error_type() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(0, 0, Cell { value: Value::Number(1.0), formula: None });
    wb.sheets[0].set(0, 1, Cell { value: Value::Number(0.0), formula: None });
    // C1 = A1/B1；B1 是 0 → 错误
    let fv = formula::eval_formula(&wb, 0, "=A1/B1");
    wb.sheets[0].set(0, 2, Cell { value: formula::from_fvalue(&fv), formula: Some("=A1/B1".into()) });
    assert_eq!(wb.sheets[0].value(0, 2), Value::Error(ErrKind::Div0));

    // 改动无关的依赖项：错误类型必须保住，不能被降级成文本缓存
    let before = wb.sheets[0].value(0, 2).clone();
    wb.sheets[0].set(5, 5, Cell { value: Value::Number(9.0), formula: None });
    formula::recalc_all(&mut wb);
    assert_eq!(before, Value::Error(ErrKind::Div0));
    assert_eq!(wb.sheets[0].value(0, 2), Value::Error(ErrKind::Div0), "全量重算后错误类型丢了");
}

// ---------------------------------------------------------------- 存盘往返

#[test]
fn save_and_reload_keep_the_error_type() {
    let dir = std::env::temp_dir().join(format!("c01-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("errors.xlsx");
    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(0, 0, Cell { value: Value::Error(ErrKind::Div0), formula: Some("=1/0".into()) });
    wb.sheets[0].set(1, 0, Cell { value: Value::Error(ErrKind::Na), formula: None });
    wb.sheets[0].set(2, 0, Cell { value: Value::Text("#DIV/0!".into()), formula: None });
    sheetcore::xlsx::save_atomic(&path, &wb).expect("保存失败");

    let back = sheetcore::xlsx::load(&path).expect("载入失败");
    assert_eq!(back.sheets[0].value(0, 0), Value::Error(ErrKind::Div0), "公式错误存盘后变成了文本");
    assert_eq!(back.sheets[0].value(1, 0), Value::Error(ErrKind::Na), "静态错误值存盘后变成了文本");
    assert_eq!(
        back.sheets[0].value(2, 0),
        Value::Text("#DIV/0!".into()),
        "文本 #DIV/0! 存盘后不能变成错误值"
    );
}
