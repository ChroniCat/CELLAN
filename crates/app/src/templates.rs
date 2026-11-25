//! 开始页的内置工作簿模板：空白工作簿 + 6 套生产级中文财务模板（带真实公式、格式与列宽）。

use sheetcore::model::{Cell, Value, Workbook};
use sheetcore::style::{Border, CellStyle};
use std::sync::OnceLock;

pub struct Template {
    pub name: &'static str,
    pub build: fn() -> Workbook,
}

pub const ALL: [Template; 7] = [
    Template { name: "空白工作簿", build: blank },
    Template { name: "科目余额表", build: trial_balance },
    Template { name: "利润表", build: income_statement },
    Template { name: "现金流量表", build: cash_flow },
    Template { name: "费用报销单", build: expense_claim },
    Template { name: "固定资产折旧台账", build: fixed_assets },
    Template { name: "工资核算表", build: payroll_sheet },
];

pub fn build(idx: usize) -> Workbook {
    (ALL.get(idx).map(|t| t.build).unwrap_or(blank))()
}

fn blank() -> Workbook {
    Workbook::new_blank()
}

// ---------------------------------------------------------------- 模板构建辅助

struct Styles {
    title: u16,
    header: u16,
    text: u16,
    text_center: u16,
    money: u16,
    pct: u16,
    int_num: u16,
    total_label: u16,
    total_money: u16,
}

fn thin_all() -> [Border; 4] {
    let b = Border { style: 1, color: [0xCB, 0xD5, 0xE1] };
    [b, b, b, b]
}

fn make_styles(wb: &mut Workbook) -> Styles {
    let base = CellStyle::default();
    let title = wb.styles.intern(CellStyle {
        bold: true,
        size: 14.0,
        color: [0x0F, 0x17, 0x2A],
        fill: Some([0xEF, 0xF6, 0xFF]),
        halign: 2,
        valign: 2,
        ..base.clone()
    });
    let header = wb.styles.intern(CellStyle {
        bold: true,
        size: 11.0,
        color: [0xFF, 0xFF, 0xFF],
        fill: Some([0x1E, 0x3A, 0x5F]),
        halign: 2,
        valign: 2,
        wrap: true,
        border: thin_all(),
        ..base.clone()
    });
    let text = wb.styles.intern(CellStyle {
        halign: 1,
        valign: 2,
        border: thin_all(),
        ..base.clone()
    });
    let text_center = wb.styles.intern(CellStyle {
        halign: 2,
        valign: 2,
        border: thin_all(),
        ..base.clone()
    });
    let money = wb.styles.intern(CellStyle {
        halign: 3,
        valign: 2,
        format: "#,##0.00".into(),
        border: thin_all(),
        ..base.clone()
    });
    let pct = wb.styles.intern(CellStyle {
        halign: 3,
        valign: 2,
        format: "0.00%".into(),
        border: thin_all(),
        ..base.clone()
    });
    let int_num = wb.styles.intern(CellStyle {
        halign: 3,
        valign: 2,
        format: "#,##0".into(),
        border: thin_all(),
        ..base.clone()
    });
    let total_label = wb.styles.intern(CellStyle {
        bold: true,
        halign: 2,
        valign: 2,
        fill: Some([0xF1, 0xF5, 0xF9]),
        border: thin_all(),
        ..base.clone()
    });
    let total_money = wb.styles.intern(CellStyle {
        bold: true,
        halign: 3,
        valign: 2,
        fill: Some([0xF1, 0xF5, 0xF9]),
        format: "¥#,##0.00".into(),
        border: thin_all(),
        ..base.clone()
    });
    Styles {
        title,
        header,
        text,
        text_center,
        money,
        pct,
        int_num,
        total_label,
        total_money,
    }
}

fn set_txt(wb: &mut Workbook, r: u32, c: u32, s: &str, sid: u16) {
    let sh = &mut wb.sheets[0];
    sh.set(r, c, Cell { value: Value::Text(s.into()), formula: None });
    sh.set_style(r, c, sid);
}

