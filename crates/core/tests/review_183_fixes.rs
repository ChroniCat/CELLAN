//! Comprehensive regression tests for CELLAN 1.83.0 source review findings.

use sheetcore::formula::{recalc_all, recalc_all_force, ErrKind};
use sheetcore::model::{Cell, Sheet, Value, Workbook};

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

fn eval_num(formula: &str) -> f64 {
    let mut wb = Workbook::new_blank();
    match eval_one(&mut wb, formula) {
        Value::Number(v) => v,
        other => panic!("{formula}: expected Number, got {other:?}"),
    }
}

#[test]
fn issue_05_sign_zero_is_zero() {
    assert_eq!(eval_num("=SIGN(0)"), 0.0);
    assert_eq!(eval_num("=SIGN(4-4)"), 0.0);
    assert_eq!(eval_num("=SIGN(-0)"), 0.0);
    assert_eq!(eval_num("=SIGN(2.5)"), 1.0);
    assert_eq!(eval_num("=SIGN(-2.5)"), -1.0);
    let mut wb = Workbook::new_blank();
    assert_eq!(eval_one(&mut wb, "=SIGN(1/0)"), Value::Error(ErrKind::Div0));
}

#[test]
fn issue_06_atan2_excel_order_and_origin_div0() {
    assert!((eval_num("=ATAN2(1, 0)") - 0.0).abs() < 1e-12);
    assert!((eval_num("=ATAN2(0, 1)") - std::f64::consts::FRAC_PI_2).abs() < 1e-12);
    assert!((eval_num("=ATAN2(-1, 0)") - std::f64::consts::PI).abs() < 1e-12);
    assert!((eval_num("=ATAN2(0, -1)") + std::f64::consts::FRAC_PI_2).abs() < 1e-12);
    assert!((eval_num("=ATAN2(1, 1)") - std::f64::consts::FRAC_PI_4).abs() < 1e-12);
    assert!((eval_num("=ATAN2(-1, -1)") + 3.0 * std::f64::consts::FRAC_PI_4).abs() < 1e-12);

    let mut wb = Workbook::new_blank();
    assert_eq!(eval_one(&mut wb, "=ATAN2(0, 0)"), Value::Error(ErrKind::Div0));
    assert_eq!(eval_one(&mut wb, "=ATAN2(1/0, 1)"), Value::Error(ErrKind::Div0));
    assert_eq!(eval_one(&mut wb, "=ATAN2(1, 1/0)"), Value::Error(ErrKind::Div0));
}

