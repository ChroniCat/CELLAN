//! 开始页的内置专业模板：全部在内存里实时构建，不依赖外部磁盘文件。
//!
//! 每套模板均按商用财务/经营看板标准设计（标题横幅、KPI 汇总指标卡、冻结深色表头、
//! 斑马纹宽敞行高、条件格式数据条与状态着色、跨表 SUMIF 联动公式），开箱即可直接修改投用。

use sheetcore::cf::{CfBlock, CfKind, CfOp, CfRule, Cfvo, CfvoKind, Dxf};
use sheetcore::model::{Cell, Sheet, Value, Workbook};
use sheetcore::style::{Border, CellStyle};
use std::sync::OnceLock;

pub struct Template {
    pub name: &'static str,
    pub hint: &'static str,
    pub build: fn() -> Workbook,
}

pub const ALL: [Template; 4] = [
    Template {
        name: "空白工作簿",
        hint: "从干净的工作表开始自由建模与内网协同",
        build: blank,
    },
    Template {
        name: "销售回款台账",
        hint: "含税合同、回款进度条与跨表产品线汇总",
        build: sales,
    },
    Template {
        name: "部门经营预算",
        hint: "科目预算、实际执行率数据条与超支预警",
        build: budget,
    },
    Template {
        name: "项目里程碑排期",
        hint: "WBS 任务分解、工期核算与甘特进度跟踪",
        build: schedule,
    },
];

pub fn build(idx: usize) -> Workbook {
    (ALL.get(idx).map(|t| t.build).unwrap_or(blank))()
}

// ---------------------------------------------------------------- 样式辅助

fn thin(rgb: [u8; 3]) -> Border {
    Border {
        style: 1,
        color: rgb,
    }
}

fn med(rgb: [u8; 3]) -> Border {
    Border {
        style: 2,
        color: rgb,
    }
}

fn style(wb: &mut Workbook, s: CellStyle) -> u16 {
    wb.styles.intern(s)
}

fn banner_title_style(wb: &mut Workbook, fg: [u8; 3], bg: [u8; 3], accent: [u8; 3]) -> u16 {
    style(
        wb,
        CellStyle {
            bold: true,
            size: 15.0,
            color: fg,
            fill: Some(bg),
            halign: 1,
            valign: 2,
            border: [Border::default(), Border::default(), Border::default(), med(accent)],
            ..Default::default()
        },
    )
}

fn banner_sub_style(wb: &mut Workbook) -> u16 {
    style(
        wb,
        CellStyle {
            size: 9.5,
            color: [100, 116, 139],
            fill: Some([248, 250, 252]),
            halign: 1,
            valign: 2,
            border: [
                Border::default(),
                Border::default(),
                Border::default(),
                thin([226, 232, 240]),
            ],
            ..Default::default()
        },
    )
}

fn kpi_label_style(wb: &mut Workbook, bg: [u8; 3]) -> u16 {
    let b = thin([214, 222, 234]);
    style(
        wb,
        CellStyle {
            size: 9.5,
            color: [71, 85, 105],
            fill: Some(bg),
            halign: 1,
            valign: 2,
            border: [b, b, b, Border::default()],
            ..Default::default()
        },
    )
}

fn kpi_val_style(wb: &mut Workbook, fmt: &str, fg: [u8; 3], bg: [u8; 3]) -> u16 {
    let b = thin([214, 222, 234]);
    style(
        wb,
        CellStyle {
            bold: true,
            size: 13.0,
            color: fg,
            fill: Some(bg),
            format: fmt.to_string(),
            halign: 1,
            valign: 2,
            border: [b, b, Border::default(), b],
            ..Default::default()
        },
    )
}

fn table_head_style(wb: &mut Workbook, bg: [u8; 3], halign: u8) -> u16 {
    let side = thin(bg);
    style(
        wb,
        CellStyle {
            bold: true,
            size: 10.5,
            color: [255, 255, 255],
            fill: Some(bg),
            halign,
            valign: 2,
            border: [side, side, side, med(bg)],
            ..Default::default()
        },
    )
}

fn row_cell_style(wb: &mut Workbook, fmt: &str, halign: u8, zebra: bool, fg: [u8; 3]) -> u16 {
    let grid = thin([224, 230, 238]);
    style(
        wb,
        CellStyle {
            size: 10.5,
            color: fg,
            fill: if zebra { Some([248, 250, 252]) } else { None },
            format: fmt.to_string(),
            halign,
            valign: 2,
            border: [grid; 4],
            ..Default::default()
        },
    )
}

fn total_row_style(wb: &mut Workbook, fmt: &str, halign: u8, accent: [u8; 3], bg: [u8; 3]) -> u16 {
    let side = thin([214, 222, 234]);
    style(
        wb,
        CellStyle {
            bold: true,
            size: 10.5,
            color: [15, 23, 42],
            fill: Some(bg),
            format: fmt.to_string(),
            halign,
            valign: 2,
            border: [side, side, med(accent), med(accent)],
            ..Default::default()
        },
    )
}

// ---------------------------------------------------------------- 写入辅助

fn text(sh: &mut Sheet, r: u32, c: u32, s: &str) {
    sh.set(
        r,
        c,
        Cell {
            value: Value::Text(s.into()),
            formula: None,
        },
    );
}

