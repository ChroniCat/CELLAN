use sheetcore::formula::{self, ErrKind};
use sheetcore::model::{Cell, Value, Workbook};
use sheetcore::table::Table;

fn set_num(wb: &mut Workbook, si: usize, r: u32, c: u32, v: f64) {
    wb.sheets[si].set(r, c, Cell { value: Value::Number(v), formula: None });
}

fn set_txt(wb: &mut Workbook, si: usize, r: u32, c: u32, s: &str) {
    wb.sheets[si].set(r, c, Cell { value: Value::Text(s.into()), formula: None });
}

fn set_f(wb: &mut Workbook, si: usize, r: u32, c: u32, f: &str) {
    wb.sheets[si].set(r, c, Cell { value: Value::Empty, formula: Some(f.into()) });
}

fn num_at(wb: &Workbook, si: usize, r: u32, c: u32) -> f64 {
    match wb.sheets[si].value(r, c) {
        Value::Number(n) => n,
        other => panic!("expected number at ({si},{r},{c}), got {other:?}"),
    }
}

fn txt_at(wb: &Workbook, si: usize, r: u32, c: u32) -> String {
    match wb.sheets[si].value(r, c) {
        Value::Text(s) => s.to_string(),
        other => panic!("expected text at ({si},{r},{c}), got {other:?}"),
    }
}

#[test]
fn space_intersection_comma_union_dynamic_colon_and_error_literals() {
    let mut wb = Workbook::new_blank();
    // Fill 4x4 grid A1:D4 with (r+1)*10 + (c+1)
    for r in 0..4u32 {
        for c in 0..4u32 {
            set_num(&mut wb, 0, r, c, ((r + 1) * 10 + (c + 1)) as f64);
        }
    }
    // Space intersection: A1:C3 B2:D4 = B2:C3 => 22+23+32+33 = 110
    set_f(&mut wb, 0, 5, 0, "=SUM(A1:C3 B2:D4)");
    // Disjoint intersection => #NULL!
    set_f(&mut wb, 0, 5, 1, "=A1:A2 C1:C2");
    // Comma union: SUM((A1:A2, C1:C2)) = 11+21+13+23 = 68
    set_f(&mut wb, 0, 5, 2, "=SUM((A1:A2, C1:C2))");
    // AREAS on union = 2
    set_f(&mut wb, 0, 5, 3, "=AREAS((A1:A2, C1:C2))");
    // Dynamic colon: SUM(A1:INDEX(A1:A4, 3)) = 11+21+31 = 63
    set_f(&mut wb, 0, 5, 4, "=SUM(A1:INDEX(A1:A4, 3))");
    // Dynamic colon reversed: SUM(INDEX(A1:D4, 2, 2):C3) = B2:C3 = 110
    set_f(&mut wb, 0, 5, 5, "=SUM(INDEX(A1:D4, 2, 2):C3)");
    // Error literals #CALC!, #SPILL!, #FIELD!, #NAME?
    set_f(&mut wb, 0, 6, 0, "=ERROR.TYPE(#CALC!)");
    set_f(&mut wb, 0, 6, 1, "=ERROR.TYPE(#SPILL!)");
    set_f(&mut wb, 0, 6, 2, "=ERROR.TYPE(#FIELD!)");
    set_f(&mut wb, 0, 6, 3, "=ERROR.TYPE(#NAME?)");
    // 15-sig-digit tolerance & Add/Sub snap-to-zero
    set_f(&mut wb, 0, 7, 0, "=(0.1+0.2)=0.3");
    set_f(&mut wb, 0, 7, 1, "=(0.1+0.2)-0.3");

    formula::recalc_all(&mut wb);
    assert_eq!(num_at(&wb, 0, 5, 0), 110.0);
    assert_eq!(wb.sheets[0].value(5, 1), Value::Error(ErrKind::Null));
    assert_eq!(num_at(&wb, 0, 5, 2), 68.0);
    assert_eq!(num_at(&wb, 0, 5, 3), 2.0);
    assert_eq!(num_at(&wb, 0, 5, 4), 63.0);
    assert_eq!(num_at(&wb, 0, 5, 5), 110.0);
    assert_eq!(num_at(&wb, 0, 6, 0), 14.0);
    assert_eq!(num_at(&wb, 0, 6, 1), 9.0);
    assert_eq!(num_at(&wb, 0, 6, 2), 11.0);
    assert_eq!(num_at(&wb, 0, 6, 3), 5.0);
    assert_eq!(wb.sheets[0].value(7, 0), Value::Bool(true));
    assert_eq!(num_at(&wb, 0, 7, 1), 0.0);
}