#[test]
fn issue_07_sum_direct_scalars_vs_references_and_fast_path() {
    // Direct scalar booleans and numeric strings are coerced in Excel SUM
    assert_eq!(eval_num("=SUM(TRUE, 2)"), 3.0);
    assert_eq!(eval_num("=SUM(FALSE, 2)"), 2.0);
    assert_eq!(eval_num("=SUM(\"2\", 3)"), 5.0);
    assert_eq!(eval_num("=SUM(\"5\", 15, TRUE)"), 21.0);

    // Direct non-numeric text produces #VALUE!
    let mut wb = Workbook::new_blank();
    assert_eq!(eval_one(&mut wb, "=SUM(\"abc\", 3)"), Value::Error(ErrKind::Value));

    // Cell references and ranges ignore booleans and text, but propagate errors
    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(0, 0, Cell { value: Value::Bool(true), formula: None }); // A1
    wb.sheets[0].set(1, 0, Cell { value: Value::Text("2".into()), formula: None }); // A2
    wb.sheets[0].set(2, 0, Cell { value: Value::Text("abc".into()), formula: None }); // A3
    wb.sheets[0].set(3, 0, Cell { value: Value::Number(10.0), formula: None }); // A4
    // Single-cell references A1, A2, A3 must be ignored by SUM
    wb.sheets[0].set(0, 1, Cell { value: Value::Empty, formula: Some("=SUM(A1, A2, A3, A4)".into()) });
    // Range A1:A4 must ignore bool/text and sum only numbers
    wb.sheets[0].set(1, 1, Cell { value: Value::Empty, formula: Some("=SUM(A1:A4)".into()) });
    // Mixed range + direct scalar bool/numeric text
    wb.sheets[0].set(2, 1, Cell { value: Value::Empty, formula: Some("=SUM(A1:A4, TRUE, \"5\")".into()) });
    // Large range (>= 64 cells) + direct scalar hits try_stream_multi_arg_agg fast path
    for r in 0..80u32 {
        wb.sheets[0].set(r, 2, Cell { value: Value::Number(1.0), formula: None }); // C1:C80 = 80
    }
    wb.sheets[0].set(3, 1, Cell { value: Value::Empty, formula: Some("=SUM(C1:C80, TRUE, \"4\")".into()) });
    wb.sheets[0].set(4, 1, Cell { value: Value::Empty, formula: Some("=SUM(C1:C80, A1, A2)".into()) });
    wb.sheets[0].set(5, 1, Cell { value: Value::Empty, formula: Some("=SUM(C1:C80, \"bad\")".into()) });
    // Array literal ignores bool/text just like Excel
    wb.sheets[0].set(6, 1, Cell { value: Value::Empty, formula: Some("=SUM({TRUE,\"2\",7})".into()) });
    recalc_all(&mut wb);

    assert_eq!(wb.sheets[0].value(0, 1), Value::Number(10.0));
    assert_eq!(wb.sheets[0].value(1, 1), Value::Number(10.0));
    assert_eq!(wb.sheets[0].value(2, 1), Value::Number(16.0));
    assert_eq!(wb.sheets[0].value(3, 1), Value::Number(85.0));
    assert_eq!(wb.sheets[0].value(4, 1), Value::Number(80.0));
    assert_eq!(wb.sheets[0].value(5, 1), Value::Error(ErrKind::Value));
    assert_eq!(wb.sheets[0].value(6, 1), Value::Number(7.0));
}

