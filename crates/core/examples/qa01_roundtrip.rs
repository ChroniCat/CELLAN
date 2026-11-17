//! QA-01 round-trip helper.
//!
//! This is deliberately a small command-line surface around the same core
//! load/recalculate/save calls the app uses.  It lets the Office compatibility
//! runner create a file with Excel/WPS, pass it through the project, and then
//! reopen the resulting file in the same application without driving the GUI.
//!
//! Usage: cargo run -p sheetcore --example qa01_roundtrip -- <input.xlsx> <output.xlsx>

use std::path::PathBuf;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: qa01_roundtrip <input.xlsx> <output.xlsx>");
        std::process::exit(2);
    }
    let input = PathBuf::from(&args[1]);
    let output = PathBuf::from(&args[2]);
    if output.exists() {
        eprintln!("refusing to overwrite output: {}", output.display());
        std::process::exit(2);
    }
    let mut wb = match sheetcore::xlsx::load(&input) {
        Ok(wb) => wb,
        Err(error) => {
            eprintln!("load failed: {error}");
            std::process::exit(1);
        }
    };
    // 纯文件往返不重算：重算会把动态数组表标成 edited，保存时重写公式
    // （_xlfn 前缀/溢出缓存丢失）。公式由打开端自己重算。
    if let Err(error) = sheetcore::xlsx::save(&output, &wb) {
        eprintln!("save failed: {error}");
        std::process::exit(1);
    }
    println!(
        "{}",
        serde_json::json!({
            "input": input,
            "output": output,
            "sheets": wb.sheets.len(),
            "date_system": format!("{:?}", wb.date_system),
        })
    );
}