fn set_num(wb: &mut Workbook, r: u32, c: u32, n: f64, sid: u16) {
    let sh = &mut wb.sheets[0];
    sh.set(r, c, Cell { value: Value::Number(n), formula: None });
    sh.set_style(r, c, sid);
}

fn set_fx(wb: &mut Workbook, r: u32, c: u32, f: &str, sid: u16) {
    let sh = &mut wb.sheets[0];
    sh.set(r, c, Cell { value: Value::Empty, formula: Some(f.into()) });
    sh.set_style(r, c, sid);
}

fn finish_template(wb: &mut Workbook, widths: &[f32]) {
    {
        let sh = &mut wb.sheets[0];
        sh.rows.set(0, 30.0);
        sh.rows.set(1, 24.0);
        for (c, &w) in widths.iter().enumerate() {
            sh.cols.set(c as u32, w);
        }
    }
    sheetcore::formula::recalc_all(wb);
    wb.dirty = false;
}

/// 1. 科目余额表（含借贷平衡校验与期末余额自动结转）
fn trial_balance() -> Workbook {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "科目余额表".into();
    let st = make_styles(&mut wb);
    wb.sheets[0].merges.push((0, 0, 0, 8));
    set_txt(&mut wb, 0, 0, "科目余额表（单位：元）", st.title);
    for (c, h) in [
        "科目编码", "科目名称", "方向", "期初借方", "期初贷方",
        "本期借方发生", "本期贷方发生", "期末借方", "期末贷方",
    ]
    .iter()
    .enumerate()
    {
        set_txt(&mut wb, 1, c as u32, h, st.header);
    }
    let rows: [(&str, &str, &str, f64, f64, f64, f64); 6] = [
        ("1001", "库存现金", "借", 12000.0, 0.0, 5000.0, 3200.0),
        ("1002", "银行存款", "借", 580000.0, 0.0, 245000.0, 168000.0),
        ("1122", "应收账款", "借", 128000.0, 0.0, 96000.0, 84000.0),
        ("2202", "应付账款", "贷", 0.0, 115000.0, 52000.0, 68000.0),
        ("4001", "实收资本", "贷", 0.0, 500000.0, 0.0, 0.0),
        ("6001", "主营业务收入", "贷", 0.0, 105000.0, 0.0, 74800.0),
    ];
    for (i, &(code, name, dir, od, oc, pd, pc)) in rows.iter().enumerate() {
        let r = 2 + i as u32;
        let rn = r + 1;
        set_txt(&mut wb, r, 0, code, st.text_center);
        set_txt(&mut wb, r, 1, name, st.text);
        set_txt(&mut wb, r, 2, dir, st.text_center);
        set_num(&mut wb, r, 3, od, st.money);
        set_num(&mut wb, r, 4, oc, st.money);
        set_num(&mut wb, r, 5, pd, st.money);
        set_num(&mut wb, r, 6, pc, st.money);
        set_fx(&mut wb, r, 7, &format!("=MAX(0,D{rn}-E{rn}+F{rn}-G{rn})"), st.money);
        set_fx(&mut wb, r, 8, &format!("=MAX(0,E{rn}-D{rn}+G{rn}-F{rn})"), st.money);
    }
    let tot = 8u32;
    set_txt(&mut wb, tot, 0, "合计", st.total_label);
    set_txt(&mut wb, tot, 1, "", st.total_label);
    set_txt(&mut wb, tot, 2, "", st.total_label);
    for (c, col) in [(3, "D"), (4, "E"), (5, "F"), (6, "G"), (7, "H"), (8, "I")] {
        set_fx(&mut wb, tot, c, &format!("=SUM({col}3:{col}8)"), st.total_money);
    }
    set_txt(&mut wb, 9, 0, "试算校验", st.total_label);
    set_fx(
        &mut wb,
        9,
        1,
        "=IF(AND(ROUND(D9-E9,2)=0,ROUND(F9-G9,2)=0,ROUND(H9-I9,2)=0),\"试算平衡\",\"借贷不平\")",
        st.text_center,
    );
    finish_template(&mut wb, &[88.0, 140.0, 54.0, 110.0, 110.0, 116.0, 116.0, 110.0, 110.0]);
    wb
}

