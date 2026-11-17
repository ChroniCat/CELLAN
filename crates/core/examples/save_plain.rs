fn main() -> anyhow::Result<()> {
    let src = std::path::PathBuf::from(std::env::args().nth(1).unwrap());
    let dst = std::path::PathBuf::from(std::env::args().nth(2).unwrap());
    let _ = std::fs::remove_file(&dst);
    let wb = sheetcore::xlsx::load(&src)?;
    sheetcore::xlsx::save(&dst, &wb)?;
    println!("saved {}", dst.display());
    Ok(())
}
