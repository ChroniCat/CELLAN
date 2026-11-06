//! 第二批函数对拍（期望值为 Excel 365 计算结果）

use sheetcore::formula::{self, FValue};
use sheetcore::{Cell, Value, Workbook};

fn book() -> Workbook {
    Workbook::new_blank()
}

fn ev(f: &str) -> FValue {
    let wb = book();
    formula::eval_formula(&wb, 0, f)
}

fn close(f: &str, expect: f64, tol: f64) {
    match ev(f) {
        FValue::Number(n) => assert!((n - expect).abs() <= tol, "{f} = {n}, 期望 {expect}"),
        other => panic!("{f} 返回 {other:?}"),
    }
}

fn eq(f: &str, expect: &str) {
    let got = ev(f).display();
    assert_eq!(got, expect, "{f}");
}

#[test]
fn normal_distribution() {
    close("=NORM.S.DIST(1.96,TRUE)", 0.975002105, 1e-6);
    close("=NORM.S.DIST(0,TRUE)", 0.5, 1e-9);
    close("=NORM.S.DIST(0,FALSE)", 0.3989422804, 1e-9);
    close("=NORM.INV(0.975,0,1)", 1.959963985, 1e-6);
    close("=NORM.DIST(50,40,10,TRUE)", 0.841344746, 1e-6);
    close("=NORM.S.INV(0.5)", 0.0, 1e-9);
    close("=STANDARDIZE(50,40,10)", 1.0, 1e-9);
}

#[test]
fn t_chisq_f_distributions() {
    close("=T.INV.2T(0.05,19)", 2.093024054, 1e-5);
    close("=T.DIST.2T(2.093024054,19)", 0.05, 1e-5);
    close("=CHISQ.INV.RT(0.05,1)", 3.841458820, 1e-5);
    close("=CHISQ.DIST.RT(3.841458820,1)", 0.05, 1e-5);
    close("=F.INV.RT(0.05,5,10)", 3.325834530, 1e-5);
    close("=F.DIST.RT(3.325834530,5,10)", 0.05, 1e-5);
}

#[test]
fn discrete_distributions() {
    close("=BINOM.DIST(3,10,0.5,FALSE)", 0.1171875, 1e-9);
    close("=BINOM.DIST(5,10,0.5,TRUE)", 0.623046875, 1e-9);
    close("=POISSON.DIST(2,3,FALSE)", 0.224041808, 1e-6);
    close("=POISSON.DIST(2,3,TRUE)", 0.423190081, 1e-6);
    close("=EXPON.DIST(1,1,TRUE)", 0.632120559, 1e-6);
    close("=WEIBULL.DIST(1,1,1,TRUE)", 0.632120559, 1e-6);
}

#[test]
fn gamma_beta_and_conf() {
    close("=GAMMALN(5)", 3.178053830, 1e-6);
    close("=BETA.DIST(0.5,2,3,TRUE)", 0.6875, 1e-9);
    close("=BETA.INV(0.6875,2,3)", 0.5, 1e-6);
    close("=CONFIDENCE.NORM(0.05,1,100)", 0.195996402, 1e-6);
    close("=FISHER(0.5)", 0.549306144, 1e-6);
    close("=FISHERINV(0.549306144)", 0.5, 1e-6);
}

#[test]
fn engineering() {
    eq("=BIN2DEC(\"1010\")", "10");
    eq("=BIN2DEC(\"1111111111\")", "-1");
    eq("=DEC2HEX(255)", "FF");
    eq("=DEC2BIN(5)", "101");
    eq("=HEX2DEC(\"FF\")", "255");
    eq("=OCT2DEC(\"17\")", "15");
    eq("=BITAND(13,25)", "9");
    eq("=BITOR(13,25)", "29");
    eq("=BITXOR(13,25)", "20");
    close("=ERF(1)", 0.842700793, 1e-6);
    close("=IMABS(\"3+4i\")", 5.0, 1e-9);
    eq("=IMREAL(\"3+4i\")", "3");
    eq("=IMAGINARY(\"3+4i\")", "4");
    eq("=IMSUM(\"1+2i\",\"3+4i\")", "4+6i");
    eq("=ROMAN(2026)", "MMXXVI");
    eq("=ARABIC(\"MMXXVI\")", "2026");
}

