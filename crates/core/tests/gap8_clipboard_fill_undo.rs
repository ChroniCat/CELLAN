use sheetcore::clipboard_xmlss::{iso_to_serial, parse, serial_to_iso};
use sheetcore::dv::{DataValidation, DvKind, DvOp, DvStyle};
use sheetcore::flashfill::{auto_flash_fill, flash_fill};
use sheetcore::hyperlinks::Hyperlink;
use sheetcore::model::{Cell, DateSystem, Sheet, Value, Workbook};
use sheetcore::sparkline::Sparkline;
use sheetcore::sync::SheetMetaKind;
use sheetcore::undo::{MetaEdit, UndoStack};

#[test]
fn xmlss_preserves_four_side_borders_alignment_wrap_and_subsecond_time() {
    // Sub-second timestamp roundtrip
    let (serial, ymd) = iso_to_serial(DateSystem::D1900, "2026-03-15T14:30:45.250").unwrap();
    assert_eq!(ymd, (2026, 3, 15));
    let iso = serial_to_iso(DateSystem::D1900, serial).unwrap();
    assert_eq!(iso, "2026-03-15T14:30:45.250");

    // 4-side independent borders + valign + wrap + underline + strike
    let xml = r##"<?xml version="1.0"?>
<Workbook xmlns="urn:schemas-microsoft-com:office:spreadsheet" xmlns:ss="urn:schemas-microsoft-com:office:spreadsheet">
<Styles>
  <Style ss:ID="s1">
    <Alignment ss:Horizontal="Right" ss:Vertical="Top" ss:WrapText="1"/>
    <Borders>
      <Border ss:Position="Top" ss:LineStyle="Continuous" ss:Weight="1" ss:Color="#FF0000"/>
      <Border ss:Position="Bottom" ss:LineStyle="Continuous" ss:Weight="2" ss:Color="#0000FF"/>
      <Border ss:Position="Left" ss:LineStyle="None"/>
    </Borders>
    <Font ss:FontName="微软雅黑" ss:Size="12" ss:Bold="1" ss:Underline="Single" ss:StrikeThrough="1"/>
  </Style>
</Styles>
<Worksheet ss:Name="Sheet1"><Table>
  <Row><Cell ss:StyleID="s1"><Data ss:Type="String">Hello</Data></Cell></Row>
</Table></Worksheet></Workbook>"##;
    let block = parse(xml);
    let st = &block.rows[0][0].style;
    assert_eq!(st.halign, 3);
    assert_eq!(st.valign, 2);
    assert!(st.wrap);
    assert!(st.underline);
    assert!(st.strike);
    assert_eq!(st.borders[0], Some([255, 0, 0])); // Top
    assert_eq!(st.borders[1], Some([0, 0, 255])); // Bottom
    assert_eq!(st.borders[2], None); // Left was None
    assert_eq!(st.borders[3], None); // Right omitted
}

#[test]
fn flashfill_handles_brackets_nth_token_slice_mask_template_and_auto_multi_col() {
    // 1) Between Chinese brackets 【...】
    let mut sh = Sheet::new("S");
    sh.set(0, 0, Cell { value: Value::Text("项目【华东区】报销".into()), formula: None });
    sh.set(0, 1, Cell { value: Value::Text("华东区".into()), formula: None });
    sh.set(1, 0, Cell { value: Value::Text("项目【华南区】差旅".into()), formula: None });
    let out = flash_fill(&mut sh, 0, 1, 0, 1, None).unwrap();
    assert_eq!(out.filled, 1);
    assert_eq!(sh.value(1, 1).display(), "华南区");

    // 2) Fixed slice (ID birthday extraction)
    let mut sh2 = Sheet::new("S");
    sh2.set(0, 0, Cell { value: Value::Text("11010119900512001X".into()), formula: None });
    sh2.set(0, 1, Cell { value: Value::Text("19900512".into()), formula: None });
    sh2.set(1, 0, Cell { value: Value::Text("310101198811230025".into()), formula: None });
    flash_fill(&mut sh2, 0, 1, 0, 1, None).unwrap();
    assert_eq!(sh2.value(1, 1).display(), "19881123");

    // 3) Auto multi-col template with header row skipped cleanly
    let mut sh3 = Sheet::new("S");
    sh3.set(0, 0, Cell { value: Value::Text("姓".into()), formula: None });
    sh3.set(0, 1, Cell { value: Value::Text("名".into()), formula: None });
    sh3.set(0, 2, Cell { value: Value::Text("部门".into()), formula: None });
    sh3.set(0, 3, Cell { value: Value::Text("标签".into()), formula: None });
    sh3.set(1, 0, Cell { value: Value::Text("王".into()), formula: None });
    sh3.set(1, 1, Cell { value: Value::Text("敏".into()), formula: None });
    sh3.set(1, 2, Cell { value: Value::Text("审计".into()), formula: None });
    sh3.set(1, 3, Cell { value: Value::Text("王敏-审计".into()), formula: None });
    sh3.set(2, 0, Cell { value: Value::Text("陈".into()), formula: None });
    sh3.set(2, 1, Cell { value: Value::Text("刚".into()), formula: None });
    sh3.set(2, 2, Cell { value: Value::Text("税务".into()), formula: None });
    let (res, coords) = auto_flash_fill(&mut sh3, 3, 2).unwrap();
    assert_eq!(res.filled, 1);
    assert_eq!(coords, vec![(2, 3)]);
    assert_eq!(sh3.value(2, 3).display(), "陈刚-税务");
}

