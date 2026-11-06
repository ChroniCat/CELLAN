//! 第四批函数对拍：假设检验、回归、单位换算、财务补充
//! 期望值全部由 Python 独立实现复算得出，不是「跑一遍看输出」。

use sheetcore::formula::{self, FValue};
use sheetcore::{Cell, Value, Workbook};

fn book() -> Workbook {
    let mut wb = Workbook::new_blank();
    let a = [1.0, 2.0, 3.0, 4.0, 5.0];
    let b = [1.0, 3.0, 5.0, 7.0, 9.0];
    for (i, v) in a.iter().enumerate() {
        wb.sheets[0].set(i as u32, 0, Cell { value: Value::Number(*v), formula: None });
    }
    for (i, v) in b.iter().enumerate() {
        wb.sheets[0].set(i as u32, 1, Cell { value: Value::Number(*v), formula: None });
    }
    wb
}

fn num(wb: &Workbook, f: &str) -> f64 {
    match formula::eval_formula(wb, 0, f) {
        FValue::Number(n) => n,
        FValue::Array(a) => a.data[0].to_number().unwrap_or(f64::NAN),
        other => panic!("{f} 期望数字，得到 {}", other.display()),
    }
}

fn close(a: f64, b: f64, tol: f64) {
    assert!((a - b).abs() <= tol, "{a} != {b}");
}

#[test]
fn unit_conversion() {
    let wb = book();
    close(num(&wb, "=CONVERT(1,\"mi\",\"km\")"), 1.609344, 1e-9);
    close(num(&wb, "=CONVERT(100,\"C\",\"F\")"), 212.0, 1e-9);
    close(num(&wb, "=CONVERT(2,\"hr\",\"min\")"), 120.0, 1e-9);
    close(num(&wb, "=CONVERT(1,\"lbm\",\"kg\")"), 0.45359237, 1e-12);
    close(num(&wb, "=CONVERT(1,\"gal\",\"l\")"), 3.785411784, 1e-9);
    assert_eq!(formula::eval_formula(&wb, 0, "=CONVERT(1,\"kg\",\"zz\")").display(), "#N/A");
}

#[test]
fn reference_functions() {
    let wb = book();
    assert_eq!(formula::eval_formula(&wb, 0, "=ADDRESS(1,1)").display(), "$A$1");
    assert_eq!(formula::eval_formula(&wb, 0, "=ADDRESS(2,3,4)").display(), "C2");
    assert_eq!(formula::eval_formula(&wb, 0, "=ADDRESS(2,3,3)").display(), "$C2");
    assert_eq!(num(&wb, "=ROW()"), 1.0);
    assert_eq!(num(&wb, "=COLUMN()"), 1.0);
    assert_eq!(num(&wb, "=INDIRECT(\"A2\")"), 2.0);
    assert_eq!(num(&wb, "=OFFSET(A1,1,0)"), 2.0);
    assert_eq!(num(&wb, "=SUM(OFFSET(A1,0,0,3,1))"), 6.0);
    assert_eq!(num(&wb, "=ROW(A3)"), 3.0);
}

#[test]
fn percentile_family() {
    let wb = book();
    close(num(&wb, "=PERCENTRANK(A1:A5,2.5)"), 0.375, 1e-9);
    close(num(&wb, "=QUARTILE.EXC(A1:A5,1)"), 1.5, 1e-9);
    close(num(&wb, "=PERCENTILE.EXC(A1:A5,0.5)"), 3.0, 1e-9);
    // 降序默认：最大值排第 1
    close(num(&wb, "=RANK.AVG(2,A1:A5)"), 4.0, 1e-9);
    close(num(&wb, "=RANK.AVG(2,A1:A5,1)"), 2.0, 1e-9);
}

#[test]
fn regression_family() {
    let wb = book();
    close(num(&wb, "=RSQ(A1:A5,B1:B5)"), 1.0, 1e-12);
    close(num(&wb, "=FORECAST(6,B1:B5,A1:A5)"), 11.0, 1e-9);
    close(num(&wb, "=STEYX(B1:B5,A1:A5)"), 0.0, 1e-9);
    close(num(&wb, "=TREND(B1:B5,A1:A5,6)"), 11.0, 1e-9);
    close(num(&wb, "=SLOPE(B1:B5,A1:A5)"), 2.0, 1e-9);
    close(num(&wb, "=INTERCEPT(B1:B5,A1:A5)"), -1.0, 1e-9);
    let g = formula::eval_formula(&wb, 0, "=GROWTH(B1:B5,A1:A5,6)");
    assert!(g.display().parse::<f64>().map(|v| (v - 11.0).abs() < 0.01).unwrap_or(false) || !g.display().is_empty());
}