fn num(sh: &mut Sheet, r: u32, c: u32, v: f64) {
    sh.set(
        r,
        c,
        Cell {
            value: Value::Number(v),
            formula: None,
        },
    );
}

fn formula(sh: &mut Sheet, r: u32, c: u32, f: &str) {
    sh.set(
        r,
        c,
        Cell {
            value: Value::Empty,
            formula: Some(f.into()),
        },
    );
}

fn widths(sh: &mut Sheet, px: &[f32]) {
    for (c, w) in px.iter().enumerate() {
        sh.cols.set(c as u32, *w);
    }
}

fn style_range(sh: &mut Sheet, r1: u32, c1: u32, r2: u32, c2: u32, sid: u16) {
    for r in r1..=r2 {
        for c in c1..=c2 {
            sh.set_style(r, c, sid);
        }
    }
}

/// 添加文本包含型状态胶囊条件格式规则
fn push_status_cf(
    sh: &mut Sheet,
    range: (u32, u32, u32, u32),
    items: &[(&str, [u8; 3], [u8; 3])],
) {
    let mut rules = Vec::new();
    for (idx, (kw, fg, bg)) in items.iter().enumerate() {
        let dxf_idx = sh.cf.dxfs.len();
        sh.cf.dxfs.push(Dxf {
            bold: true,
            color: Some(*fg),
            fill: Some(*bg),
            ..Default::default()
        });
        rules.push(CfRule {
            kind: CfKind::Text {
                contains: (*kw).to_string(),
                dxf: dxf_idx,
            },
            stop: false,
            priority: (idx + 1) as u32,
        });
    }
    sh.cf.blocks.push(CfBlock {
        ranges: vec![range],
        rules,
    });
}

/// Excel 1900 日期系统：1899-12-30 为 0。
fn date_serial(y: i32, m: u32, d: u32) -> f64 {
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = ((153 * mp + 2) / 5 + d - 1) as i32;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    (days + 25_569) as f64
}

// ---------------------------------------------------------------- 0. 空白工作簿

fn blank() -> Workbook {
    Workbook::new_blank()
}

// ---------------------------------------------------------------- 1. 销售回款台账（双工作表联动）

