use std::io::Read;
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let (src, dst, part, from) = (&a[1], &a[2], &a[3], &a[4]);
    std::fs::copy(src, dst).unwrap();
    let mut zip = zip::ZipArchive::new(std::fs::File::open(from).unwrap()).unwrap();
    let mut f = zip.by_name(part).unwrap();
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).unwrap();
    drop(f); drop(zip);
    sheetcore::patch::append_entry(std::path::Path::new(dst), part, &buf).unwrap();
    println!("已把 {part} 换成 {from} 的版本");
}
