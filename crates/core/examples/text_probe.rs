fn main() {
    for f in ["=VLOOKUP(A1,A:C,3,FALSE)", "=SUM(A:C)", "=VLOOKUP(A1,'-202506'!A:C,3,FALSE)", "=A1:B2"] {
        println!("{f} => {}", sheetcore::normalize_formula(f));
    }
}