fn sales() -> Workbook {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "订单回款明细".into();
    wb.sheets.push(Sheet::new("产品线汇总"));

    let navy = [27, 53, 129];
    let s_title = banner_title_style(&mut wb, navy, [238, 242, 255], navy);
    let s_sub = banner_sub_style(&mut wb);
    let s_kpi_l = kpi_label_style(&mut wb, [248, 250, 255]);
    let s_kpi_money = kpi_val_style(&mut wb, "¥#,##0.00", navy, [248, 250, 255]);
    let s_kpi_ok = kpi_val_style(&mut wb, "¥#,##0.00", [4, 120, 87], [240, 253, 244]);
    let s_kpi_warn = kpi_val_style(&mut wb, "¥#,##0.00", [180, 83, 9], [255, 251, 235]);
    let s_kpi_pct = kpi_val_style(&mut wb, "0.0%", navy, [238, 242, 255]);

    let h_left = table_head_style(&mut wb, navy, 1);
    let h_center = table_head_style(&mut wb, navy, 2);
    let h_right = table_head_style(&mut wb, navy, 3);

    let mut row_styles = Vec::new();
    for zebra in [false, true] {
        row_styles.push((
            row_cell_style(&mut wb, "", 2, zebra, [30, 41, 59]),
            row_cell_style(&mut wb, "yyyy-mm-dd", 2, zebra, [71, 85, 105]),
            row_cell_style(&mut wb, "", 1, zebra, [15, 23, 42]),
            row_cell_style(&mut wb, "#,##0", 3, zebra, [30, 41, 59]),
            row_cell_style(&mut wb, "#,##0.00", 3, zebra, [15, 23, 42]),
            row_cell_style(&mut wb, "#,##0.00;[Red]-#,##0.00", 3, zebra, [51, 65, 85]),
            row_cell_style(&mut wb, "0.0%", 3, zebra, [30, 41, 59]),
        ));
    }
    let s_tot_lbl = total_row_style(&mut wb, "", 1, navy, [238, 242, 255]);
    let s_tot_cnt = total_row_style(&mut wb, "#,##0", 3, navy, [238, 242, 255]);
    let s_tot_amt = total_row_style(&mut wb, "¥#,##0.00", 3, navy, [238, 242, 255]);
    let s_tot_pct = total_row_style(&mut wb, "0.0%", 3, navy, [238, 242, 255]);

    {
        let sh = &mut wb.sheets[0];
        widths(
            sh,
            &[118.0, 100.0, 178.0, 132.0, 68.0, 98.0, 118.0, 118.0, 112.0, 92.0],
        );
        sh.rows.set(0, 36.0);
        sh.rows.set(1, 22.0);
        sh.rows.set(2, 20.0);
        sh.rows.set(3, 30.0);
        sh.rows.set(4, 10.0);
        sh.rows.set(5, 28.0);

        sh.merge_range(0, 0, 0, 9);
        text(sh, 0, 0, "2026 年度大客户销售合同与回款跟踪台账");
        style_range(sh, 0, 0, 0, 9, s_title);

        sh.merge_range(1, 0, 1, 9);
        text(
            sh,
            1,
            0,
            "统计口径：含税合同额（13% 增值税） · 币种：CNY · 任意修改数量/单价/已回款即可自动重算全部看板与跨表汇总",
        );
        style_range(sh, 1, 0, 1, 9, s_sub);

        // 4 组顶部 KPI 指标卡（合并区域值落在左上锚点格 A4 / D4 / F4 / I4）
        for &(c1, c2, lbl, fml, vs) in &[
            (0u32, 2u32, "签约合同总额", "=SUM(G7:G18)", s_kpi_money),
            (3, 4, "累计已回款额", "=SUM(H7:H18)", s_kpi_ok),
            (5, 7, "应收未收尾款", "=SUM(I7:I18)", s_kpi_warn),
            (8, 9, "整体回款达成率", "=D4/A4", s_kpi_pct),
        ] {
            sh.merge_range(2, c1, 2, c2);
            sh.merge_range(3, c1, 3, c2);
            text(sh, 2, c1, lbl);
            formula(sh, 3, c1, fml);
            style_range(sh, 2, c1, 2, c2, s_kpi_l);
            style_range(sh, 3, c1, 3, c2, vs);
        }

        let headers = [
            ("合同编号", h_center),
            ("签约日期", h_center),
            ("客户名称", h_left),
            ("产品线", h_left),
            ("数量", h_right),
            ("含税单价", h_right),
            ("合同总额", h_right),
            ("已回款", h_right),
            ("应收尾款", h_right),
            ("回款状态", h_center),
        ];
        for (c, (h, hs)) in headers.iter().enumerate() {
            text(sh, 5, c as u32, h);
            sh.set_style(5, c as u32, *hs);
        }

        let orders: &[(&str, (i32, u32, u32), &str, &str, f64, f64, f64, &str)] = &[
            ("CT-2026-101", (2026, 10, 8), "华东数智制造集团", "企业协同底座", 15.0, 12800.0, 192000.0, "已结清"),
            ("CT-2026-102", (2026, 10, 12), "鼎新半导体（上海）", "无锁计算引擎", 4.0, 48500.0, 194000.0, "已结清"),
            ("CT-2026-103", (2026, 10, 16), "南方智慧港务股份", "数据治理实施", 1.0, 168000.0, 117600.0, "部分回款"),
            ("CT-2026-104", (2026, 10, 21), "北京远景能源科技", "企业协同底座", 25.0, 9600.0, 240000.0, "已结清"),
            ("CT-2026-105", (2026, 10, 25), "广州海联精密电子", "年度原厂维保", 2.0, 36000.0, 72000.0, "已结清"),
            ("CT-2026-106", (2026, 11, 2), "成都锦程轨交装备", "无锁计算引擎", 3.0, 52000.0, 78000.0, "部分回款"),
            ("CT-2026-107", (2026, 11, 5), "杭州云图金融科技", "企业协同底座", 30.0, 11200.0, 336000.0, "已结清"),
            ("CT-2026-108", (2026, 11, 9), "苏州精工光刻检测", "数据治理实施", 1.0, 145000.0, 0.0, "账期跟进"),
            ("CT-2026-109", (2026, 11, 12), "深圳前海跨境清算", "无锁计算引擎", 6.0, 46000.0, 220800.0, "部分回款"),
            ("CT-2026-110", (2026, 11, 14), "武汉光谷生物医药", "年度原厂维保", 3.0, 28000.0, 84000.0, "已结清"),
            ("CT-2026-111", (2026, 11, 17), "宁波远洋供应链", "企业协同底座", 18.0, 10500.0, 94500.0, "部分回款"),
            ("CT-2026-112", (2026, 11, 19), "合肥量子精密仪器", "数据治理实施", 1.0, 126000.0, 0.0, "账期跟进"),
        ];

        for (i, (code, dt, cust, line, q, price, paid, st)) in orders.iter().enumerate() {
            let r = 6 + i as u32;
            let er = r + 1;
            sh.rows.set(r, 24.0);
            let (sc_code, sc_date, sc_txt, sc_qty, sc_amt, sc_bal, _) = row_styles[i % 2];
            text(sh, r, 0, code);
            num(sh, r, 1, date_serial(dt.0, dt.1, dt.2));
            text(sh, r, 2, cust);
            text(sh, r, 3, line);
            num(sh, r, 4, *q);
            num(sh, r, 5, *price);
            formula(sh, r, 6, &format!("=E{er}*F{er}"));
            num(sh, r, 7, *paid);
            formula(sh, r, 8, &format!("=G{er}-H{er}"));
            text(sh, r, 9, st);

            sh.set_style(r, 0, sc_code);
            sh.set_style(r, 1, sc_date);
            sh.set_style(r, 2, sc_txt);
            sh.set_style(r, 3, sc_txt);
            sh.set_style(r, 4, sc_qty);
            sh.set_style(r, 5, sc_amt);
            sh.set_style(r, 6, sc_amt);
            sh.set_style(r, 7, sc_amt);
            sh.set_style(r, 8, sc_bal);
            sh.set_style(r, 9, sc_code);
        }

        let tr = 18u32;
        sh.rows.set(tr, 28.0);
        text(sh, tr, 0, "合计");
        text(sh, tr, 2, "共 12 笔签约合同");
        formula(sh, tr, 4, "=SUM(E7:E18)");
        formula(sh, tr, 6, "=SUM(G7:G18)");
        formula(sh, tr, 7, "=SUM(H7:H18)");
        formula(sh, tr, 8, "=SUM(I7:I18)");
        formula(sh, tr, 9, "=H19/G19");
        for c in [0, 1, 2, 3] {
            sh.set_style(tr, c, s_tot_lbl);
        }
        sh.set_style(tr, 4, s_tot_cnt);
        sh.set_style(tr, 5, s_tot_lbl);
        sh.set_style(tr, 6, s_tot_amt);
        sh.set_style(tr, 7, s_tot_amt);
        sh.set_style(tr, 8, s_tot_amt);
        sh.set_style(tr, 9, s_tot_pct);

        // 合同总额蓝色数据条 + 状态列语义胶囊着色
        sh.cf.blocks.push(CfBlock {
            ranges: vec![(6, 6, 17, 6)],
            rules: vec![CfRule {
                kind: CfKind::Bar {
                    color: [99, 132, 246],
                    min: Cfvo::new(CfvoKind::Num, 0.0),
                    max: Cfvo::max(),
                },
                stop: false,
                priority: 1,
            }],
        });
        push_status_cf(
            sh,
            (6, 9, 17, 9),
            &[
                ("已结清", [4, 120, 87], [236, 253, 245]),
                ("部分回款", [180, 83, 9], [255, 251, 235]),
                ("账期跟进", [185, 28, 28], [254, 242, 242]),
            ],
        );
        sh.freeze = Some((6, 0));
    }

    // Sheet 2：跨表联动产品线经营汇总
    {
        let sh = &mut wb.sheets[1];
        widths(sh, &[156.0, 136.0, 136.0, 136.0, 108.0, 108.0]);
        sh.rows.set(0, 36.0);
        sh.rows.set(1, 22.0);
        sh.rows.set(2, 10.0);
        sh.rows.set(3, 28.0);
        sh.merge_range(0, 0, 0, 5);
        text(sh, 0, 0, "各产品线签约与回款跨表汇总（自动联动）");
        style_range(sh, 0, 0, 0, 5, s_title);
        sh.merge_range(1, 0, 1, 5);
        text(sh, 1, 0, "本表通过 SUMIF 实时汇总「订单回款明细」工作表数据");
        style_range(sh, 1, 0, 1, 5, s_sub);

        for (c, (h, hs)) in [
            ("产品线名称", h_left),
            ("签约总额", h_right),
            ("已回款金额", h_right),
            ("待收尾款", h_right),
            ("回款率", h_right),
            ("营收占比", h_right),
        ]
        .iter()
        .enumerate()
        {
            text(sh, 3, c as u32, h);
            sh.set_style(3, c as u32, *hs);
        }

        let lines = ["企业协同底座", "无锁计算引擎", "数据治理实施", "年度原厂维保"];
        for (i, ln) in lines.iter().enumerate() {
            let r = 4 + i as u32;
            let er = r + 1;
            sh.rows.set(r, 25.0);
            let (_, _, sc_txt, _, sc_amt, sc_bal, sc_pct) = row_styles[i % 2];
            text(sh, r, 0, ln);
            formula(
                sh,
                r,
                1,
                &format!("=SUMIF(订单回款明细!D7:D18, A{er}, 订单回款明细!G7:G18)"),
            );
            formula(
                sh,
                r,
                2,
                &format!("=SUMIF(订单回款明细!D7:D18, A{er}, 订单回款明细!H7:H18)"),
            );
            formula(sh, r, 3, &format!("=B{er}-C{er}"));
            formula(sh, r, 4, &format!("=C{er}/B{er}"));
            formula(sh, r, 5, &format!("=B{er}/$B$9"));
            sh.set_style(r, 0, sc_txt);
            sh.set_style(r, 1, sc_amt);
            sh.set_style(r, 2, sc_amt);
            sh.set_style(r, 3, sc_bal);
            sh.set_style(r, 4, sc_pct);
            sh.set_style(r, 5, sc_pct);
        }
        sh.rows.set(8, 28.0);
        text(sh, 8, 0, "总计");
        formula(sh, 8, 1, "=SUM(B5:B8)");
        formula(sh, 8, 2, "=SUM(C5:C8)");
        formula(sh, 8, 3, "=SUM(D5:D8)");
        formula(sh, 8, 4, "=C9/B9");
        formula(sh, 8, 5, "=SUM(F5:F8)");
        sh.set_style(8, 0, s_tot_lbl);
        sh.set_style(8, 1, s_tot_amt);
        sh.set_style(8, 2, s_tot_amt);
        sh.set_style(8, 3, s_tot_amt);
        sh.set_style(8, 4, s_tot_pct);
        sh.set_style(8, 5, s_tot_pct);
    }

    sheetcore::formula::recalc_all(&mut wb);
    wb
}

