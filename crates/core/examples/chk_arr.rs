fn main() {
    let src = std::path::Path::new("F:/Finance/work/array.xlsx");
    let wb = sheetcore::xlsx::load(src).unwrap();
    println!("arrays={:?}", wb.sheets[0].arrays);
    let out = std::path::Path::new("F:/Finance/work/array_out.xlsx");
    let _ = std::fs::remove_file(out);
    sheetcore::xlsx::save(out, &wb).unwrap();
    println!("scan after save: {:?}", sheetcore::arr::scan(out));
    use std::io::Read;
    let f = std::fs::File::open(out).unwrap();
    let mut z = zip::ZipArchive::new(f).unwrap();
    let mut xml = String::new();
    z.by_name("xl/worksheets/sheet1.xml").unwrap().read_to_string(&mut xml).unwrap();
    let i = xml.find("B2").unwrap_or(0);
    println!("片段: {}", &xml[i.saturating_sub(40)..(i+120).min(xml.len())]);
}
