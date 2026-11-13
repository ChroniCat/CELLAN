//! C04：**所有**日期路径共用工作簿基准（1900 / 1904）。
//!
//! 需求原文要求日期函数、数字格式化、条件格式、数据验证、透视表日期分组、图表轴
//! 用同一个基准；另外跨簿复制必须按目标簿重算，不能在所有数值上加减 1462。
//! 这里逐条给行为断言，不用"文档说明"替代。

use sheetcore::cf::{self, CfBlock, CfKind, CfOp, CfRule, CfSheet, Dxf};
use sheetcore::dv::{self, DataValidation, DvKind, DvOp};
use sheetcore::formula::{self, with_date_system};
use sheetcore::model::{Cell, DateSystem, Sheet, Value, Workbook};
use sheetcore::pivot::{self, Agg, GroupBy, PivotSpec};
use sheetcore::style::{format_value_in, CellStyle};

fn book(sys: DateSystem) -> Workbook {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "S".into();
    wb.date_system = sys;
    wb
}

// ---------------------------------------------------------------------------
// 1. 日期函数 + 日期减法：1904 簿里 DATE(1904,1,1) 是 0，减出来是真实天数
// ---------------------------------------------------------------------------

#[test]
fn date_subtraction_and_function_results_use_the_1904_basis() {
    let mut wb = book(DateSystem::D1904);
    {
        let s = &mut wb.sheets[0];
        s.set(0, 0, Cell { value: Value::Empty, formula: Some("=DATE(1904,1,1)".into()) });
        s.set(0, 1, Cell { value: Value::Empty, formula: Some("=DATE(1904,1,11)".into()) });
        s.set(0, 2, Cell { value: Value::Empty, formula: Some("=B1-A1".into()) });
        s.set(0, 3, Cell { value: Value::Empty, formula: Some("=DATE(1904,1,2)-DATE(1904,1,1)".into()) });
    }
    formula::recalc_all(&mut wb);

    let s = &wb.sheets[0];
    assert_eq!(s.get(0, 0).unwrap().value, Value::Number(0.0), "1904-01-01 必须是 0");
    assert_eq!(s.get(0, 1).unwrap().value, Value::Number(10.0));
    assert_eq!(s.get(0, 2).unwrap().value, Value::Number(10.0), "日期减法按天差，不能带 1462");
    assert_eq!(s.get(0, 3).unwrap().value, Value::Number(1.0));
    // 同样的公式在 1900 簿里差 1462 天
    let mut w1900 = book(DateSystem::D1900);
    w1900.sheets[0]
        .set(0, 0, Cell { value: Value::Empty, formula: Some("=DATE(1904,1,1)".into()) });
    formula::recalc_all(&mut w1900);
    assert_eq!(w1900.sheets[0].get(0, 0).unwrap().value, Value::Number(1462.0));
}

// ---------------------------------------------------------------------------
// 2. 数字格式化：同一序列号在两个体系里显示不同日期
// ---------------------------------------------------------------------------

#[test]
fn number_formatting_follows_the_basis() {
    assert_eq!(format_value_in(DateSystem::D1904, &Value::Number(1.0), "yyyy-mm-dd"), "1904-01-02");
    assert_eq!(format_value_in(DateSystem::D1900, &Value::Number(1.0), "yyyy-mm-dd"), "1900-01-01");
    assert_eq!(format_value_in(DateSystem::D1904, &Value::Number(0.0), "yyyy-mm-dd"), "1904-01-01");
}

// ---------------------------------------------------------------------------
// 3. 条件格式：cellIs 的 DATE() 字面量必须按工作簿体系解析
// ---------------------------------------------------------------------------

fn cf_greater_than(lit: &str) -> CfSheet {
    CfSheet {
        blocks: vec![CfBlock {
            ranges: vec![(0, 0, 1, 0)], // A1:A2
            rules: vec![CfRule {
                kind: CfKind::CellIs {
                    op: CfOp::Greater,
                    a: lit.into(),
                    b: String::new(),
                    dxf: 0,
                },
                stop: false,
            }],
        }],
        dxfs: vec![Dxf { bold: true, ..Default::default() }],
    }
}

