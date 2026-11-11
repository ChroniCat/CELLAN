//! 排查用：加载一个文件 → 加一张表 → 另存 → 打印结果里有几张表。
//! 用法：cargo run -p sheetcore --example addsheet_probe -- <输入.xlsx> <输出.xlsx>

use sheetcore::model::{Cell, Sheet, Value};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("用法: addsheet_probe <输入.xlsx> <输出.xlsx>");
        return;
    }
    let src = std::path::PathBuf::from(&args[1]);
    let out = std::path::PathBuf::from(&args[2]);
    let mut wb = sheetcore::xlsx::load(&src).expect("加载失败");
    println!(
        "载入 {:?} 表: {:?}",
        src,
        wb.sheets.iter().map(|s| (s.name.clone(), s.source_part.clone())).collect::<Vec<_>>()
    );
    wb.path = Some(src.clone());
    let mut sh = Sheet::new("Probe");
    sh.set(
        0,
        0,
        Cell {
            value: Value::Number(42.0),
            formula: None,
        },
    );
    let idx = wb.sheets.len();
    assert!(sheetcore::sheet_ops::insert_sheet(&mut wb, idx, sh));
    // 和 app 的 finish_sheet_change 一样先重算
    sheetcore::formula::recalc_all(&mut wb);
    let _ = sheetcore::formula::deps::Deps::build(&wb);
    sheetcore::xlsx::save_atomic(&out, &wb).expect("保存失败");
    let back = sheetcore::xlsx::load(&out).expect("回读失败");
    println!(
        "写盘 {:?} 表: {:?}",
        out,
        back.sheets.iter().map(|s| s.name.clone()).collect::<Vec<_>>()
    );
}
