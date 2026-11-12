//! 数据分析四件套的验收样本：分类汇总 / 快速填充 / 合并计算 / 预测。
//!
//! cargo run -p sheetcore --example data_probe -- <输出.xlsx>

fn main() -> anyhow::Result<()> {
    let out = std::env::args().nth(1).unwrap_or_else(|| "data_probe.xlsx".into());
    let mut wb = sheetcore::model::Workbook::new_blank();
    let cell_num = |v: f64| sheetcore::Cell {
        value: sheetcore::Value::Number(v),
        formula: None,
    };
    let cell_txt = |v: &str| sheetcore::Cell {
        value: sheetcore::Value::Text(v.into()),
        formula: None,
    };

    // 明细：部门相邻同值（分类汇总）+ 姓名-部门（快速填充）+ 一列月份序列（预测）
    let mut d = sheetcore::Sheet::new("明细");
    d.set(0, 0, cell_txt("部门"));
    d.set(0, 1, cell_txt("金额"));
    d.set(0, 2, cell_txt("姓名-部门"));
    d.set(0, 3, cell_txt("部门2"));
    let rows: [(&str, f64, &str, &str); 6] = [
        ("研发", 100.0, "张三-研发", "研发"),
        ("研发", 150.0, "李四-研发", ""),
        ("销售", 80.0, "王五-销售", ""),
        ("销售", 120.0, "赵六-销售", ""),
        ("行政", 60.0, "钱七-行政", ""),
        ("行政", 40.0, "孙八-行政", ""),
    ];
    for (i, (dept, amt, combo, ex)) in rows.iter().enumerate() {
        let r = i as u32 + 1;
        d.set(r, 0, cell_txt(dept));
        d.set(r, 1, cell_num(*amt));
        d.set(r, 2, cell_txt(combo));
        if !ex.is_empty() {
            d.set(r, 3, cell_txt(ex));
        }
    }
    wb.sheets[0] = d;

    // 一月 / 二月：按地区标签合并
    for (name, rows2) in [
        ("一月", [("北京", 100.0), ("上海", 50.0)]),
        ("二月", [("上海", 70.0), ("广州", 30.0)]),
    ] {
        let mut s = sheetcore::Sheet::new(name);
        s.set(0, 0, cell_txt("地区"));
        s.set(0, 1, cell_txt("销售额"));
        for (i, (k, v)) in rows2.iter().enumerate() {
            let r = i as u32 + 1;
            s.set(r, 0, cell_txt(k));
            s.set(r, 1, cell_num(*v));
        }
        wb.sheets.push(s);
    }
    // 预测：12 期近似线性 + 轻微波动
    let mut t = sheetcore::Sheet::new("销量");
    t.set(0, 0, cell_txt("月份"));
    t.set(0, 1, cell_txt("销量"));
    for i in 1..=12u32 {
        t.set(i, 0, cell_num(i as f64));
        let noise = [0.0, 2.0, -1.0, 1.5, -2.0, 0.5][(i as usize) % 6];
        t.set(i, 1, cell_num(100.0 + 10.0 * i as f64 + noise));
    }
    wb.sheets.push(t);

    sheetcore::xlsx::save(std::path::Path::new(&out), &wb)?;
    println!("已写出 {out}：{} 张表", wb.sheets.len());
    Ok(())
}
