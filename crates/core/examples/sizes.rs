use sheetcore::{Cell, Value};
fn main() {
    println!("Value  = {} 字节", std::mem::size_of::<Value>());
    println!("Cell   = {} 字节", std::mem::size_of::<Cell>());
    println!("CellRef= {} 字节", std::mem::size_of::<sheetcore::CellRef>());
}
