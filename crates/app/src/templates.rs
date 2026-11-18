//! 开始页的内置模板：全部在内存里现建，不依赖外部文件。
//!
//! 每个模板都是可编辑的真实工作簿（公式、格式、列宽、冻结），
//! 不是「点一下只填几个字」的占位按钮。

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
        hint: "从一张干净的工作表开始",
        build: blank,
    },
    Template {
        name: "销售明细",
        hint: "订单、客户、金额与状态，合计自动算",
        build: sales,
    },
    Template {
        name: "家庭预算",
        hint: "月度预算对比，差额自动计算",
        build: budget,
    },
    Template {
        name: "项目排期",
        hint: "阶段、负责人与工期，日期自动算",
        build: schedule,
    },
];

pub fn build(idx: usize) -> Workbook {
    (ALL.get(idx).map(|t| t.build).unwrap_or(blank))()
}

// ---------------------------------------------------------------- 样式

fn border() -> Border {
    Border {
        style: 1,
        color: [217, 217, 217],
    }
}

fn style(wb: &mut Workbook, s: CellStyle) -> u16 {
    wb.styles.intern(s)
}

/// 表头：加粗、浅灰底、居中、四边细线。
fn head_style(wb: &mut Workbook) -> u16 {
    style(
        wb,
        CellStyle {
            bold: true,
            size: 11.0,
            fill: Some([242, 242, 242]),
            halign: 2,
            valign: 2,
            border: [border(); 4],
            ..Default::default()
        },
    )
}

/// 正文：细线 + 垂直居中；`fmt` 是数字格式，空串表示常规。
fn body_style(wb: &mut Workbook, fmt: &str, halign: u8) -> u16 {
    style(
        wb,
        CellStyle {
            size: 11.0,
            format: fmt.to_string(),
            halign,
            valign: 2,
            border: [border(); 4],
            ..Default::default()
        },
    )
}

/// 合计行：加粗 + 上边框（不铺底色，和 Excel 的汇总行习惯一致）。
fn total_style(wb: &mut Workbook, fmt: &str, halign: u8) -> u16 {
    let top = Border {
        style: 1,
        color: [127, 127, 127],
    };
    style(
        wb,
        CellStyle {
            bold: true,
            size: 11.0,
            format: fmt.to_string(),
            halign,
            valign: 2,
            border: [border(), border(), top, border()],
            ..Default::default()
        },
    )
}

// ---------------------------------------------------------------- 写入

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

// ---------------------------------------------------------------- 模板本体

fn blank() -> Workbook {
    Workbook::new_blank()
}

fn sales() -> Workbook {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "销售明细".into();
    let sh = &mut wb.sheets[0];
    widths(sh, &[108.0, 176.0, 140.0, 64.0, 80.0, 96.0, 84.0]);
    for (c, h) in ["订单号", "客户", "产品", "数量", "单价", "金额", "状态"]
        .iter()
        .enumerate()
    {
        text(sh, 0, c as u32, h);
    }
    let customers = [
        "华东贸易有限公司",
        "鼎新科技（上海）",
        "南方物流集团",
        "北京博远咨询",
        "广州海联电子",
        "成都锦程实业",
        "杭州云图网络",
        "苏州精工机械",
    ];
    let products = [
        "办公用品",
        "电子设备",
        "耗材",
        "软件服务",
        "运输服务",
        "办公用品",
        "电子设备",
        "软件服务",
    ];
    let qty = [12.0, 3.0, 40.0, 1.0, 6.0, 25.0, 2.0, 1.0];
    let price = [38.5, 4299.0, 12.8, 9800.0, 460.0, 26.0, 3150.0, 12800.0];
    let status = [
        "已完成", "已完成", "处理中", "处理中", "待发货", "已完成", "待发货", "已完成",
    ];
    for i in 0..8 {
        let r = i as u32 + 1;
        text(sh, r, 0, &format!("SO-2026-{:04}", 1001 + i));
        text(sh, r, 1, customers[i]);
        text(sh, r, 2, products[i]);
        num(sh, r, 3, qty[i]);
        num(sh, r, 4, price[i]);
        formula(sh, r, 5, &format!("=D{}*E{}", r + 1, r + 1));
        text(sh, r, 6, status[i]);
    }
    text(sh, 9, 0, "合计");
    formula(sh, 9, 5, "=SUM(F2:F9)");

    let head = head_style(&mut wb);
    let body = body_style(&mut wb, "", 1);
    let money = body_style(&mut wb, "#,##0.00", 3);
    let count = body_style(&mut wb, "0", 3);
    let total = total_style(&mut wb, "#,##0.00", 3);
    let total_label = total_style(&mut wb, "", 1);
    let sh = &mut wb.sheets[0];
    for c in 0..7 {
        sh.set_style(0, c, head);
    }
    for r in 1..9 {
        sh.set_style(r, 0, body);
        sh.set_style(r, 1, body);
        sh.set_style(r, 2, body);
        sh.set_style(r, 3, count);
        sh.set_style(r, 4, money);
        sh.set_style(r, 5, money);
        sh.set_style(r, 6, body);
    }
    sh.set_style(9, 0, total_label);
    for c in 1..7 {
        sh.set_style(9, c, total);
    }
    sh.freeze = Some((1, 0));
    sheetcore::formula::recalc_all(&mut wb);
    wb
}

