use sheetcore::formula::recalc_all_force;
use sheetcore::model::{Cell, Sheet, Value, Workbook};
use std::path::Path;

fn main() {
    let out = std::env::args().nth(1).expect("output path");
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "Start 1".into();
    wb.sheets[0].set(0, 0, Cell { value: Value::Number(10.0), formula: None });
    let mut middle = Sheet::new("Middle");
    middle.set(0, 0, Cell { value: Value::Number(20.0), formula: None });
    wb.sheets.push(middle);
    let mut end = Sheet::new("End 3");
    end.set(0, 0, Cell { value: Value::Number(30.0), formula: None });
    wb.sheets.push(end);
    let mut other = Sheet::new("Other");
    other.set(0, 0, Cell { value: Value::Empty, formula: Some("=SUM('Start 1:End 3'!A1)".into()) });
    wb.sheets.push(other);
    recalc_all_force(&mut wb);
    sheetcore::xlsx::save(Path::new(&out), &wb).expect("save quoted 3d fixture");
    println!("{out}");
}