#[test]
fn issue_08_averagea_maxa_mina_propagate_errors_and_preserve_excel_semantics() {
    let mut wb = Workbook::new_blank();
    for f in ["=AVERAGEA(1, 1/0)", "=MAXA(-10, 1/0)", "=MINA(10, 1/0)"] {
        assert_eq!(eval_one(&mut wb, f), Value::Error(ErrKind::Div0), "{f}");
    }
    // Direct non-numeric text errors while direct numeric text parses
    assert_eq!(eval_one(&mut wb, "=AVERAGEA(1, \"abc\")"), Value::Error(ErrKind::Value));
    assert_eq!(eval_one(&mut wb, "=MAXA(-10, \"abc\")"), Value::Error(ErrKind::Value));
    assert_eq!(eval_one(&mut wb, "=MINA(10, \"abc\")"), Value::Error(ErrKind::Value));
    assert_eq!(eval_num("=AVERAGEA(1, \"3\")"), 2.0);

    // Referenced cells: text = 0, bool = 1/0, empty ignored, errors propagated
    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(0, 0, Cell { value: Value::Number(10.0), formula: None }); // A1
    wb.sheets[0].set(1, 0, Cell { value: Value::Text("hello".into()), formula: None }); // A2 -> 0
    wb.sheets[0].set(2, 0, Cell { value: Value::Bool(true), formula: None }); // A3 -> 1
    // A4 is empty -> ignored
    wb.sheets[0].set(4, 0, Cell { value: Value::Number(-5.0), formula: None }); // A5
    wb.sheets[0].set(5, 0, Cell { value: Value::Error(ErrKind::Na), formula: None }); // A6

    wb.sheets[0].set(0, 1, Cell { value: Value::Empty, formula: Some("=AVERAGEA(A1:A4)".into()) });
    wb.sheets[0].set(1, 1, Cell { value: Value::Empty, formula: Some("=MAXA(A4:A5)".into()) });
    wb.sheets[0].set(2, 1, Cell { value: Value::Empty, formula: Some("=MINA(A1:A4)".into()) });
    wb.sheets[0].set(3, 1, Cell { value: Value::Empty, formula: Some("=AVERAGEA(A1:A6)".into()) });
    wb.sheets[0].set(4, 1, Cell { value: Value::Empty, formula: Some("=MAXA(A1:A6)".into()) });
    wb.sheets[0].set(5, 1, Cell { value: Value::Empty, formula: Some("=MINA(A1:A6)".into()) });
    wb.sheets[0].set(6, 1, Cell { value: Value::Empty, formula: Some("=AVERAGEA(A6, 1)".into()) });
    wb.sheets[0].set(7, 1, Cell { value: Value::Empty, formula: Some("=AVERAGEA(1, A2)".into()) });
    recalc_all(&mut wb);

    match wb.sheets[0].value(0, 1) {
        Value::Number(v) => assert!((v - 11.0 / 3.0).abs() < 1e-12, "got {v}"),
        other => panic!("expected 11/3, got {other:?}"),
    }
    assert_eq!(wb.sheets[0].value(1, 1), Value::Number(-5.0));
    assert_eq!(wb.sheets[0].value(2, 1), Value::Number(0.0));
    assert_eq!(wb.sheets[0].value(3, 1), Value::Error(ErrKind::Na));
    assert_eq!(wb.sheets[0].value(4, 1), Value::Error(ErrKind::Na));
    assert_eq!(wb.sheets[0].value(5, 1), Value::Error(ErrKind::Na));
    assert_eq!(wb.sheets[0].value(6, 1), Value::Error(ErrKind::Na));
    assert_eq!(wb.sheets[0].value(7, 1), Value::Number(0.5));
}

#[test]
fn issue_13_networkdays_intl_all_weekend_mask_returns_zero() {
    let mut wb = Workbook::new_blank();
    assert_eq!(eval_one(&mut wb, "=NETWORKDAYS.INTL(45000, 45010, \"1111111\")"), Value::Number(0.0));
    assert_eq!(eval_one(&mut wb, "=NETWORKDAYS.INTL(45010, 45000, \"1111111\")"), Value::Number(0.0));
    assert_eq!(eval_one(&mut wb, "=NETWORKDAYS.INTL(45000, 45010, \"0000011\")"), Value::Number(8.0));
    assert_eq!(eval_one(&mut wb, "=WORKDAY.INTL(45000, 5, \"1111111\")"), Value::Error(ErrKind::Value));
    assert_eq!(eval_one(&mut wb, "=WORKDAY.INTL(45000, 5, \"0000011\")"), Value::Number(45007.0));
}