#[test]
fn chinese_structured_table_names_and_bare_table_interior_refs() {
    let mut wb = Workbook::new_blank();
    set_txt(&mut wb, 0, 0, 0, "单价");
    set_txt(&mut wb, 0, 0, 1, "数量");
    set_txt(&mut wb, 0, 0, 2, "金额");
    set_num(&mut wb, 0, 1, 0, 10.0);
    set_num(&mut wb, 0, 1, 1, 3.0);
    set_f(&mut wb, 0, 1, 2, "=[@单价]*[@数量]");
    set_num(&mut wb, 0, 2, 0, 20.0);
    set_num(&mut wb, 0, 2, 1, 4.0);
    set_f(&mut wb, 0, 2, 2, "=[@单价]*[@数量]");
    // Bare column ref [金额] inside table totals row
    set_f(&mut wb, 0, 3, 2, "=SUM([金额])");
    // External Chinese table name ref outside table
    set_f(&mut wb, 0, 5, 0, "=SUM(销售表[金额])");

    wb.sheets[0].tables.push(Table {
        name: "销售表".into(),
        display: "销售表".into(),
        range: (0, 0, 3, 2),
        columns: vec!["单价".into(), "数量".into(), "金额".into()],
        column_ids: vec![1, 2, 3],
        header_row: true,
        totals_row: true,
        style_name: "TableStyleMedium2".into(),
        totals_fns: vec!["".into(), "".into(), "sum".into()],
    });

    formula::recalc_all(&mut wb);
    assert_eq!(num_at(&wb, 0, 1, 2), 30.0);
    assert_eq!(num_at(&wb, 0, 2, 2), 80.0);
    assert_eq!(num_at(&wb, 0, 3, 2), 110.0);
    assert_eq!(num_at(&wb, 0, 5, 0), 110.0);
}

#[test]
fn indirect_r1c1_names_whole_col_and_offset_dynamic_omitted() {
    let mut wb = Workbook::new_blank();
    set_num(&mut wb, 0, 0, 0, 10.0);
    set_num(&mut wb, 0, 1, 0, 20.0);
    set_num(&mut wb, 0, 2, 0, 30.0);
    set_num(&mut wb, 0, 1, 1, 99.0);
    wb.names.push(("MyCol".into(), None, "Sheet1!$A$1:$A$3".into()));

    // INDIRECT R1C1 absolute & relative
    set_f(&mut wb, 0, 0, 3, "=INDIRECT(\"R2C2\", FALSE)");
    set_f(&mut wb, 0, 1, 3, "=INDIRECT(\"R[1]C[-3]\", FALSE)"); // from (1,3) -> (2,0) = 30
    // INDIRECT defined name
    set_f(&mut wb, 0, 2, 3, "=SUM(INDIRECT(\"MyCol\"))");
    // INDIRECT whole column
    set_f(&mut wb, 0, 3, 3, "=SUM(INDIRECT(\"A:A\"))");
    // OFFSET on dynamic INDEX source + omitted height/width
    set_f(&mut wb, 0, 4, 3, "=OFFSET(INDEX(A1:B3, 1, 1), 1, 1)"); // (0,0) + (1,1) = B2 = 99
    set_f(&mut wb, 0, 5, 3, "=SUM(OFFSET(A1:A3, 0, 0, , 1))"); // omitted height keeps 3 -> 60

    formula::recalc_all(&mut wb);
    assert_eq!(num_at(&wb, 0, 0, 3), 99.0);
    assert_eq!(num_at(&wb, 0, 1, 3), 30.0);
    assert_eq!(num_at(&wb, 0, 2, 3), 60.0);
    assert_eq!(num_at(&wb, 0, 3, 3), 60.0);
    assert_eq!(num_at(&wb, 0, 4, 3), 99.0);
    assert_eq!(num_at(&wb, 0, 5, 3), 60.0);
}

