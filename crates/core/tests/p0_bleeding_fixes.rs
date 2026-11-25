//! P0 止血专项回归测试（覆盖 A/B/C 三组核心引擎修复）。

use sheetcore::cf::{CfBlock, CfKind, CfRule, Cfvo, CfvoKind, CfStats};
use sheetcore::charts::{Chart, ChartKind, Series};
use sheetcore::consolidate::{ConsFn, Source};
use sheetcore::dv::{DataValidation, DvKind, DvOp};
use sheetcore::formula::{self, ErrKind};
use sheetcore::model::{Cell, DateSystem, Sheet, Value, Workbook};
use sheetcore::page::{paper_mm, PageSetup};
use sheetcore::pic::Picture;
use sheetcore::print::{header_parts, page_ops, paginate};
use sheetcore::richtext::RichRun;
use sheetcore::sort_filter::{CmpOp, ColorFilter, Criterion, DateGroupItem, Filter};
use sheetcore::structural::{apply_structural_wb, Structural};
use sheetcore::style::{format_color, format_value, CellStyle, StyleTable};
use sheetcore::style_ext::FillPattern;
use sheetcore::style_theme::{ThemeRef, XfTheme};
use sheetcore::table::Table;
use sheetcore::textbox::TextBox;

fn eval_cell(wb: &mut Workbook, r: u32, c: u32, f: &str) -> Value {
    wb.sheets[0].set(
        r,
        c,
        Cell {
            value: Value::Empty,
            formula: Some(f.into()),
        },
    );
    formula::recalc_all(wb);
    wb.sheets[0].value(r, c)
}

fn num(v: Value) -> f64 {
    match v {
        Value::Number(n) => n,
        other => panic!("expected Number, got {other:?}"),
    }
}

#[test]
fn p0_01_calc_iteration_patch_produces_well_formed_xml() {
    let xml = r#"<workbook><calcPr calcId="191029"/></workbook>"#;
    let out = sheetcore::patch::set_calc_iteration(xml, true, 50, 0.001);
    assert!(!out.contains("/> >") && !out.contains("/>>"), "stray > in: {out}");
    assert!(out.contains("iterate=\"1\""), "missing iterate=1: {out}");
    assert!(out.contains("maxIterations=\"50\""), "missing maxIterations=50: {out}");
    assert!(out.ends_with("</workbook>"), "malformed closing tag: {out}");
    // Second consecutive save must also stay well-formed without accumulating '>'
    let out2 = sheetcore::patch::set_calc_iteration(&out, true, 50, 0.001);
    assert!(!out2.contains("/>>"), "stray > after 2nd save: {out2}");
    assert!(out2.ends_with("</workbook>"), "malformed after 2nd save: {out2}");
}

#[test]
fn p0_04_clear_cf_and_page_xml_order() {
    let ws = "<worksheet><sheetData/><conditionalFormatting sqref=\"A1:A5\"><cfRule type=\"cellIs\" priority=\"1\"/></conditionalFormatting></worksheet>";
    let cleared = sheetcore::patch::insert_cf(ws, "");
    assert!(
        !cleared.contains("<conditionalFormatting"),
        "empty CF blocks must strip existing CF: {cleared}"
    );

    let ws2 = "<worksheet><sheetData/><drawing r:id=\"rId1\"/></worksheet>";
    let with_page = sheetcore::patch::set_page_xml(ws2, &PageSetup {
        header: "H".into(),
        ..Default::default()
    });
    let idx_ps = with_page.find("<pageSetup").expect("pageSetup inserted");
    let idx_dr = with_page.find("<drawing").expect("drawing kept");
    assert!(idx_ps < idx_dr, "pageSetup must precede drawing: {with_page}");
}

