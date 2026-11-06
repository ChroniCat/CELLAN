fn main() {
    use umya_spreadsheet::{reader::xlsx::read, writer::xlsx::write};
    let p = std::path::Path::new("F:/Finance/work/notes.xlsx");
    let mut book = read(p).unwrap();
    let ws = book.get_sheet_by_name_mut("批注").unwrap();
    println!("原有批注: {}", ws.get_comments().len());
    for c in ws.get_comments() {
        println!("  {:?} {}: {:?}", c.get_coordinate(), c.get_author(), c.get_text().get_text());
    }
    let mut nc = umya_spreadsheet::Comment::default();
    let mut coord = umya_spreadsheet::Coordinate::default();
    coord.set_coordinate("C5");
    *nc.coordinate_mut() = coord;
    nc.set_author("Hermes");
    nc.set_text_string("新加批注");
    ws.add_comments(nc);
    write(&book, std::path::Path::new("F:/Finance/work/notes_out.xlsx")).unwrap();
    println!("已保存");
}
