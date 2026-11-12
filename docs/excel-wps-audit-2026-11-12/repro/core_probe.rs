use sheetcore::{Workbook,Sheet,Value,Cell};
fn main(){
 let mut wb=Workbook::new_blank();
 wb.sheets.push(Sheet::new("Sheet2")); wb.sheets.push(Sheet::new("Sheet3"));
 for (i,s) in wb.sheets.iter_mut().enumerate(){s.set(0,0,Cell{value:Value::Number((i+1) as f64),formula:None});}
 for f in ["=SEQUENCE(2,2)+1","=SEQUENCE(3,1)*2","=SUM(Sheet1:Sheet3!A1)","=SUM(A1#)","=@A1","=SUM(1:1)","=DAY(60)","=TEXT(60,\"yyyy-mm-dd\")","=NOW()","=TODAY()"] { println!("{} => {:?}",f,sheetcore::formula::eval_formula(&wb,0,f)); }
 let mut d=sheetcore::dv::DataValidation::default(); d.kind=sheetcore::dv::DvKind::List; d.f1="=NamedList".into(); println!("DV NamedList accepts arbitrary: {}",sheetcore::dv::validate(&d,&Value::Text("unexpected".into()),&wb.sheets[0],0,0,|_,_,_|false));
 d.kind=sheetcore::dv::DvKind::Whole; d.op=sheetcore::dv::DvOp::GreaterEq; d.f1="=$A$1".into(); println!("DV >= A1(1) accepts 0: {}",sheetcore::dv::validate(&d,&Value::Number(0.0),&wb.sheets[0],0,1,|_,_,_|false));
 println!("normalize string: {}",sheetcore::normalize_formula("=\"_xlfn.TEXT\""));
 if let Some(path)=std::env::args().nth(1){let result=sheetcore::xlsx::load(std::path::Path::new(&path));println!("load: {:?}",result.as_ref().map(|w|w.sheets.iter().map(|s|(s.name.clone(),s.value(0,0),s.value(0,1))).collect::<Vec<_>>()));if let Ok(w)=result{println!("format A1 date: {}",sheetcore::style::format_value(&w.sheets[0].value(0,0),"yyyy-mm-dd"));}}
}
