use sheetcore::{Cell, Value, Workbook};

fn text(s: &str) -> Cell {
    Cell { value: Value::Text(s.into()), formula: None }
}

fn num(n: f64) -> Cell {
    Cell { value: Value::Number(n), formula: None }
}

fn formula(f: &str) -> Cell {
    Cell { value: Value::Empty, formula: Some(f.into()) }
}

fn main() {
    let out = std::env::args().nth(1).unwrap_or_else(|| "sample.xlsx".to_string());
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "报价单".to_string();

    {
        let s = &mut wb.sheets[0];
        s.set(0, 0, text("品名"));
        s.set(0, 1, text("数量"));
        s.set(0, 2, text("单价"));
        s.set(0, 3, text("金额"));

        s.set(1, 0, text("A4 复印纸"));
        s.set(1, 1, num(20.0));
        s.set(1, 2, num(23.5));
        s.set(1, 3, formula("=B2*C2"));

        s.set(2, 0, text("订书机"));
        s.set(2, 1, num(5.0));
        s.set(2, 2, num(18.0));
        s.set(2, 3, formula("=B3*C3"));

        s.set(3, 0, text("合计"));
        s.set(3, 1, formula("=SUM(B2:B3)"));
        s.set(3, 3, formula("=SUM(D2:D3)"));

        s.set(5, 0, text("含税（13%）"));
        s.set(5, 3, formula("=ROUND(D4*1.13,2)"));

        s.set(6, 0, text("最大单笔"));
        s.set(6, 3, formula("=MAX(D2:D3)"));

        s.set(7, 0, text("条数"));
        s.set(7, 3, formula("=COUNT(D2:D3)"));

        s.set(8, 0, text("今天"));
        s.set(8, 3, formula("=TEXT(TODAY(),\"yyyy-mm-dd\")"));

        s.set(10, 0, text("中文长文本用于检查列宽截断：内蒙古自治区呼和浩特市新城区"));
        s.set(11, 0, Cell { value: Value::Bool(true), formula: None });
        s.set(12, 0, num(3.14159));

        // 第二批函数演示
        s.set(14, 0, text("统计"));
        s.set(14, 3, formula("=NORM.S.DIST(1.96,TRUE)"));
        s.set(15, 0, text("工程"));
        s.set(15, 3, formula("=BIN2DEC(\"1010\")"));
        s.set(16, 0, text("实际利率"));
        s.set(16, 3, formula("=EFFECT(0.05,12)"));
        s.set(17, 0, text("文本截取"));
        s.set(17, 3, formula("=TEXTBEFORE(\"2026-11-06\",\"-\")"));
        s.set(18, 0, text("罗马数字"));
        s.set(18, 3, formula("=ROMAN(2026)"));
        s.set(19, 0, text("卡方临界值"));
        s.set(19, 3, formula("=ROUND(CHISQ.INV.RT(0.05,1),3)"));

        // 动态数组：溢出到右侧与下方
        s.set(21, 0, text("动态数组 →"));
        s.set(21, 1, formula("=SEQUENCE(3,3,1,1)"));
        s.set(25, 0, text("LET"));
        s.set(25, 3, formula("=LET(x,D4,y,x*1.13,y-560)"));
    }

    sheetcore::formula::recalc_all(&mut wb);
    sheetcore::xlsx::save(std::path::Path::new(&out), &wb).expect("保存失败");
    println!("已生成 {out}");
}
