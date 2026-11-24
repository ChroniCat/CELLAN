fn main() {
    use sheetcore::{model::CellRef, notes::Note, xlsx};
    let p = std::path::Path::new("F:/Finance/work/notes.xlsx");
    let mut wb = xlsx::load(p).unwrap();
    let ws = wb.sheets.iter_mut().find(|s| s.name == "批注").unwrap();
    println!("原有批注: {}", ws.notes.len());
    for (pos, n) in &ws.notes {
        println!("  ({},{}) {}: {:?}", pos.row, pos.col, n.author, n.text);
    }
    ws.notes.insert(
        CellRef::new(4, 2),
        Note {
            r: 4,
            c: 2,
            author: "Hermes".into(),
            text: "新加批注".into(),
        },
    );
    ws.notes_dirty = true;
    xlsx::save_atomic(std::path::Path::new("F:/Finance/work/notes_out.xlsx"), &wb).unwrap();
    println!("已保存");
}