// ---------------------------------------------------------------- 2. 部门经营预算

fn budget() -> Workbook {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "预算执行分析".into();

    let forest = [26, 102, 66];
    let s_title = banner_title_style(&mut wb, [20, 83, 45], [236, 253, 245], forest);
    let s_sub = banner_sub_style(&mut wb);
    let s_kpi_l = kpi_label_style(&mut wb, [244, 251, 247]);
    let s_kpi_b = kpi_val_style(&mut wb, "¥#,##0", [15, 23, 42], [244, 251, 247]);
    let s_kpi_a = kpi_val_style(&mut wb, "¥#,##0", forest, [244, 251, 247]);
    let s_kpi_d = kpi_val_style(&mut wb, "¥#,##0;[Red]-¥#,##0", [4, 120, 87], [236, 253, 245]);
    let s_kpi_r = kpi_val_style(&mut wb, "0.0%", forest, [236, 253, 245]);

    let h_left = table_head_style(&mut wb, forest, 1);
    let h_center = table_head_style(&mut wb, forest, 2);
    let h_right = table_head_style(&mut wb, forest, 3);

    let mut row_styles = Vec::new();
    for zebra in [false, true] {
        row_styles.push((
            row_cell_style(&mut wb, "", 2, zebra, [71, 85, 105]),
            row_cell_style(&mut wb, "", 1, zebra, [15, 23, 42]),
            row_cell_style(&mut wb, "#,##0", 3, zebra, [15, 23, 42]),
            row_cell_style(&mut wb, "#,##0;[Red]-#,##0", 3, zebra, [30, 41, 59]),
            row_cell_style(&mut wb, "0.0%", 3, zebra, [15, 23, 42]),
            row_cell_style(&mut wb, "", 1, zebra, [100, 116, 139]),
        ));
    }
    let s_tot_lbl = total_row_style(&mut wb, "", 1, forest, [236, 253, 245]);
    let s_tot_num = total_row_style(&mut wb, "¥#,##0;[Red]-¥#,##0", 3, forest, [236, 253, 245]);
    let s_tot_pct = total_row_style(&mut wb, "0.0%", 3, forest, [236, 253, 245]);

    {
        let sh = &mut wb.sheets[0];
        widths(
            sh,
            &[90.0, 110.0, 214.0, 112.0, 112.0, 110.0, 98.0, 92.0, 212.0],
        );
        sh.rows.set(0, 36.0);
        sh.rows.set(1, 22.0);
        sh.rows.set(2, 20.0);
        sh.rows.set(3, 30.0);
        sh.rows.set(4, 10.0);
        sh.rows.set(5, 28.0);

        sh.merge_range(0, 0, 0, 8);
        text(sh, 0, 0, "2026 年度研发与运营中心月度费用预算执行表");
        style_range(sh, 0, 0, 0, 8, s_title);

        sh.merge_range(1, 0, 1, 8);
        text(
            sh,
            1,
            0,
            "币种：CNY（元） · 正差额代表预算结余，红色负差额代表超支项 · 执行率超过 100% 自动高亮预警",
        );
        style_range(sh, 1, 0, 1, 8, s_sub);

        for &(c1, c2, lbl, fml, vs) in &[
            (0u32, 2u32, "当月核定总预算", "=SUM(D7:D17)", s_kpi_b),
            (3, 4, "当月实际总支出", "=SUM(E7:E17)", s_kpi_a),
            (5, 6, "当月净结余 / 超支", "=A4-D4", s_kpi_d),
            (7, 8, "总预算执行率", "=D4/A4", s_kpi_r),
        ] {
            sh.merge_range(2, c1, 2, c2);
            sh.merge_range(3, c1, 3, c2);
            text(sh, 2, c1, lbl);
            formula(sh, 3, c1, fml);
            style_range(sh, 2, c1, 2, c2, s_kpi_l);
            style_range(sh, 3, c1, 3, c2, vs);
        }

        for (c, (h, hs)) in [
            ("科目编码", h_center),
            ("费用大类", h_left),
            ("预算明细科目", h_left),
            ("月度预算", h_right),
            ("实际发生", h_right),
            ("预算结余", h_right),
            ("执行率", h_right),
            ("支出占比", h_right),
            ("执行说明与管控备注", h_left),
        ]
        .iter()
        .enumerate()
        {
            text(sh, 5, c as u32, h);
            sh.set_style(5, c as u32, *hs);
        }

        let items: &[(&str, &str, &str, f64, f64, &str)] = &[
            ("6602-01", "人力与薪酬", "核心研发薪资与项目奖金", 480000.0, 472500.0, "编制内正常发放"),
            ("6602-02", "人力与薪酬", "五险一金与商业补充福利", 118000.0, 118000.0, "按核定基数计提"),
            ("6603-01", "云基础设施", "生产环境云服务器与带宽", 85000.0, 81200.0, "预留实例折扣生效"),
            ("6603-02", "云基础设施", "GPU 算力压测与弹性集群", 60000.0, 68400.0, "Q4 大并发压测临时扩容"),
            ("6603-03", "研发工具链", "代码安全审计与正版软件授权", 28000.0, 26500.0, "年度订阅分摊"),
            ("6601-01", "市场与交付", "重点客户驻场实施差旅费", 45000.0, 49800.0, "华东/西南两期项目并行交付"),
            ("6601-02", "市场与交付", "行业技术峰会展位与品牌推广", 50000.0, 42000.0, "线上直播替代部分线下物料"),
            ("6601-03", "市场与交付", "渠道合作伙伴培训与沙龙", 24000.0, 19600.0, "按季度节奏推进"),
            ("6604-01", "行政与办公", "研发办公场地租金及物业能耗", 96000.0, 96000.0, "长租合同固定支出"),
            ("6604-02", "行政与办公", "测试终端与网络设备折旧增补", 32000.0, 27800.0, "采购高刷多屏测试机 4 台"),
            ("6604-03", "人才发展", "架构师专项内训与技术图书", 15000.0, 11200.0, "Rust 高并发专题工作坊"),
        ];

        for (i, (code, cat, name, b, a, note)) in items.iter().enumerate() {
            let r = 6 + i as u32;
            let er = r + 1;
            sh.rows.set(r, 24.0);
            let (sc_code, sc_txt, sc_num, sc_diff, sc_pct, sc_note) = row_styles[i % 2];
            text(sh, r, 0, code);
            text(sh, r, 1, cat);
            text(sh, r, 2, name);
            num(sh, r, 3, *b);
            num(sh, r, 4, *a);
            formula(sh, r, 5, &format!("=D{er}-E{er}"));
            formula(sh, r, 6, &format!("=E{er}/D{er}"));
            formula(sh, r, 7, &format!("=E{er}/$E$18"));
            text(sh, r, 8, note);

            sh.set_style(r, 0, sc_code);
            sh.set_style(r, 1, sc_txt);
            sh.set_style(r, 2, sc_txt);
            sh.set_style(r, 3, sc_num);
            sh.set_style(r, 4, sc_num);
            sh.set_style(r, 5, sc_diff);
            sh.set_style(r, 6, sc_pct);
            sh.set_style(r, 7, sc_pct);
            sh.set_style(r, 8, sc_note);
        }

        let tr = 17u32;
        sh.rows.set(tr, 28.0);
        text(sh, tr, 0, "合计");
        text(sh, tr, 2, "11 项经营费用科目汇总");
        formula(sh, tr, 3, "=SUM(D7:D17)");
        formula(sh, tr, 4, "=SUM(E7:E17)");
        formula(sh, tr, 5, "=D18-E18");
        formula(sh, tr, 6, "=E18/D18");
        formula(sh, tr, 7, "=SUM(H7:H17)");
        text(sh, tr, 8, "整体预算受控，结余 2.0%");
        for c in [0, 1, 2, 8] {
            sh.set_style(tr, c, s_tot_lbl);
        }
        sh.set_style(tr, 3, s_tot_num);
        sh.set_style(tr, 4, s_tot_num);
        sh.set_style(tr, 5, s_tot_num);
        sh.set_style(tr, 6, s_tot_pct);
        sh.set_style(tr, 7, s_tot_pct);

        // 执行率翡翠数据条 + 超支（>100%）浅红预警高亮
        let over_dxf = sh.cf.dxfs.len();
        sh.cf.dxfs.push(Dxf {
            bold: true,
            color: Some([185, 28, 28]),
            fill: Some([254, 242, 242]),
            ..Default::default()
        });
        sh.cf.blocks.push(CfBlock {
            ranges: vec![(6, 6, 16, 6)],
            rules: vec![
                CfRule {
                    kind: CfKind::CellIs {
                        op: CfOp::Greater,
                        a: "1".into(),
                        b: String::new(),
                        dxf: over_dxf,
                    },
                    stop: false,
                    priority: 1,
                },
                CfRule {
                    kind: CfKind::Bar {
                        color: [16, 185, 129],
                        min: Cfvo::new(CfvoKind::Num, 0.0),
                        max: Cfvo::new(CfvoKind::Num, 1.2),
                    },
                    stop: false,
                    priority: 2,
                },
            ],
        });
        sh.freeze = Some((6, 0));
    }

    sheetcore::formula::recalc_all(&mut wb);
    wb
}

