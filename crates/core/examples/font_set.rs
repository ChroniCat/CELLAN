//! 给文件里的一格（或整块）设字体再另存，用来做「Excel 里读得到字体名」的验收。
//!
//! 用法：
//!   cargo run --release -p sheetcore --example font_set -- in.xlsx out.xlsx B3 宋体
//!   cargo run --release -p sheetcore --example font_set -- in.xlsx out.xlsx B3 宋体 3x2
//!
//! 第五个参数可选，`行x列` 表示从该格向右下扩一块。

use sheetcore::style::CellStyle;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 4 {
        eprintln!("用法: font_set <in.xlsx> <out.xlsx> <格名> <字体名> [行x列]");
        std::process::exit(2);
    }
    let (inp, outp, cell, font) = (&args[0], &args[1], &args[2], &args[3]);
    let (dh, dw) = match args.get(4).and_then(|s| s.split_once('x')) {
        Some((a, b)) => (
            a.parse::<u32>().unwrap_or(1),
            b.parse::<u32>().unwrap_or(1),
        ),
        None => (1, 1),
    };
    let spec = if cell.contains(':') { cell.clone() } else { format!("{cell}:{cell}") };
    let (r1, c1, _, _) =
        sheetcore::table::parse_a1(&spec).unwrap_or_else(|| panic!("格名不合法：{cell}"));
    let t = std::time::Instant::now();
    let mut wb = sheetcore::xlsx::load(std::path::Path::new(inp)).expect("读入失败");
    eprintln!("[font_set] 读入 {:.1}s", t.elapsed().as_secs_f64());

    for r in r1..r1 + dh {
        for c in c1..c1 + dw {
            let mut st: CellStyle = wb.styles.get(wb.sheets[0].style_of(r, c)).clone();
            st.font = font.clone();
            let id = wb.styles.intern(st);
            wb.sheets[0].set_style(r, c, id);
        }
    }
    let t2 = std::time::Instant::now();
    sheetcore::xlsx::save(std::path::Path::new(outp), &wb).expect("保存失败");
    eprintln!(
        "[font_set] 写盘 {:.1}s → {outp}（{cell} 起 {dh}x{dw} 设为 {font}）",
        t2.elapsed().as_secs_f64()
    );
}