#[test]
fn p0_07_style_intern_derived_preserves_theme_pattern_and_quote() {
    let mut table = StyleTable::new();
    let base = table.intern(CellStyle::default());
    let mut th = XfTheme::default();
    th.fill = Some(ThemeRef { idx: 4, tint: 0.0 });
    table.set_theme(base, th);
    table.set_pattern(
        base,
        FillPattern {
            kind: "lightGray".into(),
            fg: Some([10, 20, 30]),
            bg: None,
            fg_theme: None,
            bg_theme: None,
            fg_indexed: None,
            bg_indexed: None,
        },
    );
    table.set_quote(base, true);

    let mut bold = table.get(base).clone();
    bold.bold = true;
    let derived = table.intern_derived(base, bold);
    assert_eq!(table.theme_of(derived).fill.map(|t| t.idx), Some(4));
    assert!(table.pattern_of(derived).is_some());
    assert!(table.quote_of(derived));
}

#[test]
fn p0_09_csv_preserves_long_ids_and_quoted_leading_zeros() {
    let dir = std::env::temp_dir().join("cellan_p0_09_csv");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("ids.csv");
    std::fs::write(
        &path,
        "id,code,amount\n110101199001011234,\"00123\",42.5\n",
    )
    .unwrap();
    let mut sh = Sheet::new("CSV");
    sheetcore::external::import_csv(&mut sh, path.to_str().unwrap(), ',', true, (0, 0)).unwrap();
    assert_eq!(sh.value(1, 0), Value::Text("110101199001011234".into()));
    assert_eq!(sh.value(1, 1), Value::Text("00123".into()));
    assert_eq!(sh.value(1, 2), Value::Number(42.5));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn p0_11_number_format_scaling_conditions_and_leading_zeros() {
    assert_eq!(format_value(&Value::Number(1_234_567.0), "#,##0,"), "1,235");
    assert_eq!(format_value(&Value::Number(2_500_000.0), "0.0,,"), "2.5");
    assert_eq!(format_value(&Value::Number(42.0), "00000"), "00042");
    assert_eq!(
        format_value(&Value::Number(150.0), "[>100][Blue]\"高\";[<=100][Red]\"低\""),
        "高"
    );
    assert_eq!(
        format_color(&Value::Number(150.0), "[>100][Blue]\"高\";[<=100][Red]\"低\""),
        Some([0, 0, 255])
    );
    assert_eq!(
        format_value(&Value::Number(-5.0), "[>100]\"高\";[<=100]\"低\""),
        "低"
    );
}

#[test]
fn p0_14_and_15_find_search_and_text_formatting() {
    let mut wb = Workbook::new_blank();
    assert_eq!(num(eval_cell(&mut wb, 0, 0, "=FIND(\"文\",\"你好中文\")")), 4.0);
    assert_eq!(num(eval_cell(&mut wb, 0, 1, "=SEARCH(\"中*试\",\"你好中文测试\")")), 3.0);
    assert_eq!(num(eval_cell(&mut wb, 0, 2, "=SEARCH(\"~*\",\"a*b\")")), 2.0);
    assert_eq!(
        eval_cell(&mut wb, 0, 3, "=TEXT(1234.5,\"#,##0.00\")"),
        Value::Text("1,234.50".into())
    );
    assert_eq!(
        eval_cell(&mut wb, 0, 4, "=TEXT(0.256,\"0.0%\")"),
        Value::Text("25.6%".into())
    );
}

#[test]
fn p0_16_sumifs_countif_sumproduct_strict_semantics() {
    let mut wb = Workbook::new_blank();
    let sh = &mut wb.sheets[0];
    sh.set(0, 0, Cell { value: Value::Text("甲".into()), formula: None });
    sh.set(1, 0, Cell { value: Value::Empty, formula: None });
    sh.set(2, 0, Cell { value: Value::Bool(true), formula: None });
    sh.set(3, 0, Cell { value: Value::Number(0.0), formula: None });
    sh.set(0, 1, Cell { value: Value::Number(10.0), formula: None });
    sh.set(1, 1, Cell { value: Value::Number(20.0), formula: None });
    sh.set(2, 1, Cell { value: Value::Number(30.0), formula: None });
    sh.set(3, 1, Cell { value: Value::Number(40.0), formula: None });

    // Empty and Bool must NOT match `=0` or `>=0`
    assert_eq!(num(eval_cell(&mut wb, 5, 0, "=COUNTIF(A1:A4,\"=0\")")), 1.0);
    assert_eq!(num(eval_cell(&mut wb, 5, 1, "=SUMIF(A1:A4,\">=0\",B1:B4)")), 40.0);
    assert_eq!(num(eval_cell(&mut wb, 5, 2, "=COUNTIF(A1:A4,\">=0\")")), 1.0);

    // SUMPRODUCT propagates errors and treats text/bool as 0
    wb.sheets[0].set(0, 2, Cell { value: Value::Error(ErrKind::Div0), formula: None });
    wb.sheets[0].set(1, 2, Cell { value: Value::Text("abc".into()), formula: None });
    assert_eq!(
        eval_cell(&mut wb, 5, 3, "=SUMPRODUCT(C1:C2,B1:B2)"),
        Value::Error(ErrKind::Div0)
    );
    assert_eq!(num(eval_cell(&mut wb, 5, 4, "=SUMPRODUCT(C2:C2,B2:B2)")), 0.0);
}

#[test]
fn p0_17_array_if_and_scalar_function_lifting() {
    let mut wb = Workbook::new_blank();
    let sh = &mut wb.sheets[0];
    sh.set(0, 0, Cell { value: Value::Number(-2.0), formula: None });
    sh.set(1, 0, Cell { value: Value::Number(3.0), formula: None });
    sh.set(2, 0, Cell { value: Value::Number(-5.0), formula: None });
    assert_eq!(num(eval_cell(&mut wb, 0, 1, "=SUM(IF(A1:A3<0,-A1:A3,A1:A3))")), 10.0);
    assert_eq!(num(eval_cell(&mut wb, 1, 1, "=SUM(ABS(A1:A3))")), 10.0);
}

#[test]
fn p0_18_financial_math_array_lookup_fixes() {
    let mut wb = Workbook::new_blank();
    assert_eq!(num(eval_cell(&mut wb, 0, 0, "=EVEN(-3)")), -4.0);
    assert_eq!(num(eval_cell(&mut wb, 0, 1, "=IPMT(0.1,1,10,-1000,0,1)")), 0.0);
    assert_eq!(eval_cell(&mut wb, 0, 2, "=ISREF(A1)"), Value::Bool(true));
    assert_eq!(eval_cell(&mut wb, 0, 3, "=ISREF(123)"), Value::Bool(false));
    assert!((num(eval_cell(&mut wb, 0, 4, "=DOLLARDE(1.02,16)")) - 1.125).abs() < 1e-9);
    assert!((num(eval_cell(&mut wb, 0, 5, "=DOLLARFR(1.125,16)")) - 1.02).abs() < 1e-9);

    // Floating-point exact lookup: 0.1+0.2 matches 0.3
    wb.sheets[0].set(2, 0, Cell { value: Value::Number(0.3), formula: None });
    wb.sheets[0].set(2, 1, Cell { value: Value::Text("ok".into()), formula: None });
    assert_eq!(num(eval_cell(&mut wb, 2, 2, "=MATCH(0.1+0.2,A3:A3,0)")), 1.0);
    assert_eq!(
        eval_cell(&mut wb, 2, 3, "=XLOOKUP(0.1+0.2,A3:A3,B3:B3)"),
        Value::Text("ok".into())
    );
    assert_eq!(
        eval_cell(&mut wb, 2, 4, "=XLOOKUP(999,A3:A3,B3:B3,\"缺失\")"),
        Value::Text("缺失".into())
    );
}

#[test]
fn p0_19_tiny_number_display_percent_input_and_empty_text_arith() {
    assert_eq!(
        sheetcore::model::format_number(1e-12),
        "1E-12",
        "1e-12 must not collapse to 0"
    );
    let parsed = sheetcore::input_parse::parse_cell_input("15", "0%", DateSystem::D1900);
    assert_eq!(parsed.value, Value::Number(0.15));

    let mut wb = Workbook::new_blank();
    assert_eq!(eval_cell(&mut wb, 0, 0, "=\"\"+1"), Value::Error(ErrKind::Value));
}

#[test]
fn p0_20_icon_mark_propagates_show_value_false() {
    let mut sh = Sheet::new("S");
    sh.set(0, 0, Cell { value: Value::Number(10.0), formula: None });
    sh.set(1, 0, Cell { value: Value::Number(90.0), formula: None });
    sh.cf.blocks.push(CfBlock {
        ranges: vec![(0, 0, 1, 0)],
        rules: vec![CfRule {
            kind: CfKind::IconSet {
                set: "3Arrows".into(),
                reverse: false,
                show_value: false,
                thresholds: vec![
                    Cfvo { kind: CfvoKind::Percent, val: 0.0, formula: String::new() },
                    Cfvo { kind: CfvoKind::Percent, val: 33.0, formula: String::new() },
                    Cfvo { kind: CfvoKind::Percent, val: 67.0, formula: String::new() },
                ],
            },
            stop: false,
            priority: 1,
        }],
    });
    let mut stats = CfStats::default();
    sheetcore::cf::compute_stats(&sh.cf, &sh, &mut stats);
    let ov = sheetcore::cf::overlay_at(&sh.cf, &stats, &sh, 1, 0, |_, _, _| false);
    assert_eq!(ov.icon.as_ref().map(|m| m.show_value), Some(false));
}

#[test]
fn p0_21_consolidate_row_labels_and_col_labels_not_swapped() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "S1".into();
    // Only leftmost column holds row labels (no top header row)
    wb.sheets[0].set(0, 0, Cell { value: Value::Text("苹果".into()), formula: None });
    wb.sheets[0].set(0, 1, Cell { value: Value::Number(10.0), formula: None });
    wb.sheets[0].set(1, 0, Cell { value: Value::Text("香蕉".into()), formula: None });
    wb.sheets[0].set(1, 1, Cell { value: Value::Number(20.0), formula: None });
    wb.sheets.push(Sheet::new("OutLeftOnly"));
    let src = [Source {
        sheet: "S1".into(),
        rect: (0, 0, 1, 1),
    }];
    // row_labels=false (no top header), col_labels=true (leftmost col holds row labels)
    sheetcore::consolidate::consolidate(&mut wb, 1, (0, 0), &src, ConsFn::Sum, false, true)
        .unwrap();
    assert_eq!(wb.sheets[1].value(0, 1), Value::Empty, "no top header when row_labels=false");
    assert_eq!(wb.sheets[1].value(1, 0), Value::Text("苹果".into()));
    assert_eq!(wb.sheets[1].value(1, 1), Value::Number(10.0));
    assert_eq!(wb.sheets[1].value(2, 0), Value::Text("香蕉".into()));
    assert_eq!(wb.sheets[1].value(2, 1), Value::Number(20.0));

    // Only top row holds column labels (no leftmost row label col)
    let mut top = Sheet::new("S2");
    top.set(0, 0, Cell { value: Value::Text("一月".into()), formula: None });
    top.set(1, 0, Cell { value: Value::Number(30.0), formula: None });
    top.set(2, 0, Cell { value: Value::Number(40.0), formula: None });
    wb.sheets.push(top);
    wb.sheets.push(Sheet::new("OutTopOnly"));
    let src2 = [Source {
        sheet: "S2".into(),
        rect: (0, 0, 2, 0),
    }];
    sheetcore::consolidate::consolidate(&mut wb, 3, (0, 0), &src2, ConsFn::Sum, true, false)
        .unwrap();
    assert_eq!(wb.sheets[3].value(0, 1), Value::Text("一月".into()));
    assert_eq!(wb.sheets[3].value(1, 0), Value::Empty, "no left col label when col_labels=false");
    assert_eq!(wb.sheets[3].value(1, 1), Value::Number(30.0));
    assert_eq!(wb.sheets[3].value(2, 1), Value::Number(40.0));
}