#[test]
fn issue_09_audit_distinguishes_colliding_prefix_long_texts_and_net_diff() {
    use sheetcore::audit::{
        compute_net_diff, generate_keypair, workbook_state_hash, AuditAuthor, AuditIssueKind,
        AuditLog, AuditSeverity, AuditSnap,
    };

    let prefix = "A".repeat(512);
    let left = format!("{prefix};PAYMENT_STATUS=HOLD   ;NONCE=000000025972");
    let right = format!("{prefix};PAYMENT_STATUS=RELEASE;NONCE=000000060527");

    // 1) AuditSnap must distinguish two long texts that collide on 32-bit SHA-256 prefix
    let snap_l = AuditSnap::from_value(&Value::Text(left.clone().into()));
    let snap_r = AuditSnap::from_value(&Value::Text(right.clone().into()));
    assert_ne!(snap_l, snap_r, "AuditSnap must include full 256-bit digest");
    // Summary still remains compact for UI display
    assert!(snap_l.summary().contains(",sha="), "summary should remain compact: {}", snap_l.summary());

    // 2) workbook_state_hash must distinguish them losslessly
    let mut wb_a = Workbook::new_blank();
    wb_a.sheets[0].set(0, 0, Cell { value: Value::Text(left.into()), formula: None });
    let mut wb_b = wb_a.clone();
    wb_b.sheets[0].set(0, 0, Cell { value: Value::Text(right.into()), formula: None });
    assert_ne!(workbook_state_hash(&wb_a), workbook_state_hash(&wb_b));

    // 3) compute_net_diff must detect the long-text mutation
    let kp = generate_keypair().expect("keypair");
    let author = AuditAuthor {
        display_name: "Auditor".into(),
        os_user: "tester".into(),
        machine: "box".into(),
        pubkey_hex: kp.pubkey_hex.clone(),
    };
    let delta = compute_net_diff(
        &wb_a,
        &wb_b,
        &author,
        &std::collections::HashMap::new(),
        &[],
    );
    assert_eq!(delta.cells.len(), 1, "net diff must report the long-text cell change");
    assert_ne!(delta.cells[0].3, delta.cells[0].4);

    // 4) Signed v2 audit log detects external tampering from `left` -> `right`
    let log = AuditLog::enable(&wb_a, author, &kp, 1_700_000_000_000).expect("enable audit");
    let ok_v = log.verify_against_workbook(&wb_a);
    assert_eq!(ok_v.severity, AuditSeverity::Verified);
    let tampered_v = log.verify_against_workbook(&wb_b);
    assert_eq!(tampered_v.severity, AuditSeverity::Tampered);
    assert!(tampered_v.issues.iter().any(|i| i.kind == AuditIssueKind::ExternalModification));
}

