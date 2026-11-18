//! UI-14.3：分页必须把正文每一行恰好放进一页。
//!
//! PDF 导出和打印预览都消费同一个 `Layout.pages`；这里用 120 行 +
//! 顶端标题行做属性检查，防止导出 PDF 时漏行/重复行。

use sheetcore::model::{Cell, Value, Workbook};
use sheetcore::print;

#[test]
fn pagination_covers_every_body_row_once() {
    let mut wb = Workbook::new_blank();
    {
        let sh = wb.sheet_mut(0);
        sh.set(0, 0, Cell { value: Value::Text("标题".into()), formula: None });
        for r in 1..=120u32 {
            sh.set(r, 0, Cell { value: Value::Text(format!("行{r}").into()), formula: None });
        }
        sh.page.print_area = Some((0, 0, 120, 0));
        sh.page.title_rows = Some((0, 0));
    }

    let sh = wb.sheet(0);
    let lay = print::paginate(sh, &sh.page);
    assert!(lay.pages.len() > 1, "120 行应分成多页，实际 {}", lay.pages.len());

    let mut seen = vec![0usize; 121];
    for (page_idx, span) in lay.pages.iter().enumerate() {
        assert_eq!(span.cols, (0, 0), "page {page_idx} 列范围");
        assert!(span.rows.0 >= 1, "正文不应重画标题行：{span:?}");
        assert!(span.rows.0 <= span.rows.1, "空页范围：{span:?}");
        for r in span.rows.0..=span.rows.1 {
            seen[r as usize] += 1;
        }

        // PDF/打印预览的实际文本操作也按同一 span 生成：每行文本在该页恰好一次。
        let ops = print::page_ops_with(&wb, 0, &lay, page_idx, None);
        for r in span.rows.0..=span.rows.1 {
            let label = format!("行{r}");
            let n = ops.texts.iter().filter(|t| t.text == label).count();
            assert_eq!(n, 1, "page {page_idx} 行 {r} 文本次数 {n}");
        }
    }

    for r in 1..=120usize {
        assert_eq!(seen[r], 1, "正文第 {r} 行应恰好出现一次，实际 {}", seen[r]);
    }
}
