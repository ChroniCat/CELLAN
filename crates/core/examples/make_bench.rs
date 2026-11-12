//! 生成性能基准工作簿：`cargo run --release -p sheetcore --example make_bench -- <输出路径> [行数] [列数] [迷你图数] [文本框数]`
//!
//! 用来量真实渲染成本，不是测试：数据是线性序列，行高列宽用默认值。

use sheetcore::model::{Cell, Value, Workbook};
use sheetcore::sparkline::{SparkKind, Sparkline};
use sheetcore::textbox::TextBox;
use sheetcore::Sheet;

fn col_letter(mut c: u32) -> String {
    let mut s = String::new();
    loop {
        s.insert(0, (b'A' + (c % 26) as u8) as char);
        if c < 26 {
            break;
        }
        c = c / 26 - 1;
    }
    s
}

fn main() {
    let mut args = std::env::args().skip(1);
    let out = args.next().unwrap_or_else(|| "bench.xlsx".into());
    let rows: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(20_000);
    let cols: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(20);
    let sparks: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(5_000);
    let boxes: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(100);

    let t0 = std::time::Instant::now();
    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "基准".into();
    let sheet: &mut Sheet = &mut wb.sheets[0];
    // 第 6 个参数：公式列数（这些列写 =A1+B1 这样的公式，用来量重算成本）
    let formula_cols: u32 = args.next().and_then(|s| s.parse().ok()).unwrap_or(0);
    for r in 0..rows {
        for c in 0..cols {
            let is_formula = c + formula_cols >= cols && c > 0;
            sheet.set(
                r,
                c,
                Cell {
                    value: Value::Number(((r as f64) * 1.5 + (c as f64) * 0.25).fract() * 100.0),
                    formula: if is_formula {
                        Some(format!("{}{}+1", col_letter(c - 1), r + 1).into())
                    } else {
                        None
                    },
                },
            );
        }
    }
    // 每个迷你图吃掉一行 5 个点，锚在最后一列右边
    let src_cols: u32 = cols.min(5);
    // 锚点铺在可见区里：前 6 列，按行往下排（这样首屏就能看到很多迷你图）
    let anchor_cols: u32 = cols.min(6).max(1);
    for i in 0..sparks as u32 {
        let r = i / anchor_cols;
        sheet.sparklines.push(Sparkline {
            r,
            c: i % anchor_cols,
            rect: (r, 0, r, src_cols),
            source_sheet: None,
            kind: match i % 3 {
                0 => SparkKind::Line,
                1 => SparkKind::Column,
                _ => SparkKind::WinLoss,
            },
            color: [0x37, 0x60, 0x92],
            markers: false,
            high: false,
            low: false,
            first: false,
            last: false,
            negative: false,
            axis: false,
            empty: Default::default(),
        });
    }
    for i in 0..boxes {
        sheet.textboxes.push(TextBox {
            id: 1000 + i as u32,
            name: format!("文本框 {i}"),
            from: ((i % 10) as u32, 190_500, (i / 10) as u32 * 2, 190_500),
            to: ((i % 10) as u32 + 2, 190_500, (i / 10) as u32 * 2 + 1, 190_500),
            text: format!("说明 {i}"),
            size: 11.0,
            bold: false,
            italic: false,
            color: [0, 0, 0],
            fill: None,
            line: Some([127, 127, 127]),
            align: 0,
            wrap: true,
        });
    }
    sheet.sparklines_dirty = true;
    sheet.textboxes_dirty = true;
    let t1 = std::time::Instant::now();
    match sheetcore::xlsx::save(std::path::Path::new(&out), &wb) {
        Ok(()) => {
            let sz = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
            println!(
                "生成 {out}：{} 格 / {} 迷你图 / {} 文本框，{} 字节；造模型 {:?}，写出 {:?}",
                (rows as u64) * (cols as u64),
                sparks,
                boxes,
                sz,
                t1 - t0,
                t1.elapsed()
            );
        }
        Err(e) => {
            eprintln!("写出失败: {e:#}");
            std::process::exit(1);
        }
    }
}
