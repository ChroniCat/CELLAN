//! Regression tests verifying confirmed security, crash, and correctness fixes from the full code review.

use sheetcore::formula::{recalc_all_force, ErrKind};
use sheetcore::model::{Cell, Sheet, Value, Workbook};
use sheetcore::structural::{apply_structural_wb, Structural};

fn eval_one(wb: &mut Workbook, formula: &str) -> Value {
    wb.sheets[0].set(
        0,
        0,
        Cell {
            value: Value::Empty,
            formula: Some(formula.into()),
        },
    );
    recalc_all_force(wb);
    wb.sheets[0].value(0, 0)
}

#[test]
fn formula_lambda_self_recursion_and_deep_nesting_return_error_without_overflow() {
    let mut wb = Workbook::new_blank();
    // Classic Y-combinator self-recursion: must fail cleanly instead of overflowing the stack
    let v = eval_one(&mut wb, "=LAMBDA(f, f(f))(LAMBDA(f, f(f)))");
    assert!(matches!(v, Value::Error(_)), "recursive lambda must return error, got {v:?}");

    // LET-bound self-recursive lambda
    let v2 = eval_one(&mut wb, "=LET(f, LAMBDA(self, n, self(self, n+1)), f(f, 0))");
    assert!(matches!(v2, Value::Error(_)), "LET recursive lambda must return error, got {v2:?}");

    // Deeply nested parentheses must fail cleanly instead of overflowing parser stack
    let deep = format!("={}1{}", "(".repeat(256), ")".repeat(256));
    assert!(sheetcore::formula::parse(&deep).is_err(), "deep nesting must be rejected");

    // Overlong formula (> 65536 bytes) must be rejected cleanly
    let huge = format!("=1{}", "+1".repeat(40_000));
    assert!(sheetcore::formula::parse(&huge).is_err(), "overlong formula must be rejected");
}

#[test]
fn formula_networkdays_workday_isoweeknum_and_date_edge_cases() {
    let mut wb = Workbook::new_blank();

    // O(1) NETWORKDAYS on full Excel calendar span must finish rapidly without hanging,
    // and out-of-range serials (> 9999-12-31) return #NUM! immediately.
    let t0 = std::time::Instant::now();
    let v = eval_one(&mut wb, "=NETWORKDAYS(1, 2958465)");
    assert!(t0.elapsed().as_millis() < 200, "NETWORKDAYS took too long: {:?}", t0.elapsed());
    assert!(matches!(v, Value::Number(n) if n > 2_000_000.0), "got {v:?}");
    let t1 = std::time::Instant::now();
    assert_eq!(eval_one(&mut wb, "=NETWORKDAYS(1, 100000000)"), Value::Error(ErrKind::Num));
    assert!(t1.elapsed().as_millis() < 50, "out-of-range NETWORKDAYS took too long: {:?}", t1.elapsed());

    // All-weekend mask "1111111" must return #VALUE!
    assert_eq!(eval_one(&mut wb, "=WORKDAY.INTL(45000, 5, \"1111111\")"), Value::Error(ErrKind::Value));
    assert_eq!(eval_one(&mut wb, "=NETWORKDAYS.INTL(45000, 45010, \"1111111\")"), Value::Error(ErrKind::Value));

    // Extreme negative days must not panic on i64::MIN.abs()
    assert_eq!(eval_one(&mut wb, "=WORKDAY(45000, -1E30)"), Value::Error(ErrKind::Num));

    // DATE(0..1899) adds 1900 per Excel spec; out-of-range years return #NUM!
    assert_eq!(eval_one(&mut wb, "=DATE(24, 1, 1)"), eval_one(&mut wb, "=DATE(1924, 1, 1)"));
    assert_eq!(eval_one(&mut wb, "=DATE(-1, 1, 1)"), Value::Error(ErrKind::Num));
    assert_eq!(eval_one(&mut wb, "=DATE(100000, 1, 1)"), Value::Error(ErrKind::Num));

    // ISOWEEKNUM must respect 1904 date system (serial 0 in 1904 == 1904-01-01, ISO week 53 of 1903)
    wb.date_system = sheetcore::model::DateSystem::D1904;
    let w1904 = eval_one(&mut wb, "=ISOWEEKNUM(1)"); // 1904-01-02 (Sat) -> ISO week 53
    assert_eq!(w1904, Value::Number(53.0), "1904 ISOWEEKNUM should offset by 1462 days");
}