/// 2. 利润表（月度/本年累计 + 同比变动率 + 毛利率/净利率）
fn income_statement() -> Workbook {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "利润表".into();
    let st = make_styles(&mut wb);
    wb.sheets[0].merges.push((0, 0, 0, 4));
    set_txt(&mut wb, 0, 0, "利润表（月度核算模板，单位：元）", st.title);
    for (c, h) in ["项目", "行次", "本月金额", "本年累计", "占收入比"].iter().enumerate() {
        set_txt(&mut wb, 1, c as u32, h, st.header);
    }
    let items: [(&str, u32, Option<f64>, Option<f64>, Option<&str>, Option<&str>); 10] = [
        ("一、营业收入", 1, Some(1_280_000.0), Some(11_450_000.0), None, None),
        ("减：营业成本", 2, Some(742_000.0), Some(6_680_000.0), None, None),
        ("营业毛利", 3, None, None, Some("=C3-C4"), Some("=D3-D4")),
        ("减：税金及附加", 4, Some(18_500.0), Some(162_000.0), None, None),
        ("销售费用", 5, Some(112_000.0), Some(980_000.0), None, None),
        ("管理费用", 6, Some(96_000.0), Some(865_000.0), None, None),
        ("研发费用", 7, Some(68_000.0), Some(612_000.0), None, None),
        ("二、营业利润", 8, None, None, Some("=C5-SUM(C6:C9)"), Some("=D5-SUM(D6:D9)")),
        ("减：所得税费用（25%）", 9, None, None, Some("=MAX(0,C10*0.25)"), Some("=MAX(0,D10*0.25)")),
        ("三、净利润", 10, None, None, Some("=C10-C11"), Some("=D10-D11")),
    ];
    for (i, &(label, ln, mv, yv, mf, yf)) in items.iter().enumerate() {
        let r = 2 + i as u32;
        let rn = r + 1;
        let is_sub = mf.is_some() && ln != 9;
        let lbl_s = if is_sub { st.total_label } else { st.text };
        let num_s = if is_sub { st.total_money } else { st.money };
        set_txt(&mut wb, r, 0, label, lbl_s);
        set_num(&mut wb, r, 1, ln as f64, st.text_center);
        match (mv, mf) {
            (_, Some(f)) => set_fx(&mut wb, r, 2, f, num_s),
            (Some(v), None) => set_num(&mut wb, r, 2, v, num_s),
            _ => {}
        }
        match (yv, yf) {
            (_, Some(f)) => set_fx(&mut wb, r, 3, f, num_s),
            (Some(v), None) => set_num(&mut wb, r, 3, v, num_s),
            _ => {}
        }
        set_fx(&mut wb, r, 4, &format!("=IF($C$3=0,0,C{rn}/$C$3)"), st.pct);
    }
    finish_template(&mut wb, &[210.0, 56.0, 128.0, 134.0, 96.0]);
    wb
}

