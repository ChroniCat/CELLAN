fn main() {
    let wb = sheetcore::xlsx::load(std::path::Path::new("F:/Finance/work/icons.xlsx")).unwrap();
    let sheet = wb.sheet(0);
    let cf = &sheet.cf;
    let mut st = sheetcore::cf::CfStats::default();
    sheetcore::cf::compute_stats(cf, sheet, &mut st);
    for r in 1..=6u32 {
        for c in 0..=1u32 {
            let ov = sheetcore::cf::overlay_at(cf, &st, sheet, r, c, |_, _, _| false);
            println!("({r},{c}) 值={:?} 图标={:?}", sheet.get(r, c).map(|x| x.value.display()), ov.icon);
        }
    }
}