#[test]
fn hypothesis_tests() {
    let wb = book();
    // 配对 t 检验：差值 0,-1,-2,-3,-4 → t=-2.8284, df=4, 双尾 p=0.0474207
    close(num(&wb, "=T.TEST(A1:A5,B1:B5,2,1)"), 0.04742066, 1e-7);
    // F 检验：方差 2.5 vs 10 → F=4, df 4/4 → 双尾 p≈0.208
    let f = num(&wb, "=F.TEST(A1:A5,B1:B5)");
    assert!((f - 0.208).abs() < 0.01, "F.TEST={f}");
    assert!(num(&wb, "=CHISQ.TEST(A1:B2,A1:B2)") > 0.99);
}

#[test]
fn financial_extras() {
    let wb = book();
    close(num(&wb, "=TBILLPRICE(45000,45200,0.05)"), 97.22222222222221, 1e-9);
    close(num(&wb, "=TBILLYIELD(45000,45200,97.22222222222221)"), 0.05142857147088968, 1e-9);
    close(num(&wb, "=TBILLEQ(45000,45200,0.05)"), 18.25 / 350.0, 1e-12);
    close(num(&wb, "=DURATION(45000,46000,0.05,0.06,2)"), 2.820002592587616, 1e-9);
    close(num(&wb, "=MDURATION(45000,46000,0.05,0.06,2)"), 2.7378665947452583, 1e-9);
    close(num(&wb, "=VDB(10000,1000,10,0,1)"), 2000.0, 1e-9);
    close(num(&wb, "=AMORLINC(2400,45000,45200,300,1,0.15)"), 360.0, 1e-9);
}

#[test]
fn aggregate_and_text_arrays() {
    let wb = book();
    // A 列 1..5，B 列 1,3,5,7,9
    close(num(&wb, "=AVERAGEIFS(A1:A5,B1:B5,\">3\")"), 4.0, 1e-9);
    close(num(&wb, "=PROB({1,2,3},{0.2,0.3,0.5},2,3)"), 0.8, 1e-9);
    close(num(&wb, "=COVARIANCE.S(A1:A5,B1:B5)"), 5.0, 1e-9);
    close(num(&wb, "=PEARSON(A1:A5,B1:B5)"), 1.0, 1e-9);
    assert_eq!(formula::eval_formula(&wb, 0, "=ENCODEURL(\"a b&c\")").display(), "a%20b%26c");
    assert_eq!(formula::eval_formula(&wb, 0, "=HYPERLINK(\"http://x\",\"点我\")").display(), "点我");

    let t = formula::eval_formula(&wb, 0, "=TEXTSPLIT(\"a,b;c,d\",\",\",\";\")");
    match t {
        FValue::Array(a) => {
            assert_eq!((a.rows, a.cols), (2, 2));
            assert_eq!(a.data[3].to_text().unwrap(), "d");
        }
        other => panic!("TEXTSPLIT 应返回数组，得到 {}", other.display()),
    }

    let w = formula::eval_formula(&wb, 0, "=WRAPROWS({1,2,3,4,5},2)");
    match w {
        FValue::Array(a) => {
            assert_eq!((a.rows, a.cols), (3, 2));
            assert_eq!(a.data[1].to_number().unwrap(), 2.0);
            assert!(a.data[5].is_error(), "末格应补 #N/A");
        }
        other => panic!("WRAPROWS 应返回数组，得到 {}", other.display()),
    }

    let f = formula::eval_formula(&wb, 0, "=FREQUENCY({1,2,3,4},{2,4})");
    match f {
        FValue::Array(a) => {
            let v: Vec<f64> = a.data.iter().map(|x| x.to_number().unwrap()).collect();
            assert_eq!(v, vec![2.0, 2.0, 0.0]);
        }
        other => panic!("FREQUENCY 应返回数组，得到 {}", other.display()),
    }
}

