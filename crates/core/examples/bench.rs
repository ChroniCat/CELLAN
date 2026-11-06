//! 公式重算性能基准：三种真实负载，测 real workflow 的单线程吞吐
use sheetcore::formula;
use sheetcore::{Cell, Value, Workbook};
use std::time::Instant;

fn build(n: usize) -> Workbook {
    let mut wb = Workbook::new_blank();
    for r in 0..n as u32 {
        wb.sheets[0].set(r, 0, Cell { value: Value::Number(r as f64), formula: None });
        wb.sheets[0].set(r, 1, Cell { value: Value::Number((r % 97) as f64), formula: None });
        wb.sheets[0].set(r, 2, Cell {
            value: Value::Number(0.0),
            formula: Some(format!("=A{}+B{}", r + 1, r + 1).into()),
        });
    }
    wb
}

fn build_sum_slices(rows: usize, window: usize) -> Workbook {
    let mut wb = Workbook::new_blank();
    for r in 0..rows as u32 {
        wb.sheets[0].set(r, 0, Cell { value: Value::Number((r % 100) as f64), formula: None });
        let end = (r as usize).saturating_sub(window) + 1;
        wb.sheets[0].set(r, 3, Cell {
            value: Value::Number(0.0),
            formula: Some(format!("=SUM(A{}:A{})", end.max(1), r + 1).into()),
        });
    }
    wb
}

fn build_lookup(rows: usize, probes: usize) -> Workbook {
    let mut wb = Workbook::new_blank();
    for r in 0..rows as u32 {
        wb.sheets[0].set(r, 5, Cell { value: Value::Number((r * 3) as f64), formula: None });
        wb.sheets[0].set(r, 6, Cell { value: Value::Text(format!("name{r}").into()), formula: None });
    }
    for i in 0..probes as u32 {
        wb.sheets[0].set(i, 8, Cell {
            value: Value::Number(0.0),
            formula: Some(format!("=VLOOKUP({},F1:G{},2,FALSE)", (i * 3) % (rows as u32), rows).into()),
        });
    }
    wb
}

fn run(label: &str, mut wb: Workbook, expected: usize) {
    let t = Instant::now();
    formula::recalc_all(&mut wb);
    let dt = t.elapsed().as_secs_f64();
    println!(
        "{label:<28} {expected:>8} 个公式   {:>7.2} s   {:>9.0} 格/秒",
        dt,
        expected as f64 / dt
    );
}

fn main() {
    run("简单引用 =A+B", build(100_000), 100_000);
    run("SUM 滚动窗口(50)", build_sum_slices(20_000, 50), 20_000);
    run("VLOOKUP 精确匹配", build_lookup(10_000, 10_000), 10_000);
}