#[test]
fn unified_meta_undo_covers_dv_hyperlinks_sparklines_names_and_emits_collab_sync() {
    let mut wb = Workbook::new_blank();
    let mut undo = UndoStack::new();

    // 1) DV + Hyperlink + Sparkline + Defined Names + ColWidth in MetaEdit
    let dv_rule = DataValidation {
        ranges: vec![(0, 0, 5, 0)],
        kind: DvKind::List,
        op: DvOp::Between,
        f1: "\"A,B\"".into(),
        f2: String::new(),
        allow_blank: true,
        show_dropdown: true,
        error_style: DvStyle::Stop,
        show_error: true,
        show_prompt: false,
        error_title: String::new(),
        error_msg: String::new(),
        prompt_title: String::new(),
        prompt_msg: String::new(),
    };
    let link = Hyperlink {
        target: "https://example.com".into(),
        location: String::new(),
        tooltip: "官网".into(),
        external: true,
        span: (0, 0),
        attributes: Vec::new(),
    };
    let spark = Sparkline {
        r: 0,
        c: 2,
        rect: (0, 0, 0, 1),
        ..Default::default()
    };
    wb.sheet_mut(0).dv.push(dv_rule.clone());
    wb.sheet_mut(0).hyperlinks.insert((0, 1), link.clone());
    wb.sheet_mut(0).sparklines.push(spark.clone());
    wb.sheet_mut(0).cols.set(0, 140.0);
    wb.names.push(("TaxRate".into(), None, "=0.13".into()));

    undo.push_meta(
        "元数据综合编辑",
        (0, 0, 0),
        MetaEdit {
            sheet: 0,
            col_widths: vec![(0, Some(72.0), Some(140.0))],
            dv: Some((Vec::new(), vec![dv_rule.clone()])),
            hyperlinks: vec![((0, 1), None, Some(link.clone()))],
            sparklines: Some((Vec::new(), vec![spark.clone()])),
            names: Some((Vec::new(), vec![("TaxRate".into(), None, "=0.13".into())])),
            ..Default::default()
        },
    );

    // Undo restores all 5 meta kinds and produces collab sync payloads
    let applied = undo.undo_workbook(&mut wb, |_, _, _, _| true).unwrap();
    assert_eq!(applied.label, "元数据综合编辑");
    assert!(wb.sheet(0).dv.is_empty());
    assert!(wb.sheet(0).hyperlinks.is_empty());
    assert!(wb.sheet(0).sparklines.is_empty());
    assert!(wb.names.is_empty());
    assert!((wb.sheet(0).cols.size(0) - 72.0).abs() < 0.01);
    assert!(undo.take_needs_recalc());
    let syncs = undo.take_meta_sync();
    assert!(syncs.iter().any(|(_, k)| matches!(k, SheetMetaKind::ColWidths(_))));
    assert!(syncs.iter().any(|(_, k)| matches!(k, SheetMetaKind::Hyperlink { target: None, .. })));

    // Redo reapplies all 5
    undo.redo_workbook(&mut wb, |_, _, _, _| true).unwrap();
    assert_eq!(wb.sheet(0).dv.len(), 1);
    assert_eq!(wb.sheet(0).hyperlinks.get(&(0, 1)).unwrap().target, "https://example.com");
    assert_eq!(wb.sheet(0).sparklines.len(), 1);
    assert_eq!(wb.names.len(), 1);
    assert!((wb.sheet(0).cols.size(0) - 140.0).abs() < 0.01);
    assert!(undo.take_needs_recalc());
    let syncs_redo = undo.take_meta_sync();
    assert!(syncs_redo.iter().any(|(_, k)| matches!(k, SheetMetaKind::Hyperlink { target: Some(u), .. } if u == "https://example.com")));
}