/// 3. 现金流量表（经营/投资/筹资三类净流量 + 期末现金结存）
fn cash_flow() -> Workbook {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "现金流量表".into();
    let st = make_styles(&mut wb);
    wb.sheets[0].merges.push((0, 0, 0, 3));
    set_txt(&mut wb, 0, 0, "现金流量表（单位：元）", st.title);
    for (c, h) in ["项目", "行次", "本期金额", "上期金额"].iter().enumerate() {
        set_txt(&mut wb, 1, c as u32, h, st.header);
    }
    let rows: [(&str, u32, Option<f64>, Option<f64>, Option<&str>, Option<&str>); 11] = [
        ("销售商品、提供劳务收到的现金", 1, Some(1_180_000.0), Some(1_040_000.0), None, None),
        ("购买商品、接受劳务支付的现金", 2, Some(640_000.0), Some(580_000.0), None, None),
        ("支付给职工以及为职工支付的现金", 3, Some(225_000.0), Some(210_000.0), None, None),
        ("支付的各项税费", 4, Some(78_000.0), Some(71_000.0), None, None),
        ("一、经营活动产生的现金流量净额", 5, None, None, Some("=C3-SUM(C4:C6)"), Some("=D3-SUM(D4:D6)")),
        ("购建固定资产、无形资产支付的现金", 6, Some(95_000.0), Some(42_000.0), None, None),
        ("二、投资活动产生的现金流量净额", 7, None, None, Some("=-C8"), Some("=-D8")),
        ("偿还债务支付的现金", 8, Some(50_000.0), Some(50_000.0), None, None),
        ("三、筹资活动产生的现金流量净额", 9, None, None, Some("=-C10"), Some("=-D10")),
        ("四、现金及现金等价物净增加额", 10, None, None, Some("=C7+C9+C11"), Some("=D7+D9+D11")),
        ("五、期末现金及现金等价物余额（期初58万）", 11, None, None, Some("=580000+C12"), Some("=490000+D12")),
    ];
    for (i, &(label, ln, cv, pv, cf, pf)) in rows.iter().enumerate() {
        let r = 2 + i as u32;
        let is_sub = cf.is_some();
        set_txt(&mut wb, r, 0, label, if is_sub { st.total_label } else { st.text });
        set_num(&mut wb, r, 1, ln as f64, st.text_center);
        match (cv, cf) {
            (_, Some(f)) => set_fx(&mut wb, r, 2, f, if is_sub { st.total_money } else { st.money }),
            (Some(v), None) => set_num(&mut wb, r, 2, v, st.money),
            _ => {}
        }
        match (pv, pf) {
            (_, Some(f)) => set_fx(&mut wb, r, 3, f, if is_sub { st.total_money } else { st.money }),
            (Some(v), None) => set_num(&mut wb, r, 3, v, st.money),
            _ => {}
        }
    }
    finish_template(&mut wb, &[290.0, 54.0, 132.0, 132.0]);
    wb
}

/// 4. 费用报销单（明细含税价 + 税额计算 + 大写金额 NUMBERSTRING）
fn expense_claim() -> Workbook {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "费用报销单".into();
    let st = make_styles(&mut wb);
    wb.sheets[0].merges.push((0, 0, 0, 7));
    set_txt(&mut wb, 0, 0, "费用报销单", st.title);
    for (c, h) in ["序号", "费用日期", "费用类别", "事由摘要", "不含税金额", "税率", "税额", "价税合计"].iter().enumerate() {
        set_txt(&mut wb, 1, c as u32, h, st.header);
    }
    let rows = [
        (1, "2026-11-10", "差旅费", "上海客户拜访高铁往返", 1106.0, 0.09),
        (2, "2026-11-11", "住宿费", "出差酒店住宿 2 晚", 849.06, 0.06),
        (3, "2026-11-12", "业务招待费", "客户商务工作餐", 620.0, 0.06),
        (4, "2026-11-15", "办公费", "财务凭证装订耗材", 285.0, 0.13),
    ];
    for (i, &(idx, dt, cat, desc, amt, rate)) in rows.iter().enumerate() {
        let r = 2 + i as u32;
        let rn = r + 1;
        set_num(&mut wb, r, 0, idx as f64, st.text_center);
        set_txt(&mut wb, r, 1, dt, st.text_center);
        set_txt(&mut wb, r, 2, cat, st.text_center);
        set_txt(&mut wb, r, 3, desc, st.text);
        set_num(&mut wb, r, 4, amt, st.money);
        set_num(&mut wb, r, 5, rate, st.pct);
        set_fx(&mut wb, r, 6, &format!("=ROUND(E{rn}*F{rn},2)"), st.money);
        set_fx(&mut wb, r, 7, &format!("=E{rn}+G{rn}"), st.money);
    }
    set_txt(&mut wb, 6, 0, "合计", st.total_label);
    wb.sheets[0].merges.push((6, 1, 6, 3));
    set_fx(&mut wb, 6, 1, "=\"人民币大写：\"&NUMBERSTRING(INT(H7),2)&\"元整\"", st.total_label);
    set_fx(&mut wb, 6, 4, "=SUM(E3:E6)", st.total_money);
    set_txt(&mut wb, 6, 5, "", st.total_label);
    set_fx(&mut wb, 6, 6, "=SUM(G3:G6)", st.total_money);
    set_fx(&mut wb, 6, 7, "=SUM(H3:H6)", st.total_money);
    finish_template(&mut wb, &[52.0, 96.0, 92.0, 185.0, 108.0, 68.0, 96.0, 112.0]);
    wb
}

