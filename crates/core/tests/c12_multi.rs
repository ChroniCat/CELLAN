//! C12.2 多字段透视：两层行、两个值、筛选、小计、总计。

use sheetcore::model::{Cell, Sheet, Value};
use sheetcore::pivot::Agg;
use sheetcore::pivot_multi::{
    compute, write_grid, AxisField, FilterField, PivotLayout, PivotSpec2, ValueField,
};

fn sheet() -> Sheet {
    let mut s = Sheet::new("数据");
    let head = ["地区", "城市", "产品", "销售额", "数量", "状态"];
    for (c, h) in head.iter().enumerate() {
        s.set(0, c as u32, Cell { value: Value::Text((*h).into()), formula: None });
    }
    let rows: [(&str, &str, &str, Option<f64>, f64, &str); 6] = [
        ("华东", "上海", "甲", Some(100.0), 1.0, "有效"),
        ("华东", "上海", "乙", Some(50.0), 2.0, "有效"),
        ("华东", "杭州", "甲", Some(30.0), 3.0, "有效"),
        ("华北", "北京", "甲", Some(20.0), 4.0, "有效"),
        ("华北", "北京", "乙", None, 5.0, "有效"),
        ("华北", "天津", "甲", None, 6.0, "无效"),
    ];
    for (i, (region, city, prod, amount, qty, state)) in rows.iter().enumerate() {
        let r = i as u32 + 1;
        s.set(r, 0, Cell { value: Value::Text((*region).into()), formula: None });
        s.set(r, 1, Cell { value: Value::Text((*city).into()), formula: None });
        s.set(r, 2, Cell { value: Value::Text((*prod).into()), formula: None });
        if let Some(a) = amount {
            s.set(r, 3, Cell { value: Value::Number(*a), formula: None });
        }
        s.set(r, 4, Cell { value: Value::Number(*qty), formula: None });
        s.set(r, 5, Cell { value: Value::Text((*state).into()), formula: None });
    }
    // 天津的销售额写成文本 x，验证 Count 计非空、Sum 不计文本
    s.set(6, 3, Cell { value: Value::Text("x".into()), formula: None });
    // 天津保持「无效」，被筛选掉；清空筛选后它会带着文本 x 回来
    s
}

fn spec() -> PivotSpec2 {
    PivotSpec2 {
        source: (0, 0, 6, 5),
        rows: vec![AxisField::plain(0), AxisField::plain(1)],
        cols: vec![AxisField::plain(2)],
        values: vec![
            ValueField { col: 3, agg: Agg::Sum, label: None },
            ValueField { col: 4, agg: Agg::Count, label: None },
        ],
        filters: vec![FilterField { col: 5, keep: vec!["有效".into()] }],
        layout: PivotLayout { tabular: true, subtotals: true, grand_totals: true },
    }
}

fn text(v: &Value) -> String {
    v.display()
}

#[test]
fn two_row_fields_two_values_and_a_filter() {
    let s = sheet();
    let g = compute(&s, &spec());
    assert_eq!(g.headers.len(), 2, "一个列字段 + 值字段名 → 两行表头");
    assert_eq!(
        g.headers[0],
        vec!["地区", "城市", "甲", "甲", "乙", "乙"],
        "列标签按产品重复到每个值字段"
    );
    assert_eq!(
        g.headers[1],
        vec!["", "", "销售额 求和", "数量 计数", "销售额 求和", "数量 计数"]
    );
    assert_eq!(
        g.rows[0][..2].iter().map(|v| v.display()).collect::<Vec<_>>(),
        vec!["华北", "北京"]
    );
    assert_eq!(g.rows[0][3], Value::Number(1.0), "北京 甲 只有一条记录，数量计数 1");
}

#[test]
fn subtotal_grand_total_and_filter_semantics() {
    let s = sheet();
    let g = compute(&s, &spec());
    let labels: Vec<String> = g.rows.iter().map(|r| r[0].display()).collect();
    assert_eq!(
        labels,
        vec!["华北", "华北 小计", "华东", "华东", "华东 小计", "总计"],
        "拼音序：华北(bei) 在 华东(dong) 前"
    );
    assert_eq!(g.rows[0][2], Value::Number(20.0));
    assert_eq!(g.rows[0][4], Value::Number(0.0), "北京乙销售额为空，Sum=0");
    assert_eq!(g.rows[0][5], Value::Number(1.0), "北京乙只有一条记录，数量计数 1");
    assert_eq!(g.rows[2][2], Value::Number(30.0), "杭州(hangzhou) 排在 上海 前");
    assert_eq!(g.rows[3][2], Value::Number(100.0));
    assert_eq!(g.rows[4][2], Value::Number(130.0), "华东 甲 销售额小计");
    assert_eq!(g.rows[4][3], Value::Number(2.0), "华东 甲 计数 2");
    let total = g.grand_total_row.expect("应有总计行");
    assert_eq!(g.rows[total][2], Value::Number(150.0), "总计 甲 销售额");
    assert_eq!(g.rows[total][3], Value::Number(3.0), "总计 甲 计数 1+1+1");
    assert_eq!(g.rows[total][4], Value::Number(50.0), "总计 乙 销售额");
}

#[test]
fn clearing_the_filter_keeps_all_rows_and_recomputes() {
    let s = sheet();
    let mut sp = spec();
    sp.filters.clear();
    let g = compute(&s, &sp);
    assert_eq!(
        g.rows.iter().filter(|r| r[0].display().starts_with("华北")).count(),
        3,
        "去掉筛选后天津那一行要回来（北京 + 天津 + 小计）"
    );
    let total = g.grand_total_row.unwrap();
    assert_eq!(g.rows[total][2], Value::Number(150.0), "全部销售额 100+30+20+0（文本 x 不计）");
    assert_eq!(g.rows[total][3], Value::Number(4.0), "甲计数含文本 x");
}

#[test]
fn no_row_field_degrades_to_grand_total_only() {
    let s = sheet();
    let mut sp = spec();
    sp.rows.clear();
    let g = compute(&s, &sp);
    assert_eq!(g.headers[0].len(), 4, "没有行字段时只剩两个产品的两个值");
    let last = g.rows.last().expect("至少一行");
    assert_eq!(last[0], Value::Number(150.0), "最后一行的甲销售额是全部合计");
}

#[test]
fn write_grid_lands_values_at_the_target_anchor() {
    let s = sheet();
    let g = compute(&s, &spec());
    let mut out = Sheet::new("透视");
    let rect = write_grid(&mut out, (3, 2), &g);
    assert_eq!(rect.0, 3);
    assert_eq!(rect.1, 2);
    assert_eq!(out.get(3, 2).unwrap().value.display(), "地区");
    assert_eq!(out.get(4, 4).unwrap().value.display(), "销售额 求和");
    assert_eq!(out.get(5, 2).unwrap().value.display(), "华北");
    assert_eq!(out.get(5, 4).map(|c| c.value.display()).unwrap_or_default(), "20");
    // hue: 表头 2 行 + 数据行数
    let expect_last = 3 + g.headers.len() as u32 + g.rows.len() as u32 - 1;
    assert_eq!(rect.2, expect_last);
}
