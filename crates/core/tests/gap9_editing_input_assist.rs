use sheetcore::formula::{fncat, functions};
use sheetcore::input_parse::{
    format_cell_for_edit, normalize_formula_input, parse_cell_input, parse_cell_input_with_year,
};
use sheetcore::model::{Cell, DateSystem, Value};
use sheetcore::sort_filter::current_region;
use sheetcore::Sheet;

#[test]
fn current_region_stops_at_blank_separator_rows_and_cols() {
    let mut sh = Sheet::new("MultiTable");
    // Table 1: A1:C4
    for r in 0..4u32 {
        for c in 0..3u32 {
            sh.set(r, c, Cell { value: Value::Number((r * 10 + c) as f64), formula: None });
        }
    }
    // Blank col D (col 3), Table 2: E1:G10 (taller than Table 1!)
    for r in 0..10u32 {
        for c in 4..7u32 {
            sh.set(r, c, Cell { value: Value::Number((r * 10 + c) as f64), formula: None });
        }
    }
    // Blank row 5 (row 4), Table 3: A6:C8
    for r in 5..8u32 {
        for c in 0..3u32 {
            sh.set(r, c, Cell { value: Value::Text("T3".into()), formula: None });
        }
    }
    assert_eq!(current_region(&sh, 0, 0), (0, 0, 3, 2), "表1不能串入右侧更高的表2或下方表3");
    assert_eq!(current_region(&sh, 2, 5), (0, 4, 9, 6), "表2应精准框出 E1:G10");
    assert_eq!(current_region(&sh, 6, 1), (5, 0, 7, 2), "表3应精准框出 A6:C8");
    // 远离已用区域的空格只选自己
    assert_eq!(current_region(&sh, 50, 50), (50, 50, 50, 50));
}

#[test]
fn input_parse_supports_chinese_finance_dates_times_fractions_parens_and_errors() {
    // 月日简写（固定基准年 2026）
    let md1 = parse_cell_input_with_year("11/25", "", DateSystem::D1900, 2026);
    let ser_nov25 = functions::date_to_serial_in(DateSystem::D1900, 2026, 11, 25).unwrap();
    assert_eq!(md1.value, Value::Number(ser_nov25));
    assert_eq!(md1.auto_format.as_deref(), Some("m/d"));

    let md2 = parse_cell_input_with_year("11-25", "", DateSystem::D1900, 2026);
    assert_eq!(md2.value, Value::Number(ser_nov25));
    assert_eq!(md2.auto_format.as_deref(), Some("m-d"));

    let md_cn = parse_cell_input_with_year("11月25日", "", DateSystem::D1900, 2026);
    assert_eq!(md_cn.value, Value::Number(ser_nov25));
    assert_eq!(md_cn.auto_format.as_deref(), Some("m\"月\"d\"日\""));

    let md_cn2 = parse_cell_input_with_year("11月25号", "", DateSystem::D1900, 2026);
    assert_eq!(md_cn2.value, Value::Number(ser_nov25));

    // 年月
    let ym1 = parse_cell_input("2026-11", "", DateSystem::D1900);
    let ser_nov1 = functions::date_to_serial_in(DateSystem::D1900, 2026, 11, 1).unwrap();
    assert_eq!(ym1.value, Value::Number(ser_nov1));
    assert_eq!(ym1.auto_format.as_deref(), Some("yyyy-mm"));

    let ym_cn = parse_cell_input("2026年11月", "", DateSystem::D1900);
    assert_eq!(ym_cn.value, Value::Number(ser_nov1));
    assert_eq!(ym_cn.auto_format.as_deref(), Some("yyyy\"年\"m\"月\""));

    // 单位数分秒 & 超过 24 小时累计时间
    let t_short = parse_cell_input("9:5", "", DateSystem::D1900);
    let expected_905 = (9.0 * 3600.0 + 5.0 * 60.0) / 86_400.0;
    assert!((match t_short.value { Value::Number(v) => v, _ => panic!() } - expected_905).abs() < 1e-12);
    assert_eq!(t_short.auto_format.as_deref(), Some("hh:mm"));

    let t_elapsed = parse_cell_input("25:00", "", DateSystem::D1900);
    assert!((match t_elapsed.value { Value::Number(v) => v, _ => panic!() } - (25.0 / 24.0)).abs() < 1e-12);
    assert_eq!(t_elapsed.auto_format.as_deref(), Some("[h]:mm"));
    assert_eq!(
        format_cell_for_edit(DateSystem::D1900, &t_elapsed.value, "[h]:mm", false),
        "25:00"
    );

    let t_elapsed_s = parse_cell_input("100:30:15", "", DateSystem::D1900);
    assert_eq!(t_elapsed_s.auto_format.as_deref(), Some("[h]:mm:ss"));
    assert_eq!(
        format_cell_for_edit(DateSystem::D1900, &t_elapsed_s.value, "[h]:mm:ss", false),
        "100:30:15"
    );

    // 会计括号负数
    let pn1 = parse_cell_input("(1,000.50)", "", DateSystem::D1900);
    assert_eq!(pn1.value, Value::Number(-1000.5));
    assert_eq!(pn1.auto_format.as_deref(), Some("#,##0.00;(#,##0.00)"));

    let pn2 = parse_cell_input("(¥500)", "", DateSystem::D1900);
    assert_eq!(pn2.value, Value::Number(-500.0));
    assert_eq!(pn2.auto_format.as_deref(), Some("¥#,##0;(¥#,##0)"));

    let pn3 = parse_cell_input("$(1,200.25)", "", DateSystem::D1900);
    assert_eq!(pn3.value, Value::Number(-1200.25));
    assert_eq!(pn3.auto_format.as_deref(), Some("$#,##0.00;($#,##0.00)"));

    let pn_pct = parse_cell_input("(12.5%)", "", DateSystem::D1900);
    assert!((match pn_pct.value { Value::Number(v) => v, _ => panic!() } + 0.125).abs() < 1e-12);
    assert_eq!(pn_pct.auto_format.as_deref(), Some("0.0%;(0.0%)"));

    // 带分数
    let f1 = parse_cell_input("1 1/2", "", DateSystem::D1900);
    assert_eq!(f1.value, Value::Number(1.5));
    assert_eq!(f1.auto_format.as_deref(), Some("# ?/?"));

    let f2 = parse_cell_input("0 3/4", "", DateSystem::D1900);
    assert_eq!(f2.value, Value::Number(0.75));

    let f3 = parse_cell_input("-2 3/16", "", DateSystem::D1900);
    assert!((match f3.value { Value::Number(v) => v, _ => panic!() } + 2.1875).abs() < 1e-12);
    assert_eq!(f3.auto_format.as_deref(), Some("# ??/??"));

    // 错误字面量
    for (raw, want) in [
        ("#N/A", sheetcore::formula::ErrKind::Na),
        ("#n/a", sheetcore::formula::ErrKind::Na),
        ("#DIV/0!", sheetcore::formula::ErrKind::Div0),
        ("#VALUE!", sheetcore::formula::ErrKind::Value),
        ("#REF!", sheetcore::formula::ErrKind::Ref),
        ("#NAME?", sheetcore::formula::ErrKind::Name),
        ("#NUM!", sheetcore::formula::ErrKind::Num),
        ("#NULL!", sheetcore::formula::ErrKind::Null),
        ("#SPILL!", sheetcore::formula::ErrKind::Spill),
        ("#CALC!", sheetcore::formula::ErrKind::Calc),
    ] {
        let p = parse_cell_input(raw, "", DateSystem::D1900);
        assert_eq!(p.value, Value::Error(want), "raw={raw}");
    }
    // 文本格式 `@` 保持原样不转错误
    assert_eq!(
        parse_cell_input("#N/A", "@", DateSystem::D1900).value,
        Value::Text("#N/A".into())
    );
}