/// 5. 固定资产折旧台账（使用 SLN 直线法计算月折旧额、累计折旧与账面净值）
fn fixed_assets() -> Workbook {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "固定资产折旧台账".into();
    let st = make_styles(&mut wb);
    wb.sheets[0].merges.push((0, 0, 0, 9));
    set_txt(&mut wb, 0, 0, "固定资产折旧台账（年限平均法 SLN）", st.title);
    for (c, h) in [
        "资产编号", "资产名称", "使用部门", "原值", "残值率",
        "使用月数", "已提月数", "月折旧额", "累计折旧", "账面净值",
    ]
    .iter()
    .enumerate()
    {
        set_txt(&mut wb, 1, c as u32, h, st.header);
    }
    let rows = [
        ("FA-001", "生产服务器集群", "研发部", 180_000.0, 0.05, 60.0, 18.0),
        ("FA-002", "商用新能源车", "行政部", 260_000.0, 0.05, 48.0, 12.0),
        ("FA-003", "财务高速票据扫描仪", "财务部", 24_000.0, 0.05, 36.0, 10.0),
        ("FA-004", "会议室智能大屏", "总经办", 45_000.0, 0.05, 60.0, 24.0),
    ];
    for (i, &(id, name, dept, cost, salv_r, life_m, used_m)) in rows.iter().enumerate() {
        let r = 2 + i as u32;
        let rn = r + 1;
        set_txt(&mut wb, r, 0, id, st.text_center);
        set_txt(&mut wb, r, 1, name, st.text);
        set_txt(&mut wb, r, 2, dept, st.text_center);
        set_num(&mut wb, r, 3, cost, st.money);
        set_num(&mut wb, r, 4, salv_r, st.pct);
        set_num(&mut wb, r, 5, life_m, st.int_num);
        set_num(&mut wb, r, 6, used_m, st.int_num);
        set_fx(&mut wb, r, 7, &format!("=ROUND(SLN(D{rn},D{rn}*E{rn},F{rn}),2)"), st.money);
        set_fx(&mut wb, r, 8, &format!("=MIN(D{rn}*(1-E{rn}),ROUND(H{rn}*G{rn},2))"), st.money);
        set_fx(&mut wb, r, 9, &format!("=D{rn}-I{rn}"), st.money);
    }
    set_txt(&mut wb, 6, 0, "合计", st.total_label);
    set_txt(&mut wb, 6, 1, "", st.total_label);
    set_txt(&mut wb, 6, 2, "", st.total_label);
    set_fx(&mut wb, 6, 3, "=SUM(D3:D6)", st.total_money);
    set_txt(&mut wb, 6, 4, "", st.total_label);
    set_txt(&mut wb, 6, 5, "", st.total_label);
    set_txt(&mut wb, 6, 6, "", st.total_label);
    set_fx(&mut wb, 6, 7, "=SUM(H3:H6)", st.total_money);
    set_fx(&mut wb, 6, 8, "=SUM(I3:I6)", st.total_money);
    set_fx(&mut wb, 6, 9, "=SUM(J3:J6)", st.total_money);
    finish_template(&mut wb, &[82.0, 150.0, 82.0, 108.0, 68.0, 74.0, 74.0, 102.0, 108.0, 108.0]);
    wb
}

