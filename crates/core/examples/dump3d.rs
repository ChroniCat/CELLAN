fn main() {
    for s in ["=SUM(First:Third!A1)", "=SUM(First:Third!A1:B1)", "=First:Third!A1"] {
        let wb = sheetcore::model::Workbook::new_blank();
        let raw = s.strip_prefix('=').unwrap_or(s);
        let exp = sheetcore::table::expand_structured(&wb, 0, raw);
        let norm = sheetcore::normalize_formula(exp.as_deref().unwrap_or(raw));
        println!("{s}\n  exp={exp:?}\n  norm={norm:?}\n  parse={:?}", sheetcore::formula::parse(&norm));
    }
}
