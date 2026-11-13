//! C16.1 证据辅助：把合成格式夹具导出为 PDF，供 pdftotext 逐页核对。
use std::path::Path;

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let src = args.next().expect("usage: c16_pdf <xlsx> <pdf> [font.ttf]");
    let out = args.next().expect("usage: c16_pdf <xlsx> <pdf> [font.ttf]");
    let font = args.next().unwrap_or_else(|| {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../assets/fonts/subset/NotoSansSC-Regular.subset.ttf")
            .display()
            .to_string()
    });
    let wb = sheetcore::xlsx::load(Path::new(&src))?;
    let pages = sheetcore::print::export_pdf_from(&wb, 0, Path::new(&out), Path::new(&font), Some(Path::new(&src)))?;
    println!("pages={pages} out={out}");
    Ok(())
}
