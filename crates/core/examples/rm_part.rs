fn main() {
    let a: Vec<String> = std::env::args().collect();
    sheetcore::patch::remove_entry(std::path::Path::new(&a[1]), &a[2]).unwrap();
    println!("已删除 {}", a[2]);
}
