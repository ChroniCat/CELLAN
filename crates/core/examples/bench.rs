//! Run without arguments for throughput, or use `regression`, `depth <rows>`
//! and `profile <workbook>`. Set SC_PROFILE=1 for function timings.
use sheetcore::formula;
use sheetcore::{Cell, Value, Workbook};
use std::time::Instant;

#[cfg(all(windows, not(debug_assertions)))]
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

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
    if let Some(mode) = std::env::args().nth(1) {
        match mode.as_str() {
            "regression" => regression_probes(),
            "depth" => {
                let rows: u32 = std::env::args().nth(2).unwrap_or_else(|| "1000".into()).parse().unwrap();
                assert!(rows > 0);
                let mut wb = Workbook::new_blank();
                for row in 0..rows - 1 {
                    set_formula(&mut wb, row, 0, format!("=A{}+1", row + 2));
                }
                set_number(&mut wb, rows - 1, 0, 0.0);
                formula::recalc_all(&mut wb);
                assert_eq!(wb.sheets[0].value(0, 0), Value::Number(f64::from(rows - 1)));
                println!("depth {rows} OK");
            }
            "profile" => {
                let path = std::env::args_os().nth(2).expect("profile needs a workbook path");
                let mut wb = sheetcore::xlsx::load(std::path::Path::new(&path)).unwrap();
                let start = Instant::now();
                formula::recalc_all_force(&mut wb);
                println!("full recalc {:.3}s", start.elapsed().as_secs_f64());
            }
            _ => panic!("expected regression, depth <rows>, or profile <file>"),
        }
        return;
    }
    run("简单引用 =A+B", build(100_000), 100_000);
    run("SUM 滚动窗口(50)", build_sum_slices(20_000, 50), 20_000);
    run("VLOOKUP 精确匹配", build_lookup(10_000, 10_000), 10_000);
}

fn set_number(wb: &mut Workbook, r: u32, c: u32, n: f64) {
    wb.sheets[0].set(r, c, Cell { value: Value::Number(n), formula: None });
}

fn set_formula(wb: &mut Workbook, r: u32, c: u32, formula: String) {
    wb.sheets[0].set(r, c, Cell { value: Value::Empty, formula: Some(formula.into()) });
}

fn regression_probes() {
    for rows in [10_000, 20_000] {
        let mut wb = Workbook::new_blank();
        for r in 0..rows {
            set_number(&mut wb, r, 0, 1.0);
            set_formula(&mut wb, r, 1, format!("=SUM($A$1:A{})", r + 1));
        }
        let start = Instant::now();
        formula::recalc_all(&mut wb);
        println!("cumulative SUM {rows}: {:?}", start.elapsed());
        assert_eq!(wb.sheets[0].value(rows - 1, 1), Value::Number(rows.into()));

        for r in 1..rows {
            set_formula(&mut wb, r, 1, format!("=B{r}+1"));
        }
        let start = Instant::now();
        let deps = formula::deps::Deps::build(&wb);
        println!("dependency build {rows}: {:?}", start.elapsed());
        let start = Instant::now();
        let dirty = deps.closure(&[(0, 0, 0)]);
        println!("chain closure {rows}: {:?}", start.elapsed());
        assert_eq!(dirty.len(), rows as usize);
    }
    let mut wb = Workbook::new_blank();
    for r in 0..20_000 {
        set_number(&mut wb, r, 0, f64::from(r % 10));
        set_number(&mut wb, r, 1, 1.0);
    }
    for r in 0..1000 {
        set_formula(&mut wb, r, 2, format!("=SUMIFS(B1:B20000,A1:A20000,{})", r % 10));
    }
    let start = Instant::now();
    formula::recalc_all(&mut wb);
    println!("SUMIFS 1000 x 20000: {:?}", start.elapsed());
    assert_eq!(wb.sheets[0].value(999, 2), Value::Number(2000.0));

    let mut wb = Workbook::new_blank();
    for r in 0..10_000 {
        set_number(&mut wb, r, 0, (2 * r).into());
        set_number(&mut wb, r, 1, r.into());
    }
    for r in 0..2000 {
        set_formula(&mut wb, r, 2, format!("=VLOOKUP({},A1:B10000,2)", r * 10 + 1));
    }
    let start = Instant::now();
    formula::recalc_all(&mut wb);
    println!("approximate VLOOKUP 2000 x 10000: {:?}", start.elapsed());
    assert_eq!(wb.sheets[0].value(1999, 2), Value::Number(9995.0));

    let mut wb = Workbook::new_blank();
    for r in 0..200_000 {
        set_number(&mut wb, r, 0, r.into());
        wb.sheets[0].set(r, 1, Cell { value: Value::Text(format!("t{r}").into()), formula: None });
    }
    let start = Instant::now();
    wb.sheets[0].insert_rows(199_990, 1);
    println!("bottom insert 400000 cells: {:?}, cleared={}", start.elapsed(), wb.sheets[0].cleared.len());
    assert_eq!(wb.sheets[0].value(200_000, 0), Value::Number(199999.0));
}