#[test]
fn formula_array_oom_guards_power_offset_address_and_infinity() {
    let mut wb = Workbook::new_blank();

    // Array allocation / multiplication overflow guards
    assert!(matches!(eval_one(&mut wb, "=SEQUENCE(100000, 100000)"), Value::Error(_)));
    assert!(matches!(eval_one(&mut wb, "=RANDARRAY(100000, 100000)"), Value::Error(_)));
    assert!(matches!(eval_one(&mut wb, "=MAKEARRAY(100000, 100000, LAMBDA(r,c,1))"), Value::Error(_)));
    assert!(matches!(eval_one(&mut wb, "=EXPAND(1, 100000, 100000)"), Value::Error(_)));
    assert!(matches!(eval_one(&mut wb, "=REPT(\"a\", 10000000000)"), Value::Error(_)));

    // 0^0 -> #NUM!, 0^-1 -> #DIV/0!, 1e999 -> #NUM!
    assert_eq!(eval_one(&mut wb, "=0^0"), Value::Error(ErrKind::Num));
    assert_eq!(eval_one(&mut wb, "=POWER(0, 0)"), Value::Error(ErrKind::Num));
    assert_eq!(eval_one(&mut wb, "=0^-1"), Value::Error(ErrKind::Div0));
    assert_eq!(eval_one(&mut wb, "=POWER(0, -1)"), Value::Error(ErrKind::Div0));
    assert_eq!(eval_one(&mut wb, "=1E999"), Value::Error(ErrKind::Num));

    // OFFSET overflow & ADDRESS bounds
    assert_eq!(eval_one(&mut wb, "=OFFSET(A1, 1E20, 0)"), Value::Error(ErrKind::Ref));
    assert_eq!(eval_one(&mut wb, "=ADDRESS(0, 1)"), Value::Error(ErrKind::Value));
    assert_eq!(eval_one(&mut wb, "=ADDRESS(1048577, 1)"), Value::Error(ErrKind::Value));
    assert_eq!(eval_one(&mut wb, "=ADDRESS(1, 16385)"), Value::Error(ErrKind::Value));
}