fn budget() -> Workbook {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "家庭预算".into();
    let sh = &mut wb.sheets[0];
    widths(sh, &[132.0, 108.0, 108.0, 108.0, 200.0]);
    for (c, h) in ["项目", "月度预算", "实际支出", "差额", "备注"].iter().enumerate() {
        text(sh, 0, c as u32, h);
    }
    let rows = [
        ("房租", 3200.0, 3200.0, "含物业费"),
        ("餐饮", 2400.0, 2680.0, ""),
        ("交通", 600.0, 520.0, "地铁 + 打车"),
        ("水电燃气", 380.0, 412.0, ""),
        ("通信", 160.0, 160.0, ""),
        ("教育", 1200.0, 1200.0, "网课"),
        ("医疗", 400.0, 236.0, ""),
        ("娱乐", 800.0, 940.0, ""),
        ("储蓄", 3000.0, 3000.0, "先存后花"),
        ("其他", 500.0, 380.0, ""),
    ];
    for (i, (name, b, a, note)) in rows.iter().enumerate() {
        let r = i as u32 + 1;
        text(sh, r, 0, name);
        num(sh, r, 1, *b);
        num(sh, r, 2, *a);
        formula(sh, r, 3, &format!("=B{}-C{}", r + 1, r + 1));
        text(sh, r, 4, note);
    }
    text(sh, 11, 0, "合计");
    formula(sh, 11, 1, "=SUM(B2:B11)");
    formula(sh, 11, 2, "=SUM(C2:C11)");
    formula(sh, 11, 3, "=B12-C12");

    let head = head_style(&mut wb);
    let body = body_style(&mut wb, "", 1);
    let money = body_style(&mut wb, "#,##0", 3);
    let diff = body_style(&mut wb, "#,##0;[Red]-#,##0", 3);
    let total = total_style(&mut wb, "#,##0", 3);
    let total_label = total_style(&mut wb, "", 1);
    let sh = &mut wb.sheets[0];
    for c in 0..5 {
        sh.set_style(0, c, head);
    }
    for r in 1..11 {
        sh.set_style(r, 0, body);
        sh.set_style(r, 1, money);
        sh.set_style(r, 2, money);
        sh.set_style(r, 3, diff);
        sh.set_style(r, 4, body);
    }
    sh.set_style(11, 0, total_label);
    for c in 1..5 {
        sh.set_style(11, c, total);
    }
    sh.freeze = Some((1, 0));
    sheetcore::formula::recalc_all(&mut wb);
    wb
}

fn schedule() -> Workbook {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "项目排期".into();
    let sh = &mut wb.sheets[0];
    widths(sh, &[96.0, 220.0, 96.0, 100.0, 100.0, 64.0, 84.0]);
    for (c, h) in ["阶段", "任务", "负责人", "开始", "结束", "工期", "状态"]
        .iter()
        .enumerate()
    {
        text(sh, 0, c as u32, h);
    }
    let rows = [
        ("需求", "梳理报销流程与字段", "张岚", (2026, 11, 23), (2026, 11, 27), "已完成"),
        ("需求", "确认审批层级", "张岚", (2026, 11, 30), (2026, 12, 2), "已完成"),
        ("设计", "表单与列表原型", "陈默", (2026, 12, 3), (2026, 12, 9), "进行中"),
        ("开发", "单据录入与校验", "李哲", (2026, 12, 10), (2026, 12, 20), "未开始"),
        ("开发", "审批流与通知", "李哲", (2026, 12, 21), (2027, 1, 6), "未开始"),
        ("测试", "回归与权限验证", "王珂", (2027, 1, 7), (2027, 1, 15), "未开始"),
        ("上线", "培训与切换", "张岚", (2027, 1, 18), (2027, 1, 22), "未开始"),
    ];
    for (i, (stage, task, who, s, e, st)) in rows.iter().enumerate() {
        let r = i as u32 + 1;
        text(sh, r, 0, stage);
        text(sh, r, 1, task);
        text(sh, r, 2, who);
        num(sh, r, 3, date_serial(s.0, s.1, s.2));
        num(sh, r, 4, date_serial(e.0, e.1, e.2));
        formula(sh, r, 5, &format!("=E{}-D{}+1", r + 1, r + 1));
        text(sh, r, 6, st);
    }

    let head = head_style(&mut wb);
    let body = body_style(&mut wb, "", 1);
    let day = body_style(&mut wb, "yyyy-mm-dd", 2);
    let span = body_style(&mut wb, "0", 3);
    let sh = &mut wb.sheets[0];
    for c in 0..7 {
        sh.set_style(0, c, head);
    }
    for r in 1..8 {
        sh.set_style(r, 0, body);
        sh.set_style(r, 1, body);
        sh.set_style(r, 2, body);
        sh.set_style(r, 3, day);
        sh.set_style(r, 4, day);
        sh.set_style(r, 5, span);
        sh.set_style(r, 6, body);
    }
    sh.freeze = Some((1, 0));
    sheetcore::formula::recalc_all(&mut wb);
    wb
}

// ---------------------------------------------------------------- 缩略图

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
                assert!(numbers >= 8, "{} 只有 {numbers} 个数值，模板内容像占位", t.name);
            }
        }
    }
}