#[test]
fn normalize_formula_input_handles_leading_plus_minus_and_missing_close_parens() {
    assert_eq!(normalize_formula_input("=SUM(A1:A5").as_deref(), Some("=SUM(A1:A5)"));
    assert_eq!(
        normalize_formula_input("=IF(A1=\"(test\", SUM(B1:B3").as_deref(),
        Some("=IF(A1=\"(test\", SUM(B1:B3))"),
        "字符串中的括号不能参与括号配对计数"
    );
    assert_eq!(normalize_formula_input("+A1+B1").as_deref(), Some("=A1+B1"));
    assert_eq!(normalize_formula_input("+SUM(A1:A5").as_deref(), Some("=SUM(A1:A5)"));
    assert_eq!(normalize_formula_input("-A1*2").as_deref(), Some("=-A1*2"));
    assert_eq!(normalize_formula_input("+Sheet2!$A$1+10").as_deref(), Some("=Sheet2!$A$1+10"));
    assert_eq!(normalize_formula_input("+(1+2)*3").as_deref(), Some("=(1+2)*3"));
    assert_eq!(normalize_formula_input("+100+200").as_deref(), Some("=100+200"));

    // 普通有符号数字/货币/百分比/千分位/文本绝对不能误判为公式
    for non_formula in [
        "+123",
        "-45.67",
        "+1.5e3",
        "-15%",
        "-$500",
        "+¥1,000",
        "-1,234.56",
        "-2 1/4",
        "-hello",
        "+备注",
        "+86-138-0013-8000",
    ] {
        assert_eq!(
            normalize_formula_input(non_formula),
            None,
            "{non_formula:?} 不应误转为公式"
        );
    }
}

#[test]
fn every_implemented_function_has_exact_signature_and_description() {
    let impls = functions::implemented();
    assert!(impls.len() >= 350, "implemented count = {}", impls.len());
    let mut missing_sig = Vec::new();
    let mut missing_desc = Vec::new();
    for f in &impls {
        if fncat::signature_ref(f).is_none() {
            missing_sig.push(*f);
        }
        if fncat::description(f) == "函数" {
            missing_desc.push(*f);
        }
    }
    assert!(missing_sig.is_empty(), "缺少签名的已实现函数: {missing_sig:?}");
    assert!(missing_desc.is_empty(), "缺少中文说明的已实现函数: {missing_desc:?}");
}
