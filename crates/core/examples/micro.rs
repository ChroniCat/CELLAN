use sheetcore::formula;
use sheetcore::{Cell, Value, Workbook};
use std::time::Instant;
fn main() {
    let mut wb = Workbook::new_blank();
    for r in 0..20000u32 {
        wb.sheets[0].set(r, 0, Cell { value: Value::Number(r as f64), formula: None });
        wb.sheets[0].set(r, 7, Cell { value: Value::Text(format!("华东{}", r % 5)), formula: None });
        wb.sheets[0].set(r, 5, Cell { value: Value::Number(r as f64 * 1.5), formula: None });
    }
    let f = "=SUMIF($H$1:$H$20000,\"华东0\",$F$1:$F$20000)";

    let t = Instant::now();
    for _ in 0..2000 { let _ = formula::parse(f); }
    println!("parse  : {:>7.1} µs/次", t.elapsed().as_secs_f64() * 1e6 / 2000.0);

    let t = Instant::now();
    for _ in 0..20 { let _ = formula::eval_formula(&wb, 0, f); }
    println!("eval   : {:>7.1} µs/次 (含 parse)", t.elapsed().as_secs_f64() * 1e6 / 20.0);

    // 列引用物化耗时
    let t = Instant::now();
    for _ in 0..20 { let _ = formula::eval_formula(&wb, 0, "=SUM($F$1:$F$20000)"); }
    println!("sum20k : {:>7.1} µs/次", t.elapsed().as_secs_f64() * 1e6 / 20.0);
}