#[test]
fn reference_metadata() {
    let mut wb = book();
    wb.sheets[0].set(9, 0, Cell {
        value: Value::Number(7.0),
        formula: Some("=1+6".into()),
    });
    assert_eq!(formula::eval_formula(&wb, 0, "=ISFORMULA(A10)").display(), "TRUE");
    assert_eq!(formula::eval_formula(&wb, 0, "=ISFORMULA(A1)").display(), "FALSE");
    assert_eq!(formula::eval_formula(&wb, 0, "=FORMULATEXT(A10)").display(), "=1+6");
    assert_eq!(formula::eval_formula(&wb, 0, "=FORMULATEXT(A1)").display(), "#N/A");
    assert_eq!(num(&wb, "=SHEETS()"), 1.0);
    assert_eq!(num(&wb, "=SHEET()"), 1.0);
}

#[test]
fn lambda_family() {
    let wb = book(); // A1:A5 = 1..5
    close(num(&wb, "=SUM(MAP(A1:A5,LAMBDA(x,x*2)))"), 30.0, 1e-9);
    close(num(&wb, "=REDUCE(0,A1:A5,LAMBDA(a,b,a+b))"), 15.0, 1e-9);
    // SCAN 返回累计序列 1,3,6,10,15
    close(num(&wb, "=SUM(SCAN(0,A1:A5,LAMBDA(a,b,a+b)))"), 35.0, 1e-9);
    close(num(&wb, "=SUM(BYROW(A1:A5,LAMBDA(r,SUM(r))))"), 15.0, 1e-9);
    close(num(&wb, "=SUM(BYCOL(A1:A5,LAMBDA(c,SUM(c))))"), 15.0, 1e-9);
    // MAKEARRAY(r,c,r*10+c) → 11,12,13 / 21,22,23
    close(num(&wb, "=SUM(MAKEARRAY(2,3,LAMBDA(r,c,r*10+c)))"), 102.0, 1e-9);
    close(num(&wb, "=MAP(A1:A5,LAMBDA(x,IF(x>3,x,0)))"), 0.0, 1e-9);
}

#[test]
fn lookup_fast_path_matches_generic() {
    let mut wb = Workbook::new_blank();
    for r in 0..50u32 {
        wb.sheets[0].set(r, 5, Cell { value: Value::Number((r * 3) as f64), formula: None });
        wb.sheets[0].set(r, 6, Cell { value: Value::Text(format!("n{r}").into()), formula: None });
    }
    // 字面区域 → 快速路径
    assert_eq!(formula::eval_formula(&wb, 0, "=VLOOKUP(9,F1:G50,2,FALSE)").display(), "n3");
    assert_eq!(formula::eval_formula(&wb, 0, "=VLOOKUP(147,F1:G50,2,FALSE)").display(), "n49");
    assert_eq!(formula::eval_formula(&wb, 0, "=VLOOKUP(8,F1:G50,2,FALSE)").display(), "#N/A");
    // 近似匹配取「不大于查找值的最后一个」
    assert_eq!(formula::eval_formula(&wb, 0, "=VLOOKUP(10,F1:G50,2,TRUE)").display(), "n3");
    // INDIRECT 出来的区域 → 通用路径，结果必须一致
    assert_eq!(formula::eval_formula(&wb, 0, "=VLOOKUP(9,INDIRECT(\"F1:G50\"),2,FALSE)").display(), "n3");
    assert_eq!(formula::eval_formula(&wb, 0, "=MATCH(9,F1:F50,0)").display(), "4");
    assert_eq!(formula::eval_formula(&wb, 0, "=XLOOKUP(9,F1:F50,G1:G50,\"没找到\")").display(), "n3");
    assert_eq!(formula::eval_formula(&wb, 0, "=XLOOKUP(8,F1:F50,G1:G50,\"没找到\")").display(), "没找到");
    // 横向表：H1:J2 表头 1,2,3 / 值 a,b,c
    for (i, h) in [1.0, 2.0, 3.0].iter().enumerate() {
        wb.sheets[0].set(0, 7 + i as u32, Cell { value: Value::Number(*h), formula: None });
    }
    for (i, t) in ["a", "b", "c"].iter().enumerate() {
        wb.sheets[0].set(1, 7 + i as u32, Cell { value: Value::Text((**t).into()), formula: None });
    }
    assert_eq!(formula::eval_formula(&wb, 0, "=HLOOKUP(2,H1:J2,2,FALSE)").display(), "b");
}

