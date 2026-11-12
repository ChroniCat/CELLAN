//! 迷你图排查/验收用：读基准文件 → 改一条 + 加一条 → 另存。
//!
//! cargo run -p sheetcore --example spark_probe -- <输入.xlsx> <输出.xlsx>
//! 输出文件可用 Excel 打开核对 SparklineGroups（本仓库用 COM 回读）。

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let src = args.next().unwrap_or_else(|| {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/data/sparklines.xlsx")
            .to_string_lossy()
            .to_string()
    });
    let out = args.next().unwrap_or_else(|| "spark_probe_out.xlsx".to_string());
    let mut wb = sheetcore::xlsx::load(std::path::Path::new(&src))?;
    println!("读入 {src}：{} 张表", wb.sheets.len());
    for (i, s) in wb.sheets.iter().enumerate() {
        println!("  表[{i}] {} 迷你图 {} 条", s.name, s.sparklines.len());
        for sp in &s.sparklines {
            println!(
                "    {} {}  ←  {}  ({} 点, 颜色 {:02X?})",
                sheetcore::model::cell_name(sp.r, sp.c),
                sp.kind.label(),
                sp.data_ref(&s.name),
                sp.point_count(),
                sp.color
            );
        }
    }
    let sh = &mut wb.sheets[0];
    if let Some(sp) = sh.sparklines.iter_mut().find(|s| (s.r, s.c) == (0, 5)) {
        sp.color = [0x00, 0xB0, 0x50];
        sp.high = true;
        sp.low = true;
    }
    let mut add = sheetcore::sparkline::Sparkline {
        r: 0,
        c: 7,
        rect: (2, 0, 2, 4),
        kind: sheetcore::sparkline::SparkKind::WinLoss,
        ..Default::default()
    };
    add.source_sheet = Some(sh.name.clone());
    sh.sparklines.push(add);
    sh.sparklines_dirty = true;
    sheetcore::xlsx::save(std::path::Path::new(&out), &wb)?;
    println!("另存 → {out}");
    let back = sheetcore::xlsx::load(std::path::Path::new(&out))?;
    println!("回读：{} 条迷你图", back.sheet(0).sparklines.len());
    Ok(())
}