#[test]
fn issue_10_move_rows_and_cols_use_permutation_semantics_not_copy_translate() {
    use sheetcore::structural::{move_cols_wb, move_rows_wb};

    let formula_of = |sh: &Sheet, r: u32, c: u32| -> Option<String> {
        sh.get(r, c).and_then(|cell| cell.formula.as_deref().map(str::to_owned))
    };

    // 1) Row move: unmoved target =A10 and cross-sheet =Other!A10 must stay untouched,
    //    while references to moved row (=A2, =$A$2) and displaced row (=A1) follow permutation.
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "S".into();
    let mut other = Sheet::new("Other");
    other.set(9, 0, Cell { value: Value::Number(77.0), formula: None }); // Other!A10
    other.set(0, 0, Cell { value: Value::Empty, formula: Some("=S!A2".into()) }); // cross-sheet ref into S!A2
    other.set(1, 0, Cell { value: Value::Empty, formula: Some("=S!A10".into()) }); // cross-sheet ref into unmoved S!A10
    wb.sheets.push(other);

    wb.sheets[0].set(0, 0, Cell { value: Value::Number(1.0), formula: None }); // A1 (row 0 -> 1)
    wb.sheets[0].set(1, 0, Cell { value: Value::Number(2.0), formula: None }); // A2 (row 1 -> 0)
    wb.sheets[0].set(8, 0, Cell { value: Value::Number(11.0), formula: None }); // A9 (unmoved)
    wb.sheets[0].set(9, 0, Cell { value: Value::Number(22.0), formula: None }); // A10 (unmoved)
    wb.sheets[0].set(0, 1, Cell { value: Value::Empty, formula: Some("=A1+100".into()) }); // B1 moves to B2 -> =A2+100
    wb.sheets[0].set(1, 1, Cell { value: Value::Empty, formula: Some("=A10".into()) }); // B2 moves to B1 -> stays =A10
    wb.sheets[0].set(1, 2, Cell { value: Value::Empty, formula: Some("=Other!A10".into()) }); // C2 moves to C1 -> stays =Other!A10
    wb.sheets[0].set(1, 3, Cell { value: Value::Empty, formula: Some("=A2*2".into()) }); // D2 moves to D1 -> =A1*2
    wb.sheets[0].set(1, 4, Cell { value: Value::Empty, formula: Some("=$A$2+$A$10".into()) }); // E2 moves to E1 -> =$A$1+$A$10
    wb.sheets[0].set(9, 1, Cell { value: Value::Empty, formula: Some("=A2".into()) }); // unmoved B10 referencing moved A2 -> =A1
    wb.sheets[0].set(9, 2, Cell { value: Value::Empty, formula: Some("=SUM(A1:A10)".into()) }); // covers full band -> stays =SUM(A1:A10)

    move_rows_wb(&mut wb, 0, 1, 1, 0);
    recalc_all_force(&mut wb);

    assert_eq!(formula_of(&wb.sheets[0], 0, 1).as_deref(), Some("=A10"));
    assert_eq!(wb.sheets[0].value(0, 1), Value::Number(22.0));
    assert_eq!(formula_of(&wb.sheets[0], 0, 2).as_deref(), Some("=Other!A10"));
    assert_eq!(wb.sheets[0].value(0, 2), Value::Number(77.0));
    assert_eq!(formula_of(&wb.sheets[0], 0, 3).as_deref(), Some("=A1*2"));
    assert_eq!(wb.sheets[0].value(0, 3), Value::Number(4.0));
    assert_eq!(formula_of(&wb.sheets[0], 0, 4).as_deref(), Some("=$A$1+$A$10"));
    assert_eq!(wb.sheets[0].value(0, 4), Value::Number(24.0));
    assert_eq!(formula_of(&wb.sheets[0], 1, 1).as_deref(), Some("=A2+100"));
    assert_eq!(wb.sheets[0].value(1, 1), Value::Number(101.0));
    assert_eq!(formula_of(&wb.sheets[0], 9, 1).as_deref(), Some("=A1"));
    assert_eq!(wb.sheets[0].value(9, 1), Value::Number(2.0));
    assert_eq!(formula_of(&wb.sheets[0], 9, 2).as_deref(), Some("=SUM(A1:A10)"));
    assert_eq!(formula_of(&wb.sheets[1], 0, 0).as_deref(), Some("=S!A1"));
    assert_eq!(wb.sheets[1].value(0, 0), Value::Number(2.0));
    assert_eq!(formula_of(&wb.sheets[1], 1, 0).as_deref(), Some("=S!A10"));

    // 2) Column move: B2 = J1 moving col B before col A keeps =J1, while =B1 -> =A1 and =Other!J1 stays =Other!J1
    let mut wb2 = Workbook::new_blank();
    wb2.sheets[0].name = "S".into();
    wb2.sheets[0].set(0, 1, Cell { value: Value::Number(5.0), formula: None }); // B1
    wb2.sheets[0].set(0, 8, Cell { value: Value::Number(11.0), formula: None }); // I1
    wb2.sheets[0].set(0, 9, Cell { value: Value::Number(22.0), formula: None }); // J1
    wb2.sheets[0].set(1, 1, Cell { value: Value::Empty, formula: Some("=J1".into()) }); // B2 -> A2 stays =J1
    wb2.sheets[0].set(2, 1, Cell { value: Value::Empty, formula: Some("=B1*3".into()) }); // B3 -> A3 becomes =A1*3
    wb2.sheets[0].set(3, 1, Cell { value: Value::Empty, formula: Some("=Other!J1".into()) }); // B4 -> A4 stays =Other!J1
    move_cols_wb(&mut wb2, 0, 1, 1, 0);
    recalc_all_force(&mut wb2);
    assert_eq!(formula_of(&wb2.sheets[0], 1, 0).as_deref(), Some("=J1"));
    assert_eq!(wb2.sheets[0].value(1, 0), Value::Number(22.0));
    assert_eq!(formula_of(&wb2.sheets[0], 2, 0).as_deref(), Some("=A1*3"));
    assert_eq!(wb2.sheets[0].value(2, 0), Value::Number(15.0));
    assert_eq!(formula_of(&wb2.sheets[0], 3, 0).as_deref(), Some("=Other!J1"));
}

