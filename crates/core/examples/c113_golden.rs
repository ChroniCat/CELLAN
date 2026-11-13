use sheetcore::formula::recalc_all_force;
use sheetcore::model::{Cell, Sheet, Value, Workbook};
use std::path::Path;

fn f(text: &str) -> Cell {
    Cell { value: Value::Empty, formula: Some(text.into()) }
}

fn main() {
    let out = std::env::args().nth(1).expect("output path");
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Data".into();
    for (r, n) in [10.0, 20.0, 30.0].iter().enumerate() {
        wb.sheets[0].set(r as u32, 0, Cell { value: Value::Number(*n), formula: None });
    }
    wb.sheets[0].set(0, 1, Cell { value: Value::Number(20.0), formula: None });
    wb.sheets[0].set(0, 2, Cell { value: Value::Number(30.0), formula: None });
    wb.sheets[0].set(1, 2, f("=@A1"));
    wb.sheets[0].set(1, 3, f("=@A1:A3"));
    wb.sheets[0].set(2, 3, f("=@A1:A3"));
    wb.sheets[0].set(3, 3, f("=@A1:A3"));
    wb.sheets[0].set(1, 1, f("=@A1:C1"));
    wb.sheets[0].set(3, 1, f("=@A1:C1"));
    wb.sheets[0].set(1, 4, f("=@A1:C1"));
    wb.sheets[0].set(3, 4, f("=@A1:C1"));
    wb.sheets[0].set(1, 5, f("=@A1:C3"));
    let mut other = Sheet::new("Other");
    other.set(1, 1, f("=@Data!A1:A3"));
    other.set(3, 1, f("=@Data!A1:A3"));
    wb.sheets.push(other);
    recalc_all_force(&mut wb);
    sheetcore::xlsx::save(Path::new(&out), &wb).expect("save fixture");
    println!("{out}");
}