/// 6. 工资核算表（应发工资、个人社保公积金、预估个税、实发工资汇总）
fn payroll_sheet() -> Workbook {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "工资核算表".into();
    let st = make_styles(&mut wb);
    wb.sheets[0].merges.push((0, 0, 0, 9));
    set_txt(&mut wb, 0, 0, "月度员工工资核算表（单位：元）", st.title);
    for (c, h) in [
        "工号", "姓名", "部门", "基本工资", "绩效奖金",
        "餐补交通", "应发合计", "个人五险一金", "预估个税", "实发工资",
    ]
    .iter()
    .enumerate()
    {
        set_txt(&mut wb, 1, c as u32, h, st.header);
    }
    let rows = [
        ("E001", "张敏", "财务部", 12000.0, 3200.0, 800.0, 2400.0),
        ("E002", "李强", "研发部", 18000.0, 5000.0, 800.0, 3600.0),
        ("E003", "王芳", "销售部", 10000.0, 6500.0, 800.0, 2200.0),
        ("E004", "陈浩", "运营部", 11000.0, 2800.0, 800.0, 2100.0),
    ];
    for (i, &(id, name, dept, base_s, bonus, allow, social)) in rows.iter().enumerate() {
        let r = 2 + i as u32;
        let rn = r + 1;
        set_txt(&mut wb, r, 0, id, st.text_center);
        set_txt(&mut wb, r, 1, name, st.text_center);
        set_txt(&mut wb, r, 2, dept, st.text_center);
        set_num(&mut wb, r, 3, base_s, st.money);
        set_num(&mut wb, r, 4, bonus, st.money);
        set_num(&mut wb, r, 5, allow, st.money);
        set_fx(&mut wb, r, 6, &format!("=SUM(D{rn}:F{rn})"), st.money);
        set_num(&mut wb, r, 7, social, st.money);
        set_fx(
            &mut wb,
            r,
            8,
            &format!("=ROUND(MAX(0,G{rn}-H{rn}-5000)*IF(G{rn}-H{rn}-5000<=3000,0.03,IF(G{rn}-H{rn}-5000<=12000,0.1,0.2))-IF(G{rn}-H{rn}-5000<=3000,0,IF(G{rn}-H{rn}-5000<=12000,210,1410)),2)"),
            st.money,
        );
        set_fx(&mut wb, r, 9, &format!("=G{rn}-H{rn}-I{rn}"), st.money);
    }
    set_txt(&mut wb, 6, 0, "合计", st.total_label);
    set_txt(&mut wb, 6, 1, "", st.total_label);
    set_txt(&mut wb, 6, 2, "", st.total_label);
    for (c, col) in [(3, "D"), (4, "E"), (5, "F"), (6, "G"), (7, "H"), (8, "I"), (9, "J")] {
        set_fx(&mut wb, 6, c, &format!("=SUM({col}3:{col}6)"), st.total_money);
    }
    finish_template(&mut wb, &[68.0, 74.0, 82.0, 96.0, 96.0, 86.0, 102.0, 104.0, 92.0, 104.0]);
    wb
}

// ---------------------------------------------------------------- 缩略图数据

pub struct Preview {
    pub cells: Vec<Vec<PreviewCell>>,
}

pub struct PreviewCell {
    pub text: String,
    pub fill: Option<[u8; 3]>,
    pub bold: bool,
}

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
    use sheetcore::model::Value;

    #[test]
    fn every_template_builds_and_recalcs() {
        assert_eq!(ALL.len(), 7, "应包含空白工作簿 + 6 套中文财务模板");
        assert!(build(0).sheets[0].is_empty(), "索引 0 必须保持为空白工作簿");
        for (i, t) in ALL.iter().enumerate() {
            let wb = build(i);
            assert!(!wb.sheets.is_empty(), "{} 没有工作表", t.name);
            assert!(!wb.sheets[0].name.is_empty());
            let mut formula_count = 0usize;
            for (si, sheet) in wb.sheets.iter().enumerate() {
                for (cr, cell) in sheet.iter() {
                    if cell.formula.is_some() {
                        formula_count += 1;
                    }
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
            if i > 0 {
                assert!(formula_count >= 4, "财务模板 {} 必须内置真实可重算公式", t.name);
            }
        }
        let fallback = build(99);
        assert_eq!(fallback.sheets.len(), 1);
        assert!(fallback.sheets[0].is_empty());
    }
}
