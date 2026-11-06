use sheetcore::formula;
use sheetcore::{Cell, Value, Workbook};
fn main() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(0, 0, Cell { value: Value::Number(1.0), formula: None });
    wb.sheets[0].set(0, 1, Cell { value: Value::Number(0.0), formula: Some("=A1*2".into()) });
    formula::recalc_all(&mut wb);
    println!("B1 cell value = {:?}", wb.sheets[0].get(0, 1).map(|c| c.value.clone()));
    println!("B1 eval_cell  = {}", formula::eval_cell(&wb, 0, 0, 1).display());
    println!("B1 formula?   = {:?}", wb.sheets[0].get(0, 1).and_then(|c| c.formula.clone()));
}
