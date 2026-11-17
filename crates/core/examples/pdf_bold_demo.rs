//! 生成一张带粗体/常规/斜体/多行文字的 PDF，供 UI-14.2 取证：
//! 粗体走第二条嵌入字体（/F2），行高按 ascender/descender 算。
//!
//! 运行：cargo run -p sheetcore --example pdf_bold_demo -- <输出.pdf>

use sheetcore::model::{Cell, Value, Workbook};
use sheetcore::pdf::Font;
use sheetcore::style::CellStyle;

fn main() -> anyhow::Result<()> {
    let out = std::env::args().nth(1).unwrap_or_else(|| "pdf_bold_demo.pdf".into());
    let mut wb = Workbook::new_blank();
    let title = wb.styles.intern(CellStyle { bold: true, size: 18.0, ..Default::default() });
    let plain = wb.styles.intern(CellStyle { size: 12.0, ..Default::default() });
    let wrap = wb.styles.intern(CellStyle { size: 11.0, wrap: true, ..Default::default() });
    wb.sheet_mut(0).set(
        0,
        0,
        Cell { value: Value::Text("粗体标题 Bold 700".into()), formula: None },
    );
    wb.sheet_mut(0).set(
        1,
        0,
        Cell { value: Value::Text("常规正文 regular 400".into()), formula: None },
    );
    wb.sheet_mut(0).set(
        3,
        0,
        Cell {
            value: Value::Text("换行第一段很长的文字用于确认行高按字体度量走".into()),
            formula: None,
        },
    );
    wb.sheet_mut(0).set_style(0, 0, title);
    wb.sheet_mut(0).set_style(1, 0, plain);
    wb.sheet_mut(0).set_style(3, 0, wrap);
    wb.sheet_mut(0).cols.set(0, 240.0);
    for r in 0..5 {
        wb.sheet_mut(0).rows.set(r, 30.0);
    }

    let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts/subset");
    let regular = Font::load(&base.join("NotoSansSC-Regular.subset.ttf"))?;
    let bold = Font::load(&base.join("NotoSansSC-Bold.subset.ttf"))?;
    let n = sheetcore::print::export_pdf_pages_with_fonts(
        &wb,
        0,
        std::path::Path::new(&out),
        regular,
        Some(bold),
        None,
        None,
    )?;
    println!("wrote {out} pages={n}");
    Ok(())
}
