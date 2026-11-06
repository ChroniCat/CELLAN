fn main() {
    let wb = sheetcore::xlsx::load(std::path::Path::new("F:/Finance/work/cf.xlsx")).unwrap();
    let sheet = wb.sheet(0);
    let cf = &sheet.cf;
    let mut st = sheetcore::cf::CfStats::default();
    sheetcore::cf::compute_stats(cf, sheet, &mut st);
    for (r, c) in [(1u32, 3u32), (2, 3), (3, 3), (4, 3)] {
        let ov = sheetcore::cf::overlay_at(cf, &st, sheet, r, c, |f, dr, dc| {
            let shifted = sheetcore::cf::shift_formula(f.trim_start_matches('='), dr, dc);
            match sheetcore::formula::parse(&shifted) {
                Ok(n) => {
                    let mut calc = sheetcore::formula::Calc::new(&wb);
                    matches!(calc.eval_node(&n, 0), sheetcore::formula::FValue::Bool(true))
                }
                Err(_) => false,
            }
        });
        println!("({r},{c}) 值={:?} 叠加 fill={:?} color={:?}", sheet.get(r, c).map(|x| x.value.display()), ov.fill, ov.color);
    }
}