#[test]
fn issue_14_external_source_roundtrips_pipe_quotes_and_xml_chars_through_xlsx() {
    use sheetcore::external::{CsvOpts, Kind, Source};

    // 1) Pure roundtrip with pipe, quotes, XML entities, and pipe delimiter
    let csv_src = Source {
        name: "_sc_src_1".into(),
        kind: Kind::Csv {
            path: "dir|sub/data\"a&quot;b.csv".into(),
            delim: '|',
            header: true,
        },
        at: (1, 2),
        target_sheet: Some("Sales|EU".into()),
        rows: 10,
        cols: 4,
        csv_opts: Some(CsvOpts {
            encoding: Some(sheetcore::csv::Encoding::Utf8),
            text_cols: vec![0, 3],
            quote: '\'',
        }),
    };
    let encoded = csv_src.to_name_text();
    let decoded = Source::from_name_text("_sc_src_1", &encoded).expect("decode v2 csv source");
    assert_eq!(decoded, csv_src);

    let int_src = Source {
        name: "_sc_src_2".into(),
        kind: Kind::Internal {
            sheet: "Raw|Data & <1>".into(),
            rect: (0, 0, 5, 3),
        },
        at: (0, 0),
        target_sheet: Some("Sales|EU\"2".into()),
        rows: 6,
        cols: 4,
        csv_opts: None,
    };
    let int_decoded = Source::from_name_text("_sc_src_2", &int_src.to_name_text())
        .expect("decode v2 internal source");
    assert_eq!(int_decoded, int_src);

    // 2) Legacy pipe-delimited sources remain readable
    let legacy = Source::from_name_text(
        "_sc_src_legacy",
        "\"CSV|data.csv|,|1|0|0|2|2|auto||q34|Sheet1\"",
    )
    .expect("legacy format still supported");
    assert_eq!(legacy.target_sheet.as_deref(), Some("Sheet1"));

    // 3) Full xlsx save + reload roundtrip with both `Sales|EU` and `Sales` sheets
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("pipe_target.xlsx");
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Sales|EU".into();
    wb.sheets.push(Sheet::new("Sales"));
    wb.sources.push(csv_src.clone());
    wb.sources.push(int_src.clone());
    sheetcore::xlsx::save_atomic(&path, &wb).expect("save xlsx with pipe target_sheet");

    let reloaded = sheetcore::xlsx::load(&path).expect("reload xlsx");
    assert_eq!(reloaded.sources.len(), 2);
    assert_eq!(reloaded.sources[0].target_sheet.as_deref(), Some("Sales|EU"));
    assert_eq!(reloaded.sources[0], csv_src);
    assert_eq!(reloaded.sources[1].target_sheet.as_deref(), Some("Sales|EU\"2"));
    assert_eq!(reloaded.sources[1], int_src);
}

