use sheetcore::display;
use sheetcore::model::{Cell, DateSystem, Value, Workbook};
use sheetcore::print;
use sheetcore::richtext::{parse_inline, runs_text, runs_to_inline, RichRun};
use sheetcore::style::{
    format_accounting_split, format_color, format_value, format_value_in, CellStyle, StyleTable,
};
use sheetcore::style_ext::FillPattern;

#[test]
fn indexed_color9_to_color56_resolved_from_excel_palette() {
    // [Color10] = index 17 = 0x008000 (dark green), [Color33] = index 40 = 0x00CCFF
    let c10 = format_color(&Value::Number(1.0), "[Color10]0.00");
    assert_eq!(c10, Some([0x00, 0x80, 0x00]));
    let c33 = format_color(&Value::Number(1.0), "[Color33]0.00");
    assert_eq!(c33, Some([0x00, 0xCC, 0xFF]));
    let c56 = format_color(&Value::Number(1.0), "[Color56]0.00");
    assert_eq!(c56, Some([0x33, 0x33, 0x33]));
    // Quoted [Color10] inside string literal must NOT trigger color
    let quoted = format_color(&Value::Number(1.0), "\"[Color10]\"0.00");
    assert_eq!(quoted, None);
}

#[test]
fn mask_formats_phone_ssn_and_overflow() {
    assert_eq!(format_value(&Value::Number(1234567.0), "000-0000"), "123-4567");
    assert_eq!(format_value(&Value::Number(4567.0), "000-0000"), "000-4567");
    assert_eq!(format_value(&Value::Number(12345678.0), "000-0000"), "1234-5678");
    assert_eq!(
        format_value(&Value::Number(123456789.0), "000-00-0000"),
        "123-45-6789"
    );
    assert_eq!(
        format_value(&Value::Number(123456.0), "0000 0000"),
        "0012 3456"
    );
    assert_eq!(
        format_value(&Value::Number(4155551234.0), "(000) 000-0000"),
        "(415) 555-1234"
    );
}

#[test]
fn question_mark_and_mixed_decimal_placeholders() {
    // 0.0# keeps mandatory 0 and trims optional #
    assert_eq!(format_value(&Value::Number(1.0), "0.0#"), "1.0");
    assert_eq!(format_value(&Value::Number(1.25), "0.0#"), "1.25");
    assert_eq!(format_value(&Value::Number(1.2), "0.0#"), "1.2");
    // 0.?? pads trailing insignificant decimals with spaces
    assert_eq!(format_value(&Value::Number(1.5), "0.??"), "1.5 ");
    assert_eq!(format_value(&Value::Number(1.0), "0.??"), "1   ");
    assert_eq!(format_value(&Value::Number(1.25), "0.??"), "1.25");
    // Leading ? pads integer slots with spaces
    assert_eq!(format_value(&Value::Number(7.0), "??0"), "  7");
    assert_eq!(format_value(&Value::Number(42.0), "??0"), " 42");
}

#[test]
fn scientific_carry_prefix_suffix_and_engineering_notation() {
    // Carry bug: 9.995 with 0.00E+00 must be 1.00E+01 (not 10.00E+00)
    assert_eq!(format_value(&Value::Number(9.995), "0.00E+00"), "1.00E+01");
    assert_eq!(format_value(&Value::Number(99.995), "0.00E+00"), "1.00E+02");
    // Prefix/suffix preservation
    assert_eq!(
        format_value(&Value::Number(1234.0), "\"$\"0.00E+00\" kg\""),
        "$1.23E+03 kg"
    );
    // Engineering notation (exponent multiple of 3)
    assert_eq!(
        format_value(&Value::Number(12345.0), "##0.0E+0"),
        "12.3E+3"
    );
    assert_eq!(
        format_value(&Value::Number(0.0123), "##0.0E+0"),
        "12.3E-3"
    );
}