#[test]
fn map_scan_reduce_byrow_bycol_with_let_lambda_and_2d_shape() {
    let mut wb = Workbook::new_blank();
    set_num(&mut wb, 0, 0, 0, 1.0);
    set_num(&mut wb, 0, 0, 1, 2.0);
    set_num(&mut wb, 0, 1, 0, 3.0);
    set_num(&mut wb, 0, 1, 1, 4.0);

    // MAP preserves 2x2 shape & accepts LET-bound lambda + 2 input arrays
    set_f(&mut wb, 0, 3, 0, "=LET(f, LAMBDA(a, b, a*10+b), MAP(A1:B2, {1,2;3,4}, f))");
    // SCAN preserves 2x2 shape & accepts LET-bound lambda
    set_f(&mut wb, 0, 6, 0, "=LET(add, LAMBDA(acc, v, acc+v), SCAN(0, A1:B2, add))");
    // BYROW with LET-bound lambda
    set_f(&mut wb, 0, 9, 0, "=LET(rs, LAMBDA(r, SUM(r)), BYROW(A1:B2, rs))");

    formula::recalc_all(&mut wb);
    assert_eq!(num_at(&wb, 0, 3, 0), 11.0);
    assert_eq!(num_at(&wb, 0, 3, 1), 22.0);
    assert_eq!(num_at(&wb, 0, 4, 0), 33.0);
    assert_eq!(num_at(&wb, 0, 4, 1), 44.0);

    assert_eq!(num_at(&wb, 0, 6, 0), 1.0);
    assert_eq!(num_at(&wb, 0, 6, 1), 3.0);
    assert_eq!(num_at(&wb, 0, 7, 0), 6.0);
    assert_eq!(num_at(&wb, 0, 7, 1), 10.0);

    assert_eq!(num_at(&wb, 0, 9, 0), 3.0);
    assert_eq!(num_at(&wb, 0, 10, 0), 7.0);
}