#[test]
fn issue_02_recovered_unencrypted_xlsm_preserves_vba_and_guards_missing_vba() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/real_macro.xlsm");
    let original = dir.path().join("original.xlsm");
    std::fs::copy(&fixture, &original).expect("copy real_macro.xlsm");

    // 1) Simulate auto-recovery snapshot write while original is open
    let mut wb = sheetcore::xlsx::load(&original).expect("load macro workbook");
    assert!(wb.has_vba);
    wb.sheets[0].set(0, 0, Cell { value: Value::Number(321.0), formula: None });
    let snap = dir.path().join("recovery.xlsm");
    sheetcore::xlsx::save_atomic(&snap, &wb).expect("write recovery snapshot");
    let expected_vba = sheetcore::vba::read_vba(&snap).expect("snapshot must contain VBA");
    assert!(!expected_vba.is_empty());

    // 2) Simulate recovery open: detach base package, delete consumed recovery file
    let mut recovered = sheetcore::xlsx::load(&snap).expect("load recovery snapshot");
    sheetcore::xlsx::detach_as_base_package(&mut recovered, &snap).expect("detach base package");
    assert!(recovered.path.is_none());
    let detached_base_path = recovered.base_package.as_ref().expect("base_package set").path.clone();
    assert!(detached_base_path.is_file());
    std::fs::remove_file(&snap).expect("remove consumed recovery snapshot");

    // 3) Save As to a brand-new .xlsm path: VBA must match byte-for-byte
    let dst = dir.path().join("recovered-save-as.xlsm");
    let receipt = sheetcore::xlsx::save_atomic_with_receipt(&dst, &recovered)
        .expect("Save As recovered xlsm succeeds");
    assert_eq!(sheetcore::vba::read_vba(&dst), Some(expected_vba.clone()));

    // 4) Adopting the saved document switches wb.path and cleans up the retained temp base
    receipt.adopt_document(&mut recovered, &dst, true);
    assert_eq!(recovered.path.as_deref(), Some(dst.as_path()));
    assert!(recovered.base_package.is_none());
    assert!(!detached_base_path.exists(), "temporary recovery base must be cleaned up after adopt");

    // 5) Hard guard: if a workbook claims has_vba=true with no VBA source, saving must fail instead of silently stripping macros
    let mut orphan_vba = Workbook::new_blank();
    orphan_vba.has_vba = true;
    let bad_dst = dir.path().join("missing-vba.xlsm");
    let err = sheetcore::xlsx::save_atomic(&bad_dst, &orphan_vba)
        .expect_err("saving has_vba=true without VBA source must fail");
    assert!(err.to_string().contains("VBA"), "error should mention VBA loss: {err:#}");
    assert!(!bad_dst.exists(), "aborted save must not leave stripped xlsm");
}

#[test]
fn issue_03_unrelated_save_must_not_delete_active_staging_dir_older_than_30s() {
    use std::time::{Duration, SystemTime};

    let dir = tempfile::tempdir().expect("isolated folder");

    // Create a legacy-named `.~cellan-stage-*` directory whose directory mtime is 45 seconds old,
    // simulating another workbook's long-running save (>30s) in the same folder.
    let active_a = tempfile::Builder::new()
        .prefix(".~cellan-stage-")
        .tempdir_in(dir.path())
        .expect("A staging dir");
    let pkg_a = active_a.path().join("workbook-a.xlsx");
    let f = std::fs::File::create(&pkg_a).expect("create A staged file");
    let past = SystemTime::now() - Duration::from_secs(45);
    let _ = f.set_modified(past);
    drop(f);
    // Also backdate the directory itself on Windows/Unix if supported, or create a child older than 30s
    let _ = std::fs::File::open(active_a.path()).and_then(|d| d.set_modified(past));

    // Also create a current-PID owned `.~cellan-stage-*` directory (must never be touched by another save)
    let own_active = tempfile::Builder::new()
        .prefix(&format!(".~cellan-stage-00000000-{}-", std::process::id()))
        .tempdir_in(dir.path())
        .expect("own staging dir");
    let own_pkg = own_active.path().join("other.xlsx");
    std::fs::write(&own_pkg, b"in-flight").expect("write own staged file");

    // Unrelated workbook B completes an atomic save in the exact same directory
    let wb_b = Workbook::new_blank();
    sheetcore::xlsx::save_atomic(&dir.path().join("workbook-b.xlsx"), &wb_b)
        .expect("save B succeeds");

    assert!(active_a.path().exists(), "B's cleanup must not delete A's 45s-old staging dir");
    assert!(pkg_a.exists(), "B's cleanup must not delete A's staged package");
    assert!(own_active.path().exists(), "B's cleanup must not delete active sibling staging dir");
    assert!(own_pkg.exists(), "B's cleanup must not delete active sibling staged file");

    drop(active_a);
    drop(own_active);
}
