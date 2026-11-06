fn main() {
    let mut wb = sheetcore::xlsx::load(std::path::Path::new("F:/Finance/work/table.xlsx")).unwrap();
    let s = wb.sheet(0);
    println!("前: (0,0)={:?} (2,0)={:?} 格数={}", s.get(0,0).map(|c| c.value.display()), s.get(2,0).map(|c| c.value.display()), s.len());
    wb.sheet_mut(0).apply_structural(&sheetcore::structural::Structural::InsertRows { at: 0, n: 2 });
    let s = wb.sheet(0);
    println!("后: (0,0)={:?} (2,0)={:?} (3,0)={:?} 格数={}", s.get(0,0).map(|c| c.value.display()), s.get(2,0).map(|c| c.value.display()), s.get(3,0).map(|c| c.value.display()), s.len());
}