#[test]
fn conditional_format_date_literal_uses_the_workbook_basis() {
    let cf_sheet = cf_greater_than("DATE(1904,1,2)");
    let mut hits_1904 = Vec::new();
    let mut hits_1900 = Vec::new();
    for sys in [DateSystem::D1904, DateSystem::D1900] {
        let mut wb = book(sys);
        wb.sheets[0].set(0, 0, Cell { value: Value::Number(1.0), formula: None }); // 1904-01-02 / 1900-01-01
        wb.sheets[0].set(1, 0, Cell { value: Value::Number(2.0), formula: None }); // 1904-01-03 / 1900-01-02
        let stats = {
            let mut st = cf::CfStats::default();
            cf::compute_stats(&cf_sheet, &wb.sheets[0], &mut st);
            st
        };
        let (r1, r2) = with_date_system(sys, || {
            let a = !cf::overlay_at(&cf_sheet, &stats, &wb.sheets[0], 0, 0, |_, _, _| false).is_empty();
            let b = !cf::overlay_at(&cf_sheet, &stats, &wb.sheets[0], 1, 0, |_, _, _| false).is_empty();
            (a, b)
        });
        if sys == DateSystem::D1904 {
            hits_1904 = vec![r1, r2];
        } else {
            hits_1900 = vec![r1, r2];
        }
    }
    // 1904：DATE(1904,1,2)=1，所以 1 不大于 1（不命中）、2 命中
    assert_eq!(hits_1904, vec![false, true], "1904 簿的条件格式没按 1904 基准解析 DATE()");
    // 1900：同一个 DATE() 是 1463，两格都不命中
    assert_eq!(hits_1900, vec![false, false], "1900 簿不该命中");
}

// ---------------------------------------------------------------------------
// 4. 数据验证：between DATE() 上下界按体系解析
// ---------------------------------------------------------------------------

fn dv_between(a: &str, b: &str) -> DataValidation {
    DataValidation {
        ranges: vec![(0, 0, 5, 0)],
        kind: DvKind::Date,
        op: DvOp::Between,
        f1: a.into(),
        f2: b.into(),
        allow_blank: true,
        ..Default::default()
    }
}

#[test]
fn data_validation_date_bounds_use_the_workbook_basis() {
    let rule = dv_between("DATE(1904,1,1)", "DATE(1904,1,5)");
    let sheet = Sheet::new("S");
    let ok_1904 = with_date_system(DateSystem::D1904, || {
        (dv::validate(&rule, &Value::Number(3.0), &sheet, 0, 0, |_, _, _| sheetcore::formula::FValue::Bool(true)),
         dv::validate(&rule, &Value::Number(9.0), &sheet, 0, 0, |_, _, _| sheetcore::formula::FValue::Bool(true)))
    });
    assert_eq!(ok_1904, (true, false), "1904 簿：序列 3 在 1..4 之间应通过，9 应拒绝");
    let ok_1900 = with_date_system(DateSystem::D1900, || {
        (dv::validate(&rule, &Value::Number(3.0), &sheet, 0, 0, |_, _, _| sheetcore::formula::FValue::Bool(true)),
         dv::validate(&rule, &Value::Number(9.0), &sheet, 0, 0, |_, _, _| sheetcore::formula::FValue::Bool(true)))
    });
    assert_eq!(ok_1900, (false, false), "1900 簿：同一 DATE() 是 1462+，3/9 都该拒绝");
}

// ---------------------------------------------------------------------------
// 5. 透视表日期分组：按年分组要用同一基准
// ---------------------------------------------------------------------------

