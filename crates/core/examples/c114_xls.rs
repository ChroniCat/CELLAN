use sheetcore::formula::recalc_all_force;
use sheetcore::model::{Cell, Sheet, Value, Workbook};
use std::path::Path;

fn f(text: &str) -> Cell {
    Cell { value: Value::Empty, formula: Some(text.into()) }
}

fn main() {
    let out = std::env::args().nth(1).expect("output path");
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "First".into();
    wb.sheets[0].set(0, 0, Cell { value: Value::Number(10.0), formula: None });
    for (name, v) in [("Second", 20.0), ("Third", 30.0)] {
        let mut sh = Sheet::new(name);
        sh.set(0, 0, Cell { value: Value::Number(v), formula: None });
        wb.sheets.push(sh);
    }
    let mut other = Sheet::new("Other");
    other.set(0, 0, f("=SUM(First:Third!A1)"));
    other.set(1, 0, f("=SUM(First:Third!A1:A1)"));
    wb.sheets.push(other);
    recalc_all_force(&mut wb);
    sheetcore::xls_write::write(Path::new(&out), &wb).expect("write xls");
    println!("{out}");
}