#[test]
fn fraction_carry_zero_omission_fixed_denom_and_prefix() {
    // Carry: 0.9999 with # ?/? -> "1" (not "0 1/1")
    assert_eq!(format_value(&Value::Number(0.9999), "# ?/?"), "1");
    // Whole number in mixed fraction: 1.0 -> "1" (not "1 0")
    assert_eq!(format_value(&Value::Number(1.0), "# ?/?"), "1");
    // <1 with # ?/? omits leading "0 "
    assert_eq!(format_value(&Value::Number(0.5), "# ?/?"), "1/2");
    // <1 with 0 ?/? keeps leading "0 "
    assert_eq!(format_value(&Value::Number(0.5), "0 ?/?"), "0 1/2");
    // Fixed denominator: 0.5 with # ?/8 -> "4/8"
    assert_eq!(format_value(&Value::Number(0.5), "# ?/8"), "4/8");
    assert_eq!(format_value(&Value::Number(2.5), "# ?/8"), "2 4/8");
    // Prefix/suffix
    assert_eq!(format_value(&Value::Number(12.5), "\"$\"# ?/?"), "$12 1/2");
}

#[test]
fn elapsed_time_widths_precision_midnight_carry_and_negative_date() {
    // [hh], [mm], [ss] arbitrary width
    assert_eq!(format_value(&Value::Number(5.0 / 24.0), "[hh]:mm:ss"), "05:00:00");
    assert_eq!(format_value(&Value::Number(25.0 / 24.0), "[hh]:mm:ss"), "25:00:00");
    assert_eq!(format_value(&Value::Number(25.0 / 24.0), "[h]:mm:ss"), "25:00:00");
    assert_eq!(format_value(&Value::Number(3.0 / 1440.0), "[mm]:ss"), "03:00");
    assert_eq!(format_value(&Value::Number(7.0 / 86400.0), "[ss]"), "07");
    // Midnight carry: 1970-01-01 23:59:59.9 -> 1970-01-02 00:00:00 (not 24:00:00)
    let near_midnight = 25569.0 + 86399.9 / 86400.0;
    assert_eq!(
        format_value(&Value::Number(near_midnight), "yyyy-mm-dd hh:mm:ss"),
        "1970-01-02 00:00:00"
    );
    // Negative date serial in 1900 system -> "###" (never "-###")
    assert_eq!(
        format_value_in(DateSystem::D1900, &Value::Number(-5.0), "yyyy-mm-dd"),
        "###"
    );
}

#[test]
fn accounting_star_split_for_positive_negative_zero_and_text() {
    let acc = "_ ¥* #,##0.00_ ;_ ¥* -#,##0.00_ ;_ ¥* \"-\"??_ ;_ @_ ";
    let pos = format_accounting_split(DateSystem::D1900, &Value::Number(1234.5), acc)
        .expect("accounting split");
    assert_eq!(pos.0, " ¥");
    assert_eq!(pos.1, ' ');
    assert_eq!(pos.2, "1,234.50 ");

    let neg = format_accounting_split(DateSystem::D1900, &Value::Number(-99.5), acc)
        .expect("neg accounting split");
    assert_eq!(neg.0, " ¥");
    assert_eq!(neg.1, ' ');
    assert_eq!(neg.2, "-99.50 ");

    let leader = format_accounting_split(DateSystem::D1900, &Value::Number(50.0), "\"Total\"*.#,##0")
        .expect("dot leader split");
    assert_eq!(leader, ("Total".into(), '.', "50".into()));
}