#[test]
fn sort_multikey_text_regex_date_ols_dbcs_numberstring_accrint_and_dsum_formula() {
    let mut wb = Workbook::new_blank();
    // TEXTBEFORE / TEXTAFTER full args
    set_f(&mut wb, 0, 0, 0, "=TEXTBEFORE(\"a-b_c-d\", {\"-\",\"_\"}, -1)");
    set_f(&mut wb, 0, 0, 1, "=TEXTAFTER(\"Alpha-Beta-Gamma\", \"-beta-\", 1, 1)");
    set_f(&mut wb, 0, 0, 2, "=TEXTBEFORE(\"hello\", \"-\", 1, 0, 1)");
    // TEXTSPLIT with ignore_empty & pad_with
    set_f(&mut wb, 0, 1, 0, "=TEXTSPLIT(\"a,,b;c\", \",\", \";\", TRUE, 0, \"N/A\")");
    // REGEXEXTRACT return_mode=1 (all matches) & REGEXREPLACE occurrence=-1
    set_f(&mut wb, 0, 4, 0, "=REGEXEXTRACT(\"id12 id34 id56\", \"[0-9]+\", 1)");
    set_f(&mut wb, 0, 5, 0, "=REGEXREPLACE(\"a1 b2 c3\", \"[0-9]\", \"X\", -1)");
    // NETWORKDAYS with holiday & WORKDAY with holiday & WEEKDAY type 12 & WEEKNUM ISO 21
    set_f(&mut wb, 0, 6, 0, "=NETWORKDAYS(DATE(2026,1,1), DATE(2026,1,7), DATE(2026,1,1))");
    set_f(&mut wb, 0, 6, 1, "=WORKDAY(DATE(2026,1,1), 1, DATE(2026,1,2))");
    set_f(&mut wb, 0, 6, 2, "=WEEKDAY(DATE(2026,1,6), 12)"); // Tue=1
    set_f(&mut wb, 0, 6, 3, "=WEEKNUM(DATE(2026,1,1), 21)"); // ISO week 1
    // DBCS & NUMBERSTRING & ACCRINTM
    set_f(&mut wb, 0, 7, 0, "=LENB(\"AB中文\")"); // 2 + 4 = 6
    set_f(&mut wb, 0, 7, 1, "=LEFTB(\"中文AB\", 4)"); // "中文"
    set_f(&mut wb, 0, 7, 2, "=NUMBERSTRING(1234, 2)");
    set_f(&mut wb, 0, 7, 3, "=ACCRINTM(DATE(2026,1,1), DATE(2026,7,1), 0.1, 1000, 0)");

    // Multivariate OLS LINEST: y = 2*x1 + 3*x2 + 5
    // Rows 10..14: col 0 = x1, col 1 = x2, col 2 = y
    let samples = [(1.0, 2.0), (2.0, 1.0), (3.0, 4.0), (4.0, 2.0), (5.0, 5.0)];
    for (i, &(x1, x2)) in samples.iter().enumerate() {
        let r = 10 + i as u32;
        set_num(&mut wb, 0, r, 0, x1);
        set_num(&mut wb, 0, r, 1, x2);
        set_num(&mut wb, 0, r, 2, 2.0 * x1 + 3.0 * x2 + 5.0);
    }
    set_f(&mut wb, 0, 16, 0, "=LINEST(C11:C15, A11:B15, TRUE, TRUE)");

    // DSUM with formula criteria (blank header E27, formula =B24>15 in E28)
    set_txt(&mut wb, 0, 22, 0, "品名");
    set_txt(&mut wb, 0, 22, 1, "数量");
    set_txt(&mut wb, 0, 22, 2, "金额");
    set_txt(&mut wb, 0, 23, 0, "甲");
    set_num(&mut wb, 0, 23, 1, 10.0);
    set_num(&mut wb, 0, 23, 2, 100.0);
    set_txt(&mut wb, 0, 24, 0, "乙");
    set_num(&mut wb, 0, 24, 1, 20.0);
    set_num(&mut wb, 0, 24, 2, 250.0);
    set_txt(&mut wb, 0, 25, 0, "丙");
    set_num(&mut wb, 0, 25, 1, 30.0);
    set_num(&mut wb, 0, 25, 2, 400.0);
    set_f(&mut wb, 0, 27, 4, "=B24>15");
    set_f(&mut wb, 0, 28, 0, "=DSUM(A23:C26, \"金额\", E27:E28)");

    // Multi-key SORT ({1, 2}, {1, -1}) on A33:B36 -> spill at D33:E36
    set_num(&mut wb, 0, 32, 0, 2.0); set_num(&mut wb, 0, 32, 1, 10.0);
    set_num(&mut wb, 0, 33, 0, 1.0); set_num(&mut wb, 0, 33, 1, 5.0);
    set_num(&mut wb, 0, 34, 0, 1.0); set_num(&mut wb, 0, 34, 1, 20.0);
    set_num(&mut wb, 0, 35, 0, 2.0); set_num(&mut wb, 0, 35, 1, 30.0);
    set_f(&mut wb, 0, 32, 3, "=SORT(A33:B36, {1, 2}, {1, -1})");

    // 2D Scalar array lift on extra function LENB
    set_f(&mut wb, 0, 38, 0, "=LENB({\"A\",\"中\";\"AB\",\"中文\"})");

    formula::recalc_all(&mut wb);
    assert_eq!(txt_at(&wb, 0, 0, 0), "a-b_c");
    assert_eq!(txt_at(&wb, 0, 0, 1), "Gamma");
    assert_eq!(txt_at(&wb, 0, 0, 2), "hello");
    assert_eq!(txt_at(&wb, 0, 1, 0), "a");
    assert_eq!(txt_at(&wb, 0, 1, 1), "b");
    assert_eq!(txt_at(&wb, 0, 2, 0), "c");
    assert_eq!(txt_at(&wb, 0, 2, 1), "N/A");
    assert_eq!(txt_at(&wb, 0, 4, 0), "12");
    assert_eq!(txt_at(&wb, 0, 4, 1), "34");
    assert_eq!(txt_at(&wb, 0, 4, 2), "56");
    assert_eq!(txt_at(&wb, 0, 5, 0), "a1 b2 cX");
    assert_eq!(num_at(&wb, 0, 6, 0), 4.0); // Jan 1..7 2026: Thu 1(hol), Fri 2, Mon 5, Tue 6, Wed 7 => 4
    // Jan 1 2026 (Thu) + 1 workday skipping Fri Jan 2 => Mon Jan 5 (46027)
    let jan5 = sheetcore::formula::date_to_serial_in(sheetcore::model::DateSystem::D1900, 2026, 1, 5).unwrap();
    assert_eq!(num_at(&wb, 0, 6, 1), jan5);
    assert_eq!(num_at(&wb, 0, 6, 2), 1.0);
    assert_eq!(num_at(&wb, 0, 6, 3), 1.0);
    assert_eq!(num_at(&wb, 0, 7, 0), 6.0);
    assert_eq!(txt_at(&wb, 0, 7, 1), "中文");
    assert_eq!(txt_at(&wb, 0, 7, 2), "壹仟贰佰叁拾肆");
    assert!((num_at(&wb, 0, 7, 3) - 50.0).abs() < 1e-6);

    // LINEST: [m2=3, m1=2, b=5], R^2 = 1.0
    assert!((num_at(&wb, 0, 16, 0) - 3.0).abs() < 1e-6);
    assert!((num_at(&wb, 0, 16, 1) - 2.0).abs() < 1e-6);
    assert!((num_at(&wb, 0, 16, 2) - 5.0).abs() < 1e-6);
    assert!((num_at(&wb, 0, 18, 0) - 1.0).abs() < 1e-6);

    // DSUM formula criteria: 250 + 400 = 650
    assert_eq!(num_at(&wb, 0, 28, 0), 650.0);

    // Multi-key SORT: (1,20), (1,5), (2,30), (2,10)
    assert_eq!(num_at(&wb, 0, 32, 3), 1.0);
    assert_eq!(num_at(&wb, 0, 32, 4), 20.0);
    assert_eq!(num_at(&wb, 0, 33, 3), 1.0);
    assert_eq!(num_at(&wb, 0, 33, 4), 5.0);
    assert_eq!(num_at(&wb, 0, 34, 3), 2.0);
    assert_eq!(num_at(&wb, 0, 34, 4), 30.0);
    assert_eq!(num_at(&wb, 0, 35, 3), 2.0);
    assert_eq!(num_at(&wb, 0, 35, 4), 10.0);

    // 2D scalar lift of LENB
    assert_eq!(num_at(&wb, 0, 38, 0), 1.0);
    assert_eq!(num_at(&wb, 0, 38, 1), 2.0);
    assert_eq!(num_at(&wb, 0, 39, 0), 2.0);
    assert_eq!(num_at(&wb, 0, 39, 1), 4.0);
}

#[test]
fn fncat_has_zero_bogus_entries() {
    for bogus in ["C", "D", "F", "K", "L", "M", "MD", "Y", "YD", "YM", "IMADD", "IMMUL"] {
        assert!(
            !sheetcore::formula::fncat::all().iter().any(|(name, _)| *name == bogus),
            "bogus pseudo-function {bogus} must be removed from fncat"
        );
    }
    assert!(sheetcore::formula::fncat::all().iter().any(|(n, _)| *n == "NUMBERSTRING"));
    assert!(sheetcore::formula::fncat::all().iter().any(|(n, _)| *n == "LENB"));
    assert!(sheetcore::formula::fncat::all().iter().any(|(n, _)| *n == "ACCRINT"));
}