#[test]
fn pivot_year_grouping_uses_the_workbook_basis() {
    let mut wb = book(DateSystem::D1904);
    {
        let s = &mut wb.sheets[0];
        s.set(0, 0, Cell { value: Value::Text("日".into()), formula: None });
        s.set(0, 1, Cell { value: Value::Text("额".into()), formula: None });
        s.set(1, 0, Cell { value: Value::Number(1.0), formula: None }); // 1904-01-02
        s.set(1, 1, Cell { value: Value::Number(5.0), formula: None });
        s.set(2, 0, Cell { value: Value::Number(400.0), formula: None }); // 1905-02-04
        s.set(2, 1, Cell { value: Value::Number(7.0), formula: None });
    }
    let spec = PivotSpec {
        source: (0, 0, 2, 1),
        row_field: 0,
        col_field: None,
        value_field: 1,
        agg: Agg::Sum,
        group: GroupBy::Year,
        exclude: Vec::new(),
    };
    let res = with_date_system(DateSystem::D1904, || pivot::compute(&wb.sheets[0], &spec));
    // 末行是"总计"，只看分组行
    let labels: Vec<String> = res.rows.iter().map(|r| r[0].display()).collect();
    assert_eq!(&labels[..2], &["1904", "1905"], "1904 簿按年分组分错了：{labels:?}");
    assert_eq!(labels.last().map(String::as_str), Some("总计"));
}

// ---------------------------------------------------------------------------
// 6. 跨簿复制：ISO 日期按目标簿重算；普通数字原样
// ---------------------------------------------------------------------------

#[test]
fn xmlss_datetime_is_rebased_on_the_target_workbook() {
    use sheetcore::clipboard_xmlss::{iso_to_serial, parse};

    // Excel 1904 簿里复制 1904-01-02 → XMLSS 是 ISO 文本
    let xml = r#"<Workbook xmlns="urn:schemas-microsoft-com:office:spreadsheet"
        xmlns:ss="urn:schemas-microsoft-com:office:spreadsheet"><Worksheet><Table><Row>
        <Cell><Data ss:Type="DateTime">1904-01-02T00:00:00</Data></Cell>
        <Cell><Data ss:Type="Number">1</Data></Cell>
        </Row></Table></Worksheet></Workbook>"#;
    let b = parse(xml);
    assert_eq!(b.rows[0][0].kind, sheetcore::clipboard_xmlss::Kind::DateTime);
    let (serial_1904, ymd) = iso_to_serial(DateSystem::D1904, &b.rows[0][0].text).unwrap();
    assert_eq!((serial_1904, ymd), (1.0, (1904, 1, 2)));
    // 粘进 1900 簿：同一天要变成 1463，而不是照抄 1
    let (serial_1900, _) = iso_to_serial(DateSystem::D1900, &b.rows[0][0].text).unwrap();
    assert_eq!(serial_1900, 1463.0, "跨簿日期必须按目标体系重算");
    // 普通数字不参与换算
    assert_eq!(b.rows[0][1].kind, sheetcore::clipboard_xmlss::Kind::Number);
    assert!(iso_to_serial(DateSystem::D1904, &b.rows[0][1].text).is_none());
    assert_eq!(b.rows[0][1].text, "1");
}

