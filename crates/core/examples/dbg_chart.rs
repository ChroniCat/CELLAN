fn main() {
    let wb = sheetcore::xlsx::load(std::path::Path::new("F:/Finance/work/chart.xlsx")).unwrap();
    for sh in &wb.sheets {
        println!("表 {} 图表 {}", sh.name, sh.charts.len());
        for c in &sh.charts {
            println!("  {:?} 标题={:?} 图例={} 锚点={:?}", c.kind, c.title, c.legend, c.anchor);
            for s in &c.series {
                println!("    系列 {:?} 类别={:?} 值={:?} 引用 name={:?} cat={:?} val={:?}", s.name, s.cats, s.vals, s.name_ref, s.cat_ref, s.val_ref);
            }
        }
    }
}