#[test]
fn external_source_sheet_roundtrip_target_sheet_and_same_sheet_refresh() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Data".into();
    wb.sheets[0].set(0, 0, Cell { value: Value::Number(42.0), formula: None });
    wb.sheets[0].set(0, 1, Cell { value: Value::Number(99.0), formula: None });

    let src = sheetcore::external::Source {
        name: "_sc_src_1".into(),
        kind: sheetcore::external::Kind::Internal {
            sheet: "Data".into(),
            rect: (0, 0, 0, 1),
        },
        at: (2, 0),
        target_sheet: Some("Data".into()),
        rows: 0,
        cols: 0,
        csv_opts: None,
    };
    // Same-sheet refresh must not panic (previously split_at_mut(idx) panicked when src_idx == dst_idx)
    let (r, c) = sheetcore::external::refresh(&mut wb, 0, &src).expect("same-sheet refresh");
    assert_eq!((r, c), (1, 2));
    assert_eq!(wb.sheets[0].value(2, 0), Value::Number(42.0));
    assert_eq!(wb.sheets[0].value(2, 1), Value::Number(99.0));

    // Persist and reload Source with SHEET kind & target_sheet (previously off-by-one split broke SHEET reload)
    wb.sources.push(sheetcore::external::Source { rows: r, cols: c, ..src });
    let dir = std::env::temp_dir().join(format!("sc_audit_ext_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("roundtrip.xlsx");
    sheetcore::xlsx::save(&path, &wb).unwrap();
    let back = sheetcore::xlsx::load(&path).unwrap();
    assert_eq!(back.sources.len(), 1);
    assert_eq!(back.sources[0].target_sheet.as_deref(), Some("Data"));
    assert!(matches!(
        &back.sources[0].kind,
        sheetcore::external::Kind::Internal { sheet, rect }
        if sheet == "Data" && *rect == (0, 0, 0, 1)
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn xlinks_auto_safe_blocks_unc_and_outside_paths_while_allowing_local_project_subdir() {
    let dir = std::env::temp_dir().join(format!("sc_audit_xlinks_{}", std::process::id()));
    let proj = dir.join("proj");
    let sub = proj.join("sub");
    let outside = dir.join("secret");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::create_dir_all(&outside).unwrap();

    // Write local subdir book and outside book
    let mut s_local = Workbook::new_blank();
    s_local.sheets[0].set(0, 0, Cell { value: Value::Number(77.0), formula: None });
    sheetcore::xlsx::save(&sub.join("local.xlsx"), &s_local).unwrap();

    let mut s_out = Workbook::new_blank();
    s_out.sheets[0].set(0, 0, Cell { value: Value::Number(999.0), formula: None });
    let out_path = outside.join("secret.xlsx");
    sheetcore::xlsx::save(&out_path, &s_out).unwrap();

    let mut wb = Workbook::new_blank();
    wb.path = Some(proj.join("main.xlsx"));
    wb.links.push(sheetcore::xlinks::ExternalBook {
        index: 1,
        target: "local.xlsx".into(),
        sheets: vec!["Sheet1".into()],
        ..Default::default()
    });
    wb.links.push(sheetcore::xlinks::ExternalBook {
        index: 2,
        target: r"\\192.0.2.99\share\evil.xlsx".into(),
        sheets: vec!["Sheet1".into()],
        ..Default::default()
    });
    wb.links.push(sheetcore::xlinks::ExternalBook {
        index: 3,
        target: out_path.to_string_lossy().into_owned(),
        sheets: vec!["Sheet1".into()],
        ..Default::default()
    });
    wb.sheets[0].set(0, 0, Cell { value: Value::Empty, formula: Some("=[1]Sheet1!A1".into()) });
    wb.sheets[0].set(1, 0, Cell { value: Value::Empty, formula: Some("=[2]Sheet1!A1".into()) });
    wb.sheets[0].set(2, 0, Cell { value: Value::Empty, formula: Some("=[3]Sheet1!A1".into()) });

    assert!(sheetcore::xlinks::is_network_or_remote_target(r"\\192.0.2.99\share\evil.xlsx"));
    assert!(sheetcore::xlinks::is_network_or_remote_target("http://example.com/a.xlsx"));
    assert!(sheetcore::xlinks::has_untrusted_auto_links(&wb));

    let (rep, _changed, skipped) = sheetcore::xlinks::refresh_auto_safe(&mut wb);
    assert_eq!(rep.ok, 1, "only local project subdir link should auto-refresh");
    assert_eq!(skipped, 2, "UNC and outside-dir absolute links must be skipped");
    assert_eq!(wb.links[0].cache.get(&(0, 0, 0)), Some(&Value::Number(77.0)));
    assert!(wb.links[1].cache.is_empty());
    assert!(wb.links[2].cache.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn crypto_and_patch_reject_malformed_inputs_without_panicking() {
    // Raw non-CFB bytes containing UTF-16LE "EncryptionInfo" must NOT be flagged as password-protected
    let mut fake = Vec::new();
    for u in "EncryptionInfo".encode_utf16() {
        fake.extend_from_slice(&u.to_le_bytes());
    }
    assert!(!sheetcore::crypto::needs_password_bytes(&fake));

    // patch::has_entry on corrupt file must return false, not panic
    let dir = std::env::temp_dir().join(format!("sc_audit_patch_{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let bad = dir.join("not_a_zip.xlsx");
    std::fs::write(&bad, b"not a zip file").unwrap();
    assert!(!sheetcore::patch::has_entry(&bad, "xl/workbook.xml"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn xls_biff8_write_splits_long_strings_over_8224_bytes() {
    let dir = std::env::temp_dir().join(format!("sc_audit_xls_{}", std::process::id()));
    std::fs::create_dir_all(&dir);
    let path = dir.join("long_text.xls");

    let mut wb = Workbook::new_blank();
    let long_text = "A".repeat(12_000);
    wb.sheets[0].set(0, 0, Cell { value: Value::Text(long_text.clone().into()), formula: None });
    wb.sheets[0].set(1, 0, Cell { value: Value::Text("B".repeat(500).into()), formula: None });
    sheetcore::xlsx::save(&path, &wb).expect("saving >8224B string to .xls must succeed");

    let back = sheetcore::xlsx::load(&path).expect("reloading .xls must succeed");
    assert_eq!(back.sheets[0].value(0, 0).display(), long_text);
    assert_eq!(back.sheets[0].value(1, 0).display().len(), 500);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn model_freeze_boundary_move_rows_table_token_style_cap_and_sync_bounds() {
    // 1) rename_table_column with a whitespace-only legacy column name must not panic
    {
        let mut wb = Workbook::new_blank();
        wb.sheets[0].tables.push(sheetcore::table::Table {
            name: "T1".into(),
            display: "T1".into(),
            range: (0, 0, 2, 0),
            columns: vec!["   ".into()],
            column_ids: vec![1],
            ..Default::default()
        });
        wb.sheets[0].set(1, 1, Cell { value: Value::Empty, formula: Some("=SUM(T1[Col])".into()) });
        let _ = sheetcore::table::rename_table_column(&mut wb, "T1", "   ", "NewCol");
    }

    // 2) ignoredError single-column range A1:A5 survives save/load roundtrip (area non-zero)
    {
        let dir = std::env::temp_dir().join(format!("sc_audit_ign_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("ign.xlsx");
        let mut wb = Workbook::new_blank();
        for r in 0..=4 {
            wb.sheets[0].ignored_errors.insert((r, 0));
        }
        wb.sheets[0].ignored_errors_dirty = true;
        sheetcore::xlsx::save(&p, &wb).unwrap();
        let back = sheetcore::xlsx::load(&p).unwrap();
        assert!(back.sheets[0].ignored_errors.contains(&(0, 0)));
        assert!(back.sheets[0].ignored_errors.contains(&(4, 0)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // 3) Freeze pane boundary: inserting at `at == fr` must NOT shift freeze; (0,0) must not spontaneously freeze
    let mut wb = Workbook::new_blank();
    wb.sheets[0].freeze = Some((2, 1));
    apply_structural_wb(&mut wb, 0, &Structural::InsertRows { at: 2, n: 1 });
    assert_eq!(wb.sheets[0].freeze, Some((2, 1)), "inserting at freeze boundary must keep freeze rows");
    wb.sheets[0].freeze = Some((0, 0));
    apply_structural_wb(&mut wb, 0, &Structural::InsertRows { at: 0, n: 1 });
    assert_eq!(wb.sheets[0].freeze, Some((0, 0)), "zero freeze must not become non-zero");

    // 4) move_rows preserves merges, translates moved formulas, and does not double-shift rich runs
    let mut sh = Sheet::new("S");
    sh.set(0, 0, Cell { value: Value::Number(10.0), formula: None });
    sh.set(1, 0, Cell { value: Value::Number(20.0), formula: None });
    sh.set(1, 1, Cell { value: Value::Empty, formula: Some("=A2*2".into()) });
    sh.merges.push((1, 2, 1, 3));
    sh.rich.insert((1, 0), vec![sheetcore::richtext::RichRun::plain("hi")]);
    sh.move_rows(1, 1, 0);
    assert_eq!(sh.get(0, 1).and_then(|c| c.formula.clone()).as_deref(), Some("=A1*2"));
    assert!(sh.merges.contains(&(0, 2, 0, 3)), "merge should follow moved row");
    assert!(sh.rich.contains_key(&(0, 0)), "rich text should move once to row 0, not double-shift");

    // 5) sync::shift_structural_coord drops coordinates pushed past Excel limits
    assert_eq!(
        sheetcore::sync::shift_structural_coord(1_048_575, 0, &Structural::InsertRows { at: 0, n: 1 }),
        None
    );
    assert_eq!(
        sheetcore::sync::shift_structural_coord(0, 16_383, &Structural::InsertCols { at: 0, n: 1 }),
        None
    );
}