/// XMLSS 的 DateTime 带时间：12:00 必须是 +0.5 天，不能被当成午夜。
#[test]
fn xmlss_datetime_keeps_the_time_of_day_in_both_systems() {
    use sheetcore::clipboard_xmlss::iso_to_serial;

    let (s1904, _) = iso_to_serial(DateSystem::D1904, "1904-01-02T12:00:00").unwrap();
    assert!((s1904 - 1.5).abs() < 1e-9, "1904 中午应为 1.5，实得 {s1904}");
    assert_eq!(
        format_value_in(DateSystem::D1904, &Value::Number(s1904), "yyyy-mm-dd hh:mm"),
        "1904-01-02 12:00"
    );

    let (s1900, _) = iso_to_serial(DateSystem::D1900, "1900-01-01T06:00:00").unwrap();
    assert!((s1900 - 1.25).abs() < 1e-9, "1900 早六点应为 1.25，实得 {s1900}");
    assert_eq!(
        format_value_in(DateSystem::D1900, &Value::Number(s1900), "yyyy-mm-dd hh:mm"),
        "1900-01-01 06:00"
    );

    // 带毫秒/不带秒也要能读
    let (ms, _) = iso_to_serial(DateSystem::D1904, "1904-01-02T12:00:00.000").unwrap();
    assert!((ms - 1.5).abs() < 1e-9);
    let (hm, _) = iso_to_serial(DateSystem::D1904, "1904-01-02 18:30").unwrap();
    assert!((hm - (1.0 + (18.5 / 24.0))).abs() < 1e-9);

    // 普通数字依旧不参与日期换算
    assert!(iso_to_serial(DateSystem::D1904, "1").is_none());
    assert!(iso_to_serial(DateSystem::D1904, "1.5").is_none());
}

/// 图表值轴：单元格带千分位/百分比/货币/日期格式时，系列点必须保留原始数值。
/// 旧实现把显示文本再 parse::<f64>()，"1,234.50" / "12.50%" / "$5.00" / "1904-01-02"
/// 全部失败 → 整条系列一个点都不剩。
#[test]
fn chart_values_keep_raw_numbers_when_cells_are_formatted() {
    use sheetcore::charts::{resolve_series_in, Chart, ChartKind, Series};

    let mut wb = book(DateSystem::D1904);
    let (s_num, s_pct, s_cur, s_date) = {
        let st = &mut wb.styles;
        (
            st.intern(CellStyle { format: "#,##0.00".into(), ..Default::default() }),
            st.intern(CellStyle { format: "0.00%".into(), ..Default::default() }),
            st.intern(CellStyle { format: "$#,##0.00".into(), ..Default::default() }),
            st.intern(CellStyle { format: "yyyy-mm-dd".into(), ..Default::default() }),
        )
    };
    {
        let s = &mut wb.sheets[0];
        let names = ["甲", "乙", "丙", "丁"];
        for (i, n) in names.iter().enumerate() {
            s.set(i as u32, 0, Cell { value: Value::Text((*n).into()), formula: None });
        }
        s.set(0, 1, Cell { value: Value::Number(1234.5), formula: None });
        s.set(1, 1, Cell { value: Value::Number(0.125), formula: None });
        s.set(2, 1, Cell { value: Value::Number(5.0), formula: None });
        s.set(3, 1, Cell { value: Value::Number(1.0), formula: None });
        for (r, sid) in [(0u32, s_num), (1, s_pct), (2, s_cur), (3, s_date)] {
            s.set_style(r, 1, sid);
        }
    }
    let mut chart = Chart {
        kind: ChartKind::Column,
        title: "t".into(),
        series: vec![Series {
            name: String::new(),
            cats: Vec::new(),
            vals: Vec::new(),
            name_ref: String::new(),
            cat_ref: "S!$A$1:$A$4".into(),
            val_ref: "S!$B$1:$B$4".into(),
            color: None,
        }],
        legend: false,
        anchor: (0, 0, 5, 5),
        legend_pos: String::new(),
        part: String::new(),
        drawing: String::new(),
        show_labels: false,
        ref_fixes: Vec::new(),
        axis_x: String::new(),
        axis_y: String::new(),
        y_min: None,
        y_max: None,
    };
    resolve_series_in(&mut chart, &wb.sheets[0], Some(&wb.styles), DateSystem::D1904);
    let s = &chart.series[0];
    assert_eq!(s.vals, vec![1234.5, 0.125, 5.0, 1.0], "格式化单元格的值被吞了：{:?}", s.vals);
    assert_eq!(s.vals.len(), 4, "系列点数不能少");
    // 分类轴仍然是显示文本
    assert_eq!(s.cats, vec!["甲", "乙", "丙", "丁"]);
}
