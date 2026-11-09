//! 加载一个工作簿，打印表数、格子数、自定义列宽/行高、合并数。
use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let p: PathBuf = std::env::args().nth(1).expect("用法: probe <文件>").into();
    let t = std::time::Instant::now();
    let wb = sheetcore::xlsx::load(&p)?;
    let mut cells = 0usize;
    for s in &wb.sheets {
        cells += s.len();
        println!(
            "表 {:?}: {} 格, 合并 {}, 列宽自定义 {}, 行高自定义 {}",
            s.name,
            s.len(),
            s.merges.len(),
            s.cols.len(),
            s.rows.len()
        );
    }
    println!("合计 {cells} 格, {} 表, 加载 {:?}", wb.sheets.len(), t.elapsed());
    Ok(())
}
