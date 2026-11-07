fn main() {
    let src = std::path::Path::new("F:/Finance/work/chart.xlsx");
    let out = std::path::Path::new("F:/Finance/work/chart_resaved.xlsx");
    let wb = sheetcore::xlsx::load(src).unwrap();
    let _ = std::fs::remove_file(out);
    sheetcore::xlsx::save(out, &wb).unwrap();
    for (label, p) in [("原文件", src), ("存回后", out)] {
        let z = zip::ZipArchive::new(std::fs::File::open(p).unwrap()).unwrap();
        let mut names: Vec<String> = (0..z.len()).map(|i| z.name_for_index(i).unwrap().to_string()).collect();
        names.sort();
        println!("{label} ({} 项):", names.len());
        for n in names.iter().filter(|n| n.contains("chart") || n.contains("drawing") || n.contains("media")) {
            println!("   {n}");
        }
    }
}
