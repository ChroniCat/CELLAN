fn main() {
    let xml = r#"<row r="2"><c r="A2"><v>2</v></c><c r="B2"><f>SUM(A1:A3*C1:C3)</f><v/></c><c r="C2"><v>20</v></c></row>"#;
    let a = sheetcore::arr::ArrayFml { range: (1,1,1,3), anchor: (1,1) };
    println!("{}", sheetcore::arr::apply_xml(xml, &[a]));
}