#[test]
fn financial_advanced() {
    close("=EFFECT(0.05,12)", 0.051161898, 1e-8);
    close("=NOMINAL(0.051161898,12)", 0.05, 1e-8);
    close("=DDB(1000,100,5,1)", 400.0, 1e-6);
    close("=RRI(10,1000,2000)", 0.071773463, 1e-8);
    close("=PDURATION(0.05,1000,2000)", 14.20669908, 1e-6);
    close("=DOLLARDE(1.02,16)", 1.125, 1e-9);
    close("=RATE(360,-5368.22,1000000)", 0.004166667, 1e-6);
    // IRR：投入 1000，收回 500/500 → 0
    let mut wb = Workbook::new_blank();
    for (i, v) in [-1000.0, 500.0, 500.0].iter().enumerate() {
        wb.sheets[0].set(i as u32, 0, Cell { value: Value::Number(*v), formula: None });
    }
    match formula::eval_formula(&wb, 0, "=IRR(A1:A3)") {
        FValue::Number(n) => assert!(n.abs() < 1e-4, "IRR = {n}"),
        other => panic!("IRR 返回 {other:?}"),
    }
}

#[test]
fn database_functions() {
    use sheetcore::Sheet;
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "DB".into();
    let s: &mut Sheet = &mut wb.sheets[0];
    // 数据区 A1:C4
    for (r, c, v) in [
        (0u32, 0u32, "树种"),
        (1, 0, "苹果"),
        (2, 0, "梨"),
        (3, 0, "苹果"),
    ] {
        s.set(r, c, Cell { value: Value::Text(v.into()), formula: None });
    }
    s.set(0, 1, Cell { value: Value::Text("高度".into()), formula: None });
    s.set(0, 2, Cell { value: Value::Text("产量".into()), formula: None });
    for (r, h, y) in [(1u32, 18.0, 20.0), (2, 12.0, 10.0), (3, 14.0, 15.0)] {
        s.set(r, 1, Cell { value: Value::Number(h), formula: None });
        s.set(r, 2, Cell { value: Value::Number(y), formula: None });
    }
    // 条件区 E1:E2 → 树种=苹果
    s.set(0, 4, Cell { value: Value::Text("树种".into()), formula: None });
    s.set(1, 4, Cell { value: Value::Text("苹果".into()), formula: None });

    let cases = [
        ("=DSUM(A1:C4,\"产量\",E1:E2)", 35.0),
        ("=DAVERAGE(A1:C4,\"高度\",E1:E2)", 16.0),
        ("=DCOUNT(A1:C4,\"产量\",E1:E2)", 2.0),
        ("=DMAX(A1:C4,2,E1:E2)", 18.0),
        ("=DMIN(A1:C4,\"高度\",E1:E2)", 14.0),
    ];
    for (f, expect) in cases {
        match formula::eval_formula(&wb, 0, f) {
            FValue::Number(n) => assert!((n - expect).abs() < 1e-9, "{f} = {n}, 期望 {expect}"),
            other => panic!("{f} 返回 {other:?}"),
        }
    }
}

#[test]
fn misc_math_and_text() {
    eq("=TEXTBEFORE(\"a-b\",\"-\")", "a");
    eq("=TEXTAFTER(\"a-b\",\"-\")", "b");
    eq("=UNICHAR(20013)", "中");
    close("=COMBINA(4,3)", 20.0, 1e-9);
    close("=PERMUT(5,3)", 60.0, 1e-9);
    close("=MULTINOMIAL(2,3,4)", 1260.0, 1e-6);
    let mut wb = Workbook::new_blank();
    for (i, (a, b)) in [(1.0, 2.0), (2.0, 4.0), (3.0, 6.0)].iter().enumerate() {
        wb.sheets[0].set(i as u32, 0, Cell { value: Value::Number(*a), formula: None });
        wb.sheets[0].set(i as u32, 1, Cell { value: Value::Number(*b), formula: None });
    }
    for (i, v) in [1.0, 2.0, 3.0, 4.0, 100.0].iter().enumerate() {
        wb.sheets[0].set(i as u32, 3, Cell { value: Value::Number(*v), formula: None });
    }
    match formula::eval_formula(&wb, 0, "=SUMXMY2(A1:A3,B1:B3)") {
        FValue::Number(n) => assert!((n - 14.0).abs() < 1e-9, "SUMXMY2 = {n}"),
        other => panic!("SUMXMY2 返回 {other:?}"),
    }
    match formula::eval_formula(&wb, 0, "=TRIMMEAN(D1:D5,0.4)") {
        FValue::Number(n) => assert!((n - 3.0).abs() < 1e-9, "TRIMMEAN = {n}"),
        other => panic!("TRIMMEAN 返回 {other:?}"),
    }
}