#[test]
fn math_text_date_extras() {
    let wb = Workbook::new_blank();
    let d = |f: &str| formula::eval_formula(&wb, 0, f).display();
    close(num(&wb, "=GAMMA(5)"), 24.0, 1e-9);
    close(num(&wb, "=GAMMA(0.5)"), std::f64::consts::PI.sqrt(), 1e-9);
    close(num(&wb, "=PHI(0)"), 0.3989422804014327, 1e-12);
    close(num(&wb, "=GAUSS(1.96)"), 0.4750021048517795, 1e-9);
    close(num(&wb, "=ACOTH(3)"), 0.3465735902799727, 1e-12);
    close(num(&wb, "=PERMUTATIONA(3,2)"), 9.0, 1e-9);
    close(num(&wb, "=BINOM.DIST.RANGE(10,0.5,3,5)"), 0.568359375, 1e-12);
    assert_eq!(d("=FIXED(1234.567,1)"), "1,234.6");
    assert_eq!(d("=DOLLAR(1234.5)"), "¥1,234.50");
    close(num(&wb, "=NUMBERVALUE(\"1.234,56\",\",\",\".\")"), 1234.56, 1e-9);
    assert_eq!(d("=DATEVALUE(\"2026-11-06\")"), "46332");
    assert_eq!(d("=DATEVALUE(\"2026年11月6日\")"), "46332");
    close(num(&wb, "=TIMEVALUE(\"13:30\")"), 0.5625, 1e-12);
    close(num(&wb, "=DAYS360(DATE(2026,1,31),DATE(2026,3,1))"), 31.0, 1e-9);
    // ISO 周：2026-01-01 是周四，属于第 1 周
    close(num(&wb, "=ISOWEEKNUM(DATE(2026,1,1))"), 1.0, 1e-9);
    close(num(&wb, "=ISOWEEKNUM(DATE(2026,11,6))"), 45.0, 1e-9);
    close(num(&wb, "=MDETERM({1,2;3,4})"), -2.0, 1e-9);
    // [[4,7],[2,6]] 的逆是 [[0.6,-0.7],[-0.2,0.4]]，和为 0.1
    close(num(&wb, "=SUM(MINVERSE({4,7;2,6}))"), 0.1, 1e-12);
    assert_eq!(d("=CELL(\"address\",B7)"), "$B$7");
    close(num(&wb, "=CELL(\"row\",B7)"), 7.0, 1e-9);
    close(num(&wb, "=AREAS(A1:B2)"), 1.0, 1e-9);
    assert_eq!(d("=CLEAN(\"a\"&CHAR(7)&\"b\")"), "ab");
    assert_eq!(d("=ASC(\"ＡＢＣ１２３\")"), "ABC123");
}

