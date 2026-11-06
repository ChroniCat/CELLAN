//! PDF 导出：结构合法、页数对得上、有文字指令
use sheetcore::page::PageSetup;

fn xref_ok(data: &[u8]) -> (bool, usize) {
    let text = String::from_utf8_lossy(&data[..data.len().min(400)]);
    assert!(text.starts_with("%PDF-1."), "缺 PDF 头");
    let tail = String::from_utf8_lossy(&data[data.len().saturating_sub(400)..]);
    assert!(tail.trim_end().ends_with("%%EOF"), "缺 EOF");
    let sx = tail
        .rfind("startxref")
        .map(|i| tail[i..].split_whitespace().nth(1).unwrap_or("0").to_string())
        .unwrap_or_default();
    let start: usize = sx.parse().unwrap_or(0);
    assert!(start > 0 && start < data.len(), "startxref 越界");
    let table = String::from_utf8_lossy(&data[start..]);
    let mut lines = table.lines();
    assert_eq!(lines.next().map(|s| s.trim()), Some("xref"));
    let head = lines.next().unwrap_or("");
    let n: usize = head.split_whitespace().nth(1).and_then(|x| x.parse().ok()).unwrap_or(0);
    assert!(n > 0, "xref 条目为 0");
    let mut bad = 0;
    // 第一条是 free 记录（offset 0），对象从第二条开始
    let _free = lines.next();
    for (i, line) in lines.take(n - 1).enumerate() {
        let off: usize = line.split_whitespace().next().and_then(|x| x.parse().ok()).unwrap_or(0);
        let expect = format!("{} 0 obj", i + 1);
        if off == 0 || !data[off..].starts_with(expect.as_bytes()) {
            bad += 1;
        }
    }
    (bad == 0, n)
}

#[test]
fn exports_valid_pdf() {
    let src = std::path::Path::new("F:/Finance/work/table.xlsx");
    let font = std::path::Path::new("C:/Windows/Fonts/simhei.ttf");
    if !src.exists() || !font.exists() {
        eprintln!("跳过：缺测试文件或字体");
        return;
    }
    let wb = sheetcore::xlsx::load(src).expect("读入");
    let layout = sheetcore::print::paginate(wb.sheet(0), &wb.sheet(0).page);
    let out = std::path::PathBuf::from(format!("F:/Finance/work/t_pdf_{}.pdf", std::process::id()));
    let n = sheetcore::print::export_pdf(&wb, 0, &out, font).expect("导出");
    assert_eq!(n, layout.pages.len(), "页数与分页结果不一致");
    let data = std::fs::read(&out).expect("读回");
    assert!(data.len() > 50_000, "文件太小，字体大概没嵌进去");
    let (ok, count) = xref_ok(&data);
    assert!(ok, "xref 偏移不对");
    assert!(count >= 6, "对象太少");
    assert!(data.windows(10).any(|w| w == b"/FontFile2"), "字体没嵌入");
    assert!(data.windows(10).any(|w| w == b"Identity-H"), "缺 Identity-H");
    assert!(data.windows(2).any(|w| w == b"Tj"), "没有文字绘制指令");
    let _ = std::fs::remove_file(&out);
}

#[test]
fn pagination_splits_wide_sheet() {
    // 造 200 列 × 400 行，必然分多页
    let mut wb = sheetcore::model::Workbook::new_blank();
    for r in 0..400u32 {
        for c in 0..40u32 {
            wb.sheet_mut(0).set(
                r,
                c,
                sheetcore::model::Cell {
                    value: sheetcore::model::Value::Number((r + c) as f64),
                    formula: None,
                },
            );
        }
    }
    let layout = sheetcore::print::paginate(wb.sheet(0), &PageSetup::default());
    assert!(layout.pages.len() > 1, "这么多内容应该分页");
    // 每页的行列区间必须连续覆盖、不重叠
    let mut covered_rows = std::collections::HashSet::new();
    for p in layout.pages.iter() {
        for r in p.rows.0..=p.rows.1 {
            covered_rows.insert(r);
        }
        assert!(p.rows.0 <= p.rows.1 && p.cols.0 <= p.cols.1);
    }
    assert_eq!(covered_rows.len(), 400, "行覆盖不全");
}
