use sheetcore::{Workbook,Value,Cell};
use sheetcore::formula::{self,deps::Deps};
fn main(){
 let mut w=Workbook::new_blank();
 w.sheets[0].set(0,0,Cell{value:Value::Empty,formula:Some("=1/0".into())});
 w.sheets[0].set(0,1,Cell{value:Value::Number(1.0),formula:None});
 w.sheets[0].set(0,2,Cell{value:Value::Empty,formula:Some("=SUM(A1,B1)".into())});
 formula::recalc_all(&mut w); println!("full C1 {:?}",w.sheets[0].value(0,2));
 let d=Deps::build(&w);
 w.sheets[0].set(0,1,Cell{value:Value::Number(2.0),formula:None});
 formula::recalc_dirty(&mut w,&d,&[(0,0,1)]);println!("dirty C1 {:?}",w.sheets[0].value(0,2));
 formula::recalc_all(&mut w);println!("full_again C1 {:?}",w.sheets[0].value(0,2));
 let mut p=sheetcore::Sheet::new("s"); for (r,text) in [(0,"Group"),(1,"A"),(2,"A")]{p.set(r,0,Cell{value:Value::Text(text.into()),formula:None});}
 p.set(0,1,Cell{value:Value::Text("Value".into()),formula:None});p.set(1,1,Cell{value:Value::Number(1.0),formula:None});
 let spec=sheetcore::pivot::PivotSpec{source:(0,0,2,1),row_field:0,col_field:None,value_field:1,agg:sheetcore::pivot::Agg::Count,group:sheetcore::pivot::GroupBy::None,exclude:vec![]};
 println!("pivot count missing cell {:?}",sheetcore::pivot::compute(&p,&spec));
 let blank=Workbook::new_blank();for f in ["=A1=0","=A1=\"\"","=A1<0","=SUM({1;2}+{10,20})"]{println!("{} => {:?}",f,formula::eval_formula(&blank,0,f));}
}