#[test]
fn p0_22_relocate_moved_rect_updates_absolute_and_relative_refs() {
    let f = "=SUM($A$1:$A$3)+B1";
    let moved = sheetcore::refs::relocate_moved_rect(
        f,
        "Sheet1",
        "Sheet1",
        (0, 0, 2, 0),
        "Sheet1",
        5,
        2,
    );
    assert_eq!(moved, "=SUM($C$6:$C$8)+B1");
}

#[test]
fn p0_27_autofilter_between_text_date_and_color_roundtrip() {
    let dir = std::env::temp_dir().join("cellan_p0_27_filter");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("af.xlsx");

    let mut wb = Workbook::new_blank();
    let red_sid = wb.styles.intern(CellStyle {
        fill: Some([255, 0, 0]),
        ..Default::default()
    });
    let date_sid = wb.styles.intern(CellStyle {
        format: "yyyy-mm-dd".into(),
        ..Default::default()
    });
    let sh = &mut wb.sheets[0];
    sh.set(0, 0, Cell { value: Value::Text("名".into()), formula: None });
    sh.set(0, 1, Cell { value: Value::Text("值".into()), formula: None });
    sh.set(0, 2, Cell { value: Value::Text("日".into()), formula: None });
    sh.set(0, 3, Cell { value: Value::Text("色".into()), formula: None });
    sh.set(1, 0, Cell { value: Value::Text("张三".into()), formula: None });
    sh.set(1, 1, Cell { value: Value::Number(15.0), formula: None });
    sh.set(1, 2, Cell { value: Value::Number(46023.0), formula: None }); // 2026-01-01
    sh.set_style(1, 2, date_sid);
    sh.set(1, 3, Cell { value: Value::Text("红".into()), formula: None });
    sh.set_style(1, 3, red_sid);
    sh.set(2, 0, Cell { value: Value::Text("李四".into()), formula: None });
    sh.set(2, 1, Cell { value: Value::Number(50.0), formula: None });
    sh.set(2, 2, Cell { value: Value::Number(45658.0), formula: None }); // 2025-01-01
    sh.set_style(2, 2, date_sid);
    sh.set(2, 3, Cell { value: Value::Text("白".into()), formula: None });

    let mut f = Filter::new(0, 0, 2, 3);
    f.criteria.insert(0, Criterion::new(CmpOp::Contains, "张"));
    f.criteria.insert(1, Criterion::between("10", "20"));
    f.date_groups.insert(
        2,
        vec![DateGroupItem {
            year: 2026,
            month: Some(1),
            day: None,
        }],
    );
    f.color_filters.insert(3, ColorFilter::Fill(Some([255, 0, 0])));
    sh.filter = Some(f);
    sh.filter_dirty = true;

    sheetcore::xlsx::save_atomic(&path, &wb).unwrap();
    let loaded = sheetcore::xlsx::load(&path).unwrap();
    let lf = loaded.sheets[0].filter.as_ref().expect("filter persisted");
    assert_eq!(lf.criteria.get(&0), Some(&Criterion::new(CmpOp::Contains, "张")));
    assert_eq!(lf.criteria.get(&1), Some(&Criterion::between("10", "20")));
    assert!(lf.date_groups.contains_key(&2));
    assert_eq!(lf.color_filters.get(&3), Some(&ColorFilter::Fill(Some([255, 0, 0]))));
    assert!(loaded.sheets[0].hidden_rows.contains(&2), "row 2 must be hidden on load");
    assert!(!loaded.sheets[0].hidden_rows.contains(&1), "row 1 must remain visible");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn p0_28_dv_unquoted_list_named_range_and_date_bound() {
    let mut wb = Workbook::new_blank();
    wb.names.push(("DeptList".into(), None, "Sheet1!$B$1:$B$2".into()));
    wb.sheets[0].set(0, 1, Cell { value: Value::Text("财务".into()), formula: None });
    wb.sheets[0].set(1, 1, Cell { value: Value::Text("研发".into()), formula: None });

    let dv_unquoted = DataValidation {
        ranges: vec![(0, 0, 0, 0)],
        kind: DvKind::List,
        f1: "甲,乙,丙".into(),
        ..Default::default()
    };
    assert!(sheetcore::dv::validate_wb(
        &wb,
        0,
        &dv_unquoted,
        &Value::Text("乙".into()),
        0,
        0,
        |_, _, _| formula::FValue::Empty
    ));
    assert!(!sheetcore::dv::validate_wb(
        &wb,
        0,
        &dv_unquoted,
        &Value::Text("丁".into()),
        0,
        0,
        |_, _, _| formula::FValue::Empty
    ));

    // Named range must NOT be wrapped in quotes when serialized
    let dv_named = DataValidation {
        ranges: vec![(1, 0, 1, 0)],
        kind: DvKind::List,
        f1: "DeptList".into(),
        ..Default::default()
    };
    let xml = sheetcore::patch::dv_xml(&[dv_named]);
    assert!(
        xml.contains("<formula1>DeptList</formula1>"),
        "named list must not be quoted: {xml}"
    );

    // Date bound `2026-01-01` must parse as date serial (46023), NOT 2026-1-1 = 2024
    let dv_date = DataValidation {
        ranges: vec![(2, 0, 2, 0)],
        kind: DvKind::Date,
        op: DvOp::GreaterEq,
        f1: "2026-01-01".into(),
        ..Default::default()
    };
    assert!(sheetcore::dv::validate_wb(
        &wb,
        0,
        &dv_date,
        &Value::Number(46023.0),
        2,
        0,
        |_, _, _| formula::FValue::Number(2024.0)
    ));
    assert!(!sheetcore::dv::validate_wb(
        &wb,
        0,
        &dv_date,
        &Value::Number(45000.0),
        2,
        0,
        |_, _, _| formula::FValue::Number(2024.0)
    ));
}

#[test]
fn p0_30_print_merged_cell_header_line_chart_and_paper_sizes() {
    // Paper size IDs match OOXML
    assert_eq!(paper_mm(5), (215.9, 355.6)); // Legal
    assert_eq!(paper_mm(6), (139.7, 215.9)); // Statement
    assert_eq!(paper_mm(11), (148.0, 210.0)); // A5

    // Header without &L/&C/&R renders centered instead of empty
    assert_eq!(header_parts("财务月报 &P/&N", 1, 3), ("".into(), "财务月报 1/3".into(), "".into()));

    // Merged cell spanning rows 0..=2 extends DOWNWARD (smaller y in PDF), never upward above row 0
    let mut wb = Workbook::new_blank();
    let fill_sid = wb.styles.intern(CellStyle {
        fill: Some([200, 220, 240]),
        ..Default::default()
    });
    let sh = &mut wb.sheets[0];
    sh.set(0, 0, Cell { value: Value::Text("合并".into()), formula: None });
    sh.set_style(0, 0, fill_sid);
    sh.merge_range(0, 0, 2, 0);
    sh.set(3, 0, Cell { value: Value::Text("下格".into()), formula: None });
    sh.set(5, 6, Cell { value: Value::Number(1.0), formula: None });
    sh.charts.push(Chart {
        kind: ChartKind::Line,
        title: "趋势".into(),
        series: vec![Series {
            name: "S".into(),
            vals: vec![10.0, 30.0, 20.0],
            ..Default::default()
        }],
        legend: false,
        anchor: (0, 2, 5, 6),
        legend_pos: String::new(),
        part: String::new(),
        drawing: String::new(),
        show_labels: false,
        label_opts: Default::default(),
        radar_style: String::new(),
        ref_fixes: Vec::new(),
        axis_x: String::new(),
        axis_y: String::new(),
        y_fmt: String::new(),
        y_min: None,
        y_max: None,
    });
    let layout = paginate(&wb.sheets[0], &wb.sheets[0].page);
    let out = page_ops(&wb, 0, &layout, 0);
    let merged_rect = out
        .rects
        .iter()
        .find(|(_, _, _, _, c)| *c == [200, 220, 240])
        .expect("merged fill rect");
    let (_, ph) = layout.page_size;
    let mt = wb.sheets[0].page.margins[2] * 72.0;
    let expected_top = ph - mt;
    assert!(
        (merged_rect.1 + merged_rect.3 - expected_top).abs() < 1.0,
        "merged top edge must align with row 0 top ({expected_top}), got {}+{}",
        merged_rect.1,
        merged_rect.3
    );
    assert!(
        out.polys.is_empty(),
        "Line chart must draw strokes in out.lines, not filled closed polygons in out.polys"
    );
    assert!(!out.lines.is_empty());
}

#[test]
fn p0_31_structural_shifts_merges_objects_page_rich_filter_and_tables() {
    let mut wb = Workbook::new_blank();
    let sh = &mut wb.sheets[0];
    sh.merge_range(2, 0, 5, 0); // deleting row 2 should shrink to (2,0,4,0), not vanish!
    sh.pictures.push(Picture {
        rect: (4, 1, 7, 3),
        off: [0; 4],
        part: "xl/media/image1.png".into(),
        data: None,
        ext: "png".into(),
        name: "P1".into(),
    });
    let mut tb = TextBox::default();
    tb.from = (1, 0, 4, 0);
    tb.to = (3, 0, 6, 0);
    sh.textboxes.push(tb);
    sh.charts.push(Chart {
        kind: ChartKind::Column,
        title: "C".into(),
        series: Vec::new(),
        legend: false,
        anchor: (4, 1, 8, 4),
        legend_pos: String::new(),
        part: String::new(),
        drawing: String::new(),
        show_labels: false,
        label_opts: Default::default(),
        radar_style: String::new(),
        ref_fixes: Vec::new(),
        axis_x: String::new(),
        axis_y: String::new(),
        y_fmt: String::new(),
        y_min: None,
        y_max: None,
    });
    sh.page.print_area = Some((1, 0, 10, 4));
    sh.page.title_rows = Some((0, 1));
    sh.page.row_breaks = vec![5];
    sh.rich.insert((4, 1), vec![RichRun::plain("hi")]);
    sh.ignored_errors.insert((4, 1));
    sh.filter = Some(Filter::new(1, 0, 10, 3));
    sh.tables.push(Table {
        name: "T1".into(),
        display: "T1".into(),
        range: (1, 0, 10, 2),
        columns: vec!["A".into(), "B".into(), "C".into()],
        column_ids: vec![1, 2, 3],
        header_row: true,
        totals_row: false,
    });

    apply_structural_wb(&mut wb, 0, &Structural::DeleteRows { at: 2, n: 1 });
    let sh = &wb.sheets[0];
    assert_eq!(sh.merges.as_slice(), &[(2, 0, 4, 0)], "merge shrinks on boundary row delete");
    assert_eq!(sh.pictures[0].rect, (3, 1, 6, 3));
    assert_eq!(sh.textboxes[0].from.2, 3);
    assert_eq!(sh.textboxes[0].to.2, 5);
    assert_eq!(sh.charts[0].anchor, (3, 1, 7, 4));
    assert_eq!(sh.page.print_area, Some((1, 0, 9, 4)));
    assert_eq!(sh.page.row_breaks, vec![4]);
    assert!(sh.rich.contains_key(&(3, 1)) && !sh.rich.contains_key(&(4, 1)));
    assert!(sh.ignored_errors.contains(&(3, 1)));
    assert_eq!(sh.filter.as_ref().unwrap().range, (1, 0, 9, 3));
    assert_eq!(sh.tables[0].range, (1, 0, 9, 2));

    apply_structural_wb(&mut wb, 0, &Structural::InsertCols { at: 1, n: 2 });
    let sh = &wb.sheets[0];
    assert_eq!(sh.pictures[0].rect, (3, 3, 6, 5));
    assert_eq!(sh.filter.as_ref().unwrap().range, (1, 0, 9, 5));
    assert_eq!(sh.tables[0].range, (1, 0, 9, 4));
    assert_eq!(sh.tables[0].columns.len(), 5);
}
