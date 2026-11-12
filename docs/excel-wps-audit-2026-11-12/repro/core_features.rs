use sheetcore::{Workbook,Value,Cell};
fn main(){
 let mut w=Workbook::new_blank();
 w.sheets[0].tables.push(sheetcore::table::Table{name:"Sales".into(),display:"Sales".into(),range:(0,0,2,0),columns:vec!["Amount".into()],header_row:true,totals_row:false});
 w.sheets[0].set(1,0,Cell{value:Value::Number(10.0),formula:None});w.sheets[0].set(2,0,Cell{value:Value::Number(20.0),formula:None});
 for f in ["=SUM(Sales[Amount])","=Sales[Amount]","=SUM(Sales[[#Data],[Amount]])","=\"Sales[Amount]\""]{println!("{} => {:?}",f,sheetcore::formula::eval_formula(&w,0,f));}
 let path=std::path::Path::new("target/audit-core/custom-props.xlsx");sheetcore::xlsx::save(path,&w).unwrap();
 sheetcore::patch::append_entry(path,"docProps/custom.xml",br#"<Properties xmlns="http://schemas.openxmlformats.org/officeDocument/2006/custom-properties" xmlns:vt="http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes"><property fmtid="{D5CDD505-2E9C-101B-9397-08002B2CF9AE}" pid="2" name="Customer"><vt:lpwstr>original-metadata</vt:lpwstr></property></Properties>"#).unwrap();
 let src=sheetcore::external::Source{name:"_sc_src_1".into(),kind:sheetcore::external::Kind::Csv{path:"synthetic.csv".into(),delim:',',header:true},at:(0,0),rows:2,cols:2};
 sheetcore::external::write_sources(path,&[src]).unwrap();
 let xml=sheetcore::fixups::read_part_file(path,"docProps/custom.xml").unwrap();println!("custom original kept: {}",String::from_utf8_lossy(&xml).contains("original-metadata"));
 std::fs::write("target/audit-core/multiline.csv","Name,Note\nA,\"line1\nline2\"\n").unwrap();let mut s=sheetcore::Sheet::new("csv");let rc=sheetcore::external::import_csv(&mut s,"target/audit-core/multiline.csv",',',true,(0,0)).unwrap();println!("csv refreshed {:?},B2={:?},A3={:?}",rc,s.value(1,1),s.value(2,0));
}
