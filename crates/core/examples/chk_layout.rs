//! 打印一张表的布局：列宽、行高、隐藏行、冻结、合并
use sheetcore::xlsx;
use std::path::Path;

fn main() -> anyhow::Result<()> {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "F:/Finance/work/excel-1900-initial.xlsx".into());
    let wb = xlsx::load(Path::new(&path))?;
    for (i, sh) in wb.sheets.iter().enumerate() {
        println!("sheet[{i}] name={:?} cells={}", sh.name, sh.len());
        println!("  freeze={:?}", sh.freeze);
        println!("  hidden_rows={:?}", {
            let mut v: Vec<u32> = sh.hidden_rows.iter().copied().collect();
            v.sort();
            v
        });
        println!("  hidden_cols={:?}", {
            let mut v: Vec<u32> = sh.hidden_cols.iter().copied().collect();
            v.sort();
            v
        });
        let widths: Vec<f32> = (0..8).map(|c| sh.cols.size(c)).collect();
        println!("  col sizes A..H = {:?}", widths);
        let heights: Vec<f32> = (0..8).map(|r| sh.rows.size(r)).collect();
        println!("  row sizes 1..8 = {:?}", heights);
        println!("  merges={:?}", sh.merges);
    }
    Ok(())
}