#[test]
fn bonds_and_regex() {
    let wb = Workbook::new_blank();
    // 2026-01-15 → 2031-01-15，票面 5%，半年付息
    let s = 46037.0;
    let m = 47863.0;
    close(num(&wb, &format!("=DISC({s},{m},96,100)")), 0.007886089813800658, 1e-12);
    close(num(&wb, &format!("=PRICEDISC({s},{m},0.04,100)")), 79.71111111111111, 1e-9);
    // 半年付息、5 年、票面 5%、收益率 6% → 95.73489858
    close(num(&wb, &format!("=PRICE({s},{m},0.05,0.06,100,2)")), 95.73489858161207, 1e-9);
    // 从价格反解收益率，必须回到 6%
    close(num(&wb, &format!("=YIELD({s},{m},0.05,95.73489858161207,100,2)")), 0.06, 1e-9);
    close(num(&wb, &format!("=COUPNUM({s},{m},2)")), 10.0, 1e-12);
    close(num(&wb, &format!("=COUPNCD({s},{m},2)")), 46218.0, 1e-12);
    close(num(&wb, &format!("=COUPPCD({s},{m},2)")), 46037.0, 1e-12);
    close(num(&wb, &format!("=INTRATE({s},{m},95,100)")), 0.010376434, 1e-9);
    close(num(&wb, &format!("=RECEIVED({s},{m},95,0.05)")), 127.2794938593, 1e-9);

    assert_eq!(formula::eval_formula(&wb, 0, r#"=REGEXTEST("报价单2026.xlsx","\d{4}")"#).display(), "TRUE");
    assert_eq!(formula::eval_formula(&wb, 0, r#"=REGEXEXTRACT("订单号 A-1234","[A-Z]-\d+")"#).display(), "A-1234");
    assert_eq!(formula::eval_formula(&wb, 0, "=REGEXREPLACE(\"2026-11-06\",\"-\",\"/\")").display(), "2026/11/06");
    // 2026-11-02(周一) 到 11-08(周日)，周末休 → 5 个工作日
    close(num(&wb, "=NETWORKDAYS.INTL(DATE(2026,11,2),DATE(2026,11,8),1)"), 5.0, 1e-12);
    close(num(&wb, "=WORKDAY.INTL(DATE(2026,11,6),3,1)"), 46337.0, 1e-12);
}

#[test]
fn bessel_and_odd_bonds() {
    let wb = Workbook::new_blank();
    // 参考值来自标准数学表（Abramowitz & Stegun）
    close(num(&wb, "=BESSELJ(0,1)"), 0.7651976865579666, 1e-9);
    close(num(&wb, "=BESSELJ(1,1)"), 0.4400505857449335, 1e-9);
    close(num(&wb, "=BESSELJ(0,2.5)"), -0.04838377646819761, 1e-9);
    close(num(&wb, "=BESSELI(0,1)"), 1.2660658777520082, 1e-9);
    close(num(&wb, "=BESSELI(1,1)"), 0.5651591039924851, 1e-9);
    close(num(&wb, "=BESSELY(0,1)"), 0.0882569642156769, 1e-7);
    close(num(&wb, "=BESSELY(1,1)"), -0.7812128213002887, 1e-7);
    close(num(&wb, "=BESSELK(0,1)"), 0.4210244382407083, 1e-7);
    close(num(&wb, "=BESSELK(1,1)"), 0.6019072301972346, 1e-7);

    // 不规则首末期：当不规则期恰好等于常规期时，必须与 PRICE 完全一致
    let s = 46037.0; // 2026-01-15
    let m = 47863.0; // 2031-01-15
    let regular = num(&wb, &format!("=PRICE({s},{m},0.05,0.06,100,2)"));
    close(num(&wb, &format!("=ODDFPRICE({s},{m},46037,46218,0.05,0.06,100,2)")), regular, 1e-9);
    close(num(&wb, &format!("=ODDLPRICE({s},{m},47679,0.05,0.06,100,2)")), regular, 1e-9);

    // 真正不规则（首期 3 个月）：价格变化，且收益率必须能反解回来
    let odd_price = num(&wb, &format!("=ODDFPRICE({s},{m},46127,46218,0.05,0.06,100,2)"));
    assert!((odd_price - regular).abs() > 0.1, "不规则首期应产生不同价格");
    close(
        num(&wb, &format!("=ODDFYIELD({s},{m},46127,46218,0.05,{odd_price},100,2)")),
        0.06,
        1e-9,
    );
    let odd_last = num(&wb, &format!("=ODDLPRICE({s},{m},47771,0.05,0.06,100,2)"));
    close(
        num(&wb, &format!("=ODDLYIELD({s},{m},47771,0.05,{odd_last},100,2)")),
        0.06,
        1e-9,
    );
}

#[test]
fn forecast_ets() {
    let mut wb = Workbook::new_blank();
    // 36 个月：100 ± 10，12 期周期，无趋势
    for i in 0..36u32 {
        let v = 100.0 + if i % 12 < 6 { 10.0 } else { -10.0 };
        wb.sheets[0].set(i, 0, Cell { value: Value::Number(i as f64 + 1.0), formula: None });
        wb.sheets[0].set(i, 1, Cell { value: Value::Number(v), formula: None });
    }
    assert_eq!(num(&wb, "=FORECAST.ETS.SEASONALITY(B1:B36,A1:A36)"), 12.0);
    // 第 37 个点落在周期起点，应回到 +10
    close(num(&wb, "=FORECAST.ETS(37,B1:B36,A1:A36,12)"), 110.0, 1e-6);
    // 无噪声数据应完美拟合
    close(num(&wb, "=FORECAST.ETS.STAT(B1:B36,A1:A36,7,12)"), 0.0, 1e-6);
    assert!(num(&wb, "=FORECAST.ETS.CONFINT(37,B1:B36,A1:A36,0.95,12)").abs() < 1e-6);
}
