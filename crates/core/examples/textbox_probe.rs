//! 文本框排查/验收用：新建一张表 → 放一个文本框 → 另存。
//!
//! cargo run -p sheetcore --example textbox_probe -- <输出.xlsx>

fn main() -> anyhow::Result<()> {
    let out = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "textbox_probe_out.xlsx".into());
    let mut wb = sheetcore::model::Workbook::new_blank();
    wb.sheets[0].name = "文本框测试".into();
    for r in 0..3 {
        for c in 0..3 {
            wb.sheets[0].set(
                r,
                c,
                sheetcore::Cell {
                    value: sheetcore::Value::Number(((r + 1) * (c + 1)) as f64),
                    formula: None,
                },
            );
        }
    }
    let cols = sheetcore::layout::Axis::new(72.0);
    let rows = sheetcore::layout::Axis::new(19.0);
    let mut tb = sheetcore::textbox::TextBox {
        text: "第一个文本框\n第二行".into(),
        size: 16.0,
        bold: true,
        color: [0xC0, 0x00, 0x00],
        fill: Some([0xFF, 0xF2, 0xCC]),
        line: Some([0x00, 0x70, 0xC0]),
        align: 1,
        ..Default::default()
    };
    tb.set_rect_px(&cols, &rows, 240.0, 60.0, 220.0, 80.0);
    let mut tb2 = sheetcore::textbox::TextBox {
        text: "透明的那个".into(),
        size: 12.0,
        fill: None,
        line: None,
        ..Default::default()
    };
    tb2.id = 3;
    tb2.name = "文本框 2".into();
    tb2.set_rect_px(&cols, &rows, 240.0, 180.0, 160.0, 40.0);
    wb.sheets[0].textboxes.push(tb);
    wb.sheets[0].textboxes.push(tb2);
    wb.sheets[0].textboxes_dirty = true;
    sheetcore::xlsx::save(std::path::Path::new(&out), &wb)?;
    println!("已写出 {out}");
    let back = sheetcore::xlsx::load(std::path::Path::new(&out))?;
    for tb in &back.sheet(0).textboxes {
        let (x, y, w, h) = tb.rect_px(&cols, &rows);
        println!(
            "回读：{:?} 文字={:?} 字号={} 位置=({x:.0},{y:.0},{w:.0},{h:.0}) 填充={:?} 边框={:?}",
            tb.name, tb.text, tb.size, tb.fill, tb.line
        );
    }
    Ok(())
}
