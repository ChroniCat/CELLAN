use sheetcore::model::Workbook;
fn main() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(0,0, sheetcore::model::Cell{ value: sheetcore::model::Value::Number(1.0), formula: None });
    wb.sheets[0].set(0,1, sheetcore::model::Cell{ value: sheetcore::model::Value::Number(2.0), formula: None });
    for f in ["=SUM(A1:A2 B1:B2)", "=TEXT(12.3,\"000.00\")", "=A1+A2", "=LAMBDA(x,x*x)(7)", "=LET(y,3,y*2)"] {
        println!("{f} => {:?}", sheetcore::formula::eval_formula(&wb, 0, f).display());
    }
}
