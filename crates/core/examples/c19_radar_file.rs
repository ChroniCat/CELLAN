fn main() -> anyhow::Result<()> {
    let src = std::path::PathBuf::from(std::env::args().nth(1).unwrap());
    let dst = std::path::PathBuf::from(std::env::args().nth(2).unwrap());
    let _ = std::fs::remove_file(&dst);
    let wb = sheetcore::xlsx::load(&src)?;
    sheetcore::xlsx::save(&dst, &wb)?;
    let mut ch = sheetcore::chart_write::chart_from_selection(
        &wb.sheets[0], "数据", (0, 0, 4, 1), "雷达", sheetcore::charts::ChartKind::Radar);
    ch.radar_style = "filled".into();
    sheetcore::chart_write::insert_chart(&dst, "数据", &ch)?;
    println!("wrote {}", dst.display());
    Ok(())
}
