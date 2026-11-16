//! QA-01 特性矩阵夹具：样式 / 条件格式 / 数据验证 / 表格 / 命名区域 / 隐藏 / 冻结。
//!
//! 只写新文件。产物交给 `scripts/qa01_features_probe.ps1` 在 Excel 与 WPS 里逐项读取。
//!
//! 用法：`cargo run -p sheetcore --example qa01_features -- <out.xlsx>`

use sheetcore::cf::{CfBlock, CfKind, CfOp, CfRule, Cfvo, Dxf};
use sheetcore::dv::{DataValidation, DvKind, DvOp, DvStyle};
use sheetcore::{Cell, Value, Workbook, xlsx};

fn text(s: &str) -> Cell {
    Cell { value: Value::Text(s.into()), formula: None }
}

fn num(v: f64) -> Cell {
    Cell { value: Value::Number(v), formula: None }
}

fn main() {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "work/probe/qa01/features.xlsx".into());
    let path = std::path::PathBuf::from(&out);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok();
    }

    let want = std::env::var("SC_FEAT").unwrap_or_else(|_| "styles,cf,dv,table,names,freeze,hidden".into());
    let on = |k: &str| want.split(',').any(|x| x.trim() == k);
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Feat".into();

    // ---- 数据区 ----
    let headers = ["名称", "金额", "类别", "整数"];
    for (i, h) in headers.iter().enumerate() {
        wb.sheets[0].set(0, i as u32, text(h));
    }
    let rows = [
        ("甲", 10.0, "A", 5.0),
        ("乙", 25.0, "B", 15.0),
        ("丙", 7.5, "A", 50.0),
        ("丁", 40.0, "C", 99.0),
    ];
    for (i, (n, amount, cat, int)) in rows.iter().enumerate() {
        let r = i as u32 + 1;
        wb.sheets[0].set(r, 0, text(n));
        wb.sheets[0].set(r, 1, num(*amount));
        wb.sheets[0].set(r, 2, text(cat));
        wb.sheets[0].set(r, 3, num(*int));
    }

    // ---- 样式：表头加粗 + 底色；金额列数字格式 + 边框 ----
    if on("styles") {
    let head = wb.styles.intern(sheetcore::style::CellStyle {
        bold: true,
        fill: Some([221, 235, 247]),
        ..Default::default()
    });
    for c in 0..4u32 {
        wb.sheets[0].set_style(0, c, head);
    }
    let mut border = [sheetcore::style::Border::default(); 4];
    border[sheetcore::style::BorderSide::Bottom as usize] = sheetcore::style::Border {
        style: 1, // thin
        color: [192, 0, 0],
    };
    let money = wb.styles.intern(sheetcore::style::CellStyle {
        format: "#,##0.00".into(),
        border,
        ..Default::default()
    });
    for r in 1..5u32 {
        wb.sheets[0].set_style(r, 1, money);
    }
    }

    // ---- 条件格式：金额>20 红底；金额列色阶 ----
    if on("cf") {
        let sheet = &mut wb.sheets[0];
        sheet.cf.dxfs.push(Dxf {
            fill: Some([255, 199, 206]),
            ..Default::default()
        });
        sheet.cf.dxfs.push(Dxf {
            bold: true,
            ..Default::default()
        });
        sheet.cf.blocks.push(CfBlock {
            ranges: vec![(1, 1, 4, 1)],
            rules: vec![CfRule {
                kind: CfKind::CellIs {
                    op: CfOp::Greater,
                    a: "20".into(),
                    b: String::new(),
                    dxf: 0,
                },
                stop: false,
                priority: 1,
            }],
        });
        sheet.cf.blocks.push(CfBlock {
            ranges: vec![(1, 1, 4, 1)],
            rules: vec![CfRule {
                kind: CfKind::Scale {
                    points: vec![
                        (Cfvo::min(), [99, 190, 123]),
                        (Cfvo::new(sheetcore::cf::CfvoKind::Percent, 50.0), [255, 235, 132]),
                        (Cfvo::max(), [248, 105, 107]),
                    ],
                },
                stop: false,
                priority: 2,
            }],
        });
        sheet.cf_dirty = true;
    }

    // ---- 数据验证：类别下拉、整数 1..100 ----
    if on("dv") {
        let sheet = &mut wb.sheets[0];
        sheet.dv.push(DataValidation {
            ranges: vec![(1, 2, 4, 2)],
            kind: DvKind::List,
            f1: "\"A,B,C\"".into(),
            error_style: DvStyle::Stop,
            error_title: "类别".into(),
            error_msg: "只能选 A/B/C".into(),
            ..Default::default()
        });
        sheet.dv.push(DataValidation {
            ranges: vec![(1, 3, 4, 3)],
            kind: DvKind::Whole,
            op: DvOp::Between,
            f1: "1".into(),
            f2: "100".into(),
            error_style: DvStyle::Stop,
            error_title: "整数".into(),
            error_msg: "1-100".into(),
            ..Default::default()
        });
    }

    // ---- 表格对象（A1:B5）----
    if on("table") {
    wb.sheets[0].tables.push(sheetcore::table::Table {
        name: "T1".into(),
        display: "T1".into(),
        range: (0, 0, 4, 1),
        columns: vec!["名称".into(), "金额".into()],
        column_ids: Vec::new(),
        header_row: true,
        totals_row: false,
    });
    wb.sheets[0].tables_dirty = true;
    }

    // ---- 命名区域 ----
    if on("names") {
    wb.names.push(("MyRange".into(), None, "Feat!$A$2:$A$5".into()));
    wb.names_dirty = true;
    }

    // ---- 冻结窗格 + 隐藏行/列 ----
    if on("freeze") {
    wb.sheets[0].freeze = Some((1, 1));
    wb.sheets[0].freeze_dirty = true;
    }
    if on("hidden") {
    wb.sheets[0].hidden_rows_user.insert(6);
    wb.sheets[0].hidden_cols.insert(4);
    wb.sheets[0].layout_dirty = true;
    }

    xlsx::save(&path, &wb).expect("保存夹具失败");
    println!("saved {} (1 sheet, table+cf+dv+names+freeze+hidden)", path.display());
}