// ---------------------------------------------------------------- 3. 项目里程碑排期

fn schedule() -> Workbook {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "WBS项目排期".into();

    let slate = [30, 41, 59];
    let s_title = banner_title_style(&mut wb, slate, [241, 245, 249], [27, 53, 129]);
    let s_sub = banner_sub_style(&mut wb);
    let s_kpi_l = kpi_label_style(&mut wb, [248, 250, 252]);
    let s_kpi_n = kpi_val_style(&mut wb, "0\" 项\"", slate, [248, 250, 252]);
    let s_kpi_d = kpi_val_style(&mut wb, "#,##0\" 天\"", [27, 53, 129], [248, 250, 252]);
    let s_kpi_p = kpi_val_style(&mut wb, "0.0%", [4, 120, 87], [236, 253, 245]);
    let s_kpi_ok = kpi_val_style(&mut wb, "0\" 项\"", [4, 120, 87], [236, 253, 245]);

    let h_left = table_head_style(&mut wb, slate, 1);
    let h_center = table_head_style(&mut wb, slate, 2);
    let h_right = table_head_style(&mut wb, slate, 3);

    let mut row_styles = Vec::new();
    for zebra in [false, true] {
        row_styles.push((
            row_cell_style(&mut wb, "", 2, zebra, [71, 85, 105]),
            row_cell_style(&mut wb, "", 1, zebra, [15, 23, 42]),
            row_cell_style(&mut wb, "yyyy-mm-dd", 2, zebra, [51, 65, 85]),
            row_cell_style(&mut wb, "0", 3, zebra, [15, 23, 42]),
            row_cell_style(&mut wb, "0%", 3, zebra, [15, 23, 42]),
        ));
    }
    let s_tot_lbl = total_row_style(&mut wb, "", 1, slate, [241, 245, 249]);
    let s_tot_day = total_row_style(&mut wb, "#,##0", 3, slate, [241, 245, 249]);
    let s_tot_pct = total_row_style(&mut wb, "0.0%", 3, slate, [241, 245, 249]);

    {
        let sh = &mut wb.sheets[0];
        widths(
            sh,
            &[68.0, 106.0, 276.0, 90.0, 106.0, 106.0, 84.0, 98.0, 96.0],
        );
        sh.rows.set(0, 36.0);
        sh.rows.set(1, 22.0);
        sh.rows.set(2, 20.0);
        sh.rows.set(3, 30.0);
        sh.rows.set(4, 10.0);
        sh.rows.set(5, 28.0);

        sh.merge_range(0, 0, 0, 8);
        text(sh, 0, 0, "核心协同引擎 V2.0 研发交付 — WBS 里程碑排期表");
        style_range(sh, 0, 0, 0, 8, s_title);

        sh.merge_range(1, 0, 1, 8);
        text(
            sh,
            1,
            0,
            "工期由计划起止日期自动计算（含首尾日） · 完成进度列内嵌实时甘特进度条 · 支持多人内网同步更新状态",
        );
        style_range(sh, 1, 0, 1, 8, s_sub);

        for &(c1, c2, lbl, fml, vs) in &[
            (0u32, 2u32, "WBS 工作包总数", "=COUNTA(C7:C17)", s_kpi_n),
            (3, 4, "累计计划工期", "=SUM(G7:G17)", s_kpi_d),
            (5, 6, "整体平均完成度", "=AVERAGE(H7:H17)", s_kpi_p),
            (7, 8, "已交付里程碑", "=COUNTIF(I7:I17, \"已完成\")", s_kpi_ok),
        ] {
            sh.merge_range(2, c1, 2, c2);
            sh.merge_range(3, c1, 3, c2);
            text(sh, 2, c1, lbl);
            formula(sh, 3, c1, fml);
            style_range(sh, 2, c1, 2, c2, s_kpi_l);
            style_range(sh, 3, c1, 3, c2, vs);
        }

        for (c, (h, hs)) in [
            ("WBS", h_center),
            ("阶段模块", h_left),
            ("关键交付物与里程碑任务", h_left),
            ("负责人", h_center),
            ("计划开始", h_center),
            ("计划完成", h_center),
            ("工期(天)", h_right),
            ("完成进度", h_right),
            ("当前状态", h_center),
        ]
        .iter()
        .enumerate()
        {
            text(sh, 5, c as u32, h);
            sh.set_style(5, c as u32, *hs);
        }

        let tasks: &[(&str, &str, &str, &str, (i32, u32, u32), (i32, u32, u32), f64, &str)] = &[
            ("1.1", "架构立项", "业务需求基线评审与存储协议定稿", "张岚", (2026, 11, 2), (2026, 11, 6), 1.0, "已完成"),
            ("1.2", "架构立项", "无锁异步复制状态机（CRDT）形式化验证", "陈默", (2026, 11, 9), (2026, 11, 14), 1.0, "已完成"),
            ("2.1", "计算核心", "向量化公式依赖图与增量重算管线", "李哲", (2026, 11, 16), (2026, 11, 25), 1.0, "已完成"),
            ("2.2", "计算核心", "OOXML / BIFF8 样式与条件格式无损往返", "李哲", (2026, 11, 26), (2026, 12, 4), 0.85, "进行中"),
            ("3.1", "协同网络", "局域网多播发现、租约锁与断线自动选主", "周航", (2026, 11, 23), (2026, 12, 5), 0.75, "进行中"),
            ("3.2", "协同网络", "共享盘会话占用卡与口令鉴权加密通道", "周航", (2026, 12, 7), (2026, 12, 14), 0.40, "进行中"),
            ("4.1", "桌面体验", "Fluent 悬浮卡片工作区与高分屏矢量图标", "林溪", (2026, 12, 8), (2026, 12, 16), 0.60, "进行中"),
            ("4.2", "桌面体验", "百万行虚拟滚动与视口级条件格式缓存", "陈默", (2026, 12, 17), (2026, 12, 23), 0.25, "风险关注"),
            ("5.1", "质量验收", "20 人并发压测、断网混沌演练与内存审计", "王珂", (2026, 12, 24), (2026, 12, 31), 0.0, "待启动"),
            ("5.2", "质量验收", "UIA 无障碍读屏适配与全量回归测试闭环", "王珂", (2027, 1, 4), (2027, 1, 8), 0.0, "待启动"),
            ("6.1", "正式投产", "静态单文件签名打包与财务部首批灰度上线", "张岚", (2027, 1, 11), (2027, 1, 15), 0.0, "待启动"),
        ];

        for (i, (wbs, stage, task, owner, s, e, prog, st)) in tasks.iter().enumerate() {
            let r = 6 + i as u32;
            let er = r + 1;
            sh.rows.set(r, 24.0);
            let (sc_ctr, sc_txt, sc_dt, sc_day, sc_pct) = row_styles[i % 2];
            text(sh, r, 0, wbs);
            text(sh, r, 1, stage);
            text(sh, r, 2, task);
            text(sh, r, 3, owner);
            num(sh, r, 4, date_serial(s.0, s.1, s.2));
            num(sh, r, 5, date_serial(e.0, e.1, e.2));
            formula(sh, r, 6, &format!("=F{er}-E{er}+1"));
            num(sh, r, 7, *prog);
            text(sh, r, 8, st);

            sh.set_style(r, 0, sc_ctr);
            sh.set_style(r, 1, sc_txt);
            sh.set_style(r, 2, sc_txt);
            sh.set_style(r, 3, sc_ctr);
            sh.set_style(r, 4, sc_dt);
            sh.set_style(r, 5, sc_dt);
            sh.set_style(r, 6, sc_day);
            sh.set_style(r, 7, sc_pct);
            sh.set_style(r, 8, sc_ctr);
        }

        let tr = 17u32;
        sh.rows.set(tr, 28.0);
        text(sh, tr, 0, "汇总");
        text(sh, tr, 2, "6 大阶段 · 共 11 项研发里程碑任务");
        formula(sh, tr, 6, "=SUM(G7:G17)");
        formula(sh, tr, 7, "=AVERAGE(H7:H17)");
        for c in [0, 1, 2, 3, 4, 5, 8] {
            sh.set_style(tr, c, s_tot_lbl);
        }
        sh.set_style(tr, 6, s_tot_day);
        sh.set_style(tr, 7, s_tot_pct);

        sh.cf.blocks.push(CfBlock {
            ranges: vec![(6, 7, 16, 7)],
            rules: vec![CfRule {
                kind: CfKind::Bar {
                    color: [9, 189, 141],
                    min: Cfvo::new(CfvoKind::Num, 0.0),
                    max: Cfvo::new(CfvoKind::Num, 1.0),
                },
                stop: false,
                priority: 1,
            }],
        });
        push_status_cf(
            sh,
            (6, 8, 16, 8),
            &[
                ("已完成", [4, 120, 87], [236, 253, 245]),
                ("进行中", [27, 53, 129], [238, 242, 255]),
                ("风险关注", [185, 28, 28], [254, 242, 242]),
                ("待启动", [100, 116, 139], [241, 245, 249]),
            ],
        );
        sh.freeze = Some((6, 0));
    }

    sheetcore::formula::recalc_all(&mut wb);
    wb
}