#[test]
fn rich_text_underline_strike_vert_align_and_stale_invalidation() {
    let runs = vec![
        RichRun {
            text: "H".into(),
            bold: true,
            italic: false,
            underline: true,
            strikethrough: false,
            vert_align: 0,
            color: Some([20, 40, 80]),
            size: Some(12.0),
            font: None,
        },
        RichRun {
            text: "2".into(),
            bold: false,
            italic: false,
            underline: false,
            strikethrough: false,
            vert_align: 2, // subscript
            color: None,
            size: None,
            font: None,
        },
        RichRun {
            text: "O".into(),
            bold: false,
            italic: true,
            underline: false,
            strikethrough: true,
            vert_align: 1, // superscript
            color: None,
            size: None,
            font: None,
        },
    ];
    let xml = runs_to_inline(&runs);
    assert!(xml.contains("<u/>"));
    assert!(xml.contains("<strike/>"));
    assert!(xml.contains("<vertAlign val=\"subscript\"/>"));
    assert!(xml.contains("<vertAlign val=\"superscript\"/>"));
    let parsed = parse_inline(&xml).expect("roundtrip rich");
    assert_eq!(parsed, runs);
    assert_eq!(runs_text(&parsed), "H2O");

    // Stale rich-text cleanup on Sheet::set and Sheet::remove
    let mut wb = Workbook::new_blank();
    let sh = wb.sheet_mut(0);
    sh.rich.insert((0, 0), runs.clone());
    sh.set(0, 0, Cell { value: Value::Text("H2O".into()), formula: None });
    assert!(sh.rich.contains_key(&(0, 0)), "same text preserves rich");
    sh.set(0, 0, Cell { value: Value::Text("CO2".into()), formula: None });
    assert!(!sh.rich.contains_key(&(0, 0)), "changed text clears stale rich");
    sh.rich.insert((0, 0), runs);
    sh.remove(0, 0);
    assert!(!sh.rich.contains_key(&(0, 0)), "remove clears stale rich");
}

#[test]
fn style_table_intern_derived_preserves_pattern_and_print_renders_patterns_and_accounting() {
    let mut styles = StyleTable::new();
    let mut base = CellStyle::default_dark();
    base.fill = Some([240, 244, 255]);
    let pat = FillPattern {
        kind: "lightGrid".into(),
        fg: Some([120, 140, 180]),
        bg: Some([240, 244, 255]),
        fg_theme: None,
        bg_theme: None,
        fg_indexed: None,
        bg_indexed: None,
    };
    let sid0 = styles.intern_with_ext(base.clone(), Default::default(), Some(pat.clone()), true);
    let mut bold = base.clone();
    bold.bold = true;
    bold.vert_align = 1;
    let sid1 = styles.intern_derived(sid0, bold);
    assert_eq!(styles.pattern_of(sid1), Some(&pat));
    assert!(styles.quote_of(sid1));

    let mut wb = Workbook::new_blank();
    wb.styles = styles;
    let mut acc_st = CellStyle::default_dark();
    acc_st.format = "\"Total\"*.#,##0".into();
    let sid_acc = wb.styles.intern(acc_st);
    wb.sheet_mut(0).set(0, 0, Cell { value: Value::Number(100.0), formula: None });
    wb.sheet_mut(0).set_style(0, 0, sid1);
    wb.sheet_mut(0).set(0, 1, Cell { value: Value::Number(250.0), formula: None });
    wb.sheet_mut(0).set_style(0, 1, sid_acc);

    let view0 = display::view_at(&wb, wb.sheet(0), 0, 0, None).expect("view0");
    assert_eq!(view0.pattern, Some(pat));
    assert_eq!(view0.vert_align, 1);

    let view1 = display::view_at(&wb, wb.sheet(0), 0, 1, None).expect("view1");
    assert_eq!(view1.accounting, Some(("Total".into(), '.', "250".into())));

    let layout = print::paginate(wb.sheet(0), &wb.sheet(0).page);
    let ops = print::page_ops(&wb, 0, &layout, 0);
    assert!(
        ops.lines.len() >= 4,
        "pattern fill lightGrid should emit vector lines into print output"
    );
    assert!(
        ops.texts.iter().any(|t| t.text == "Total") && ops.texts.iter().any(|t| t.text == "250"),
        "accounting split should render left and right segments in print"
    );
}