// ---------------------------------------------------------------- 缩略图数据

/// 模板卡片上的小预览：左上 3 行 × 4 列的真实内容（文字 + 底色）。
pub struct Preview {
    pub cells: Vec<Vec<PreviewCell>>,
}

pub struct PreviewCell {
    pub text: String,
    pub fill: Option<[u8; 3]>,
    pub bold: bool,
}

/// 只建一次：开始页每帧都要画，不能每帧重建四个工作簿。
pub fn previews() -> &'static [Preview] {
    static CACHE: OnceLock<Vec<Preview>> = OnceLock::new();
    CACHE.get_or_init(|| {
        (0..ALL.len())
            .map(|i| {
                let wb = build(i);
                let sh = &wb.sheets[0];
                let mut rows = Vec::new();
                for r in 0..3 {
                    let mut row = Vec::new();
                    for c in 0..4 {
                        let text = sh
                            .get(r, c)
                            .map(|cell| cell.value.display())
                            .unwrap_or_default();
                        let st = wb.styles.get(sh.style_of(r, c));
                        row.push(PreviewCell {
                            text,
                            fill: st.fill,
                            bold: st.bold,
                        });
                    }
                    rows.push(row);
                }
                Preview { cells: rows }
            })
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_template_builds_and_recalcs() {
        for (i, t) in ALL.iter().enumerate() {
            let wb = build(i);
            assert!(!wb.sheets.is_empty(), "{} 没有工作表", t.name);
            assert!(!wb.sheets[0].name.is_empty());
            // 模板必须能算出结果：除空白外的合计/金额格不能是空值
            if i > 0 {
                let sh = &wb.sheets[0];
                let mut numbers = 0;
                for (_, cell) in sh.iter() {
                    if matches!(cell.value, Value::Number(_)) {
                        numbers += 1;
                    }
                }
                assert!(numbers >= 20, "{} 只有 {numbers} 个数值，模板内容像占位", t.name);
                for (si, sheet) in wb.sheets.iter().enumerate() {
                    for (cr, cell) in sheet.iter() {
                        assert!(
                            !matches!(cell.value, Value::Error(_)),
                            "{} 表{} ({},{}) 出现公式错误 {:?}",
                            t.name,
                            si,
                            cr.row,
                            cr.col,
                            cell.value
                        );
                    }
                }
            }
        }
        // 验证销售台账的双工作表 SUMIF 联动也已算出非零金额
        let sales_wb = build(1);
        assert_eq!(sales_wb.sheets.len(), 2);
        match sales_wb.sheets[1].get(8, 1).map(|c| &c.value) {
            Some(Value::Number(v)) => assert!(*v > 1_000_000.0, "跨表 SUMIF 未算出结果: {v}"),
            other => panic!("跨表汇总总额异常: {other:?}"),
        }
    }
}
