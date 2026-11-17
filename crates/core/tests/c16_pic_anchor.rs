//! C16/QA-09：打印/导出里图片的矩形必须按 anchor 的 from/to + EMU 偏移算，
//! 不能把 to 格也整格算进去（那样会比 Excel/WPS 宽一整列、并且忽略偏移）。

use sheetcore::model::Workbook;
use sheetcore::pic::Picture;
use sheetcore::print;

fn wb_with(off: [i64; 4]) -> Workbook {
    let mut wb = Workbook::new_blank();
    let sh = wb.sheet_mut(0);
    sh.pictures.push(Picture {
        rect: (1, 1, 3, 4),
        off,
        part: "xl/media/image1.png".into(),
        data: Some(b"not-a-real-png".to_vec()),
        ext: "png".into(),
        name: "图片 1".into(),
    });
    sh.pictures_dirty = true;
    // 空表没有单元格，分页范围会是 A1；显式给打印区域，图片才在页内
    sh.page.print_area = Some((0, 0, 5, 5));
    wb
}

fn image_rect(wb: &Workbook) -> (f32, f32, f32, f32) {
    let sh = wb.sheet(0);
    let lay = print::paginate(sh, &sh.page);
    let ops = print::page_ops_with(wb, 0, &lay, 0, None);
    let img = ops.images.first().expect("应当有一张图片");
    let (x, y, w, h) = (img.0, img.1, img.2, img.3);
    assert!(w > 0.0 && h > 0.0);
    (x, y, w, h)
}

#[test]
fn picture_rect_uses_cell_boundaries_not_inclusive_to_cell() {
    let wb = wb_with([0; 4]);
    let left = wb.sheet(0).page.margins[0] * 72.0;
    let (x0, _, w0, _) = image_rect(&wb);
    // 左边界 = 左边距 + B 列起点；右边界 = E 列起点（to 是边界格，不含）
    // 1px = 9525 EMU；打印 1px = 0.75pt（scale=1）
    assert!((x0 - (left + 72.0 * 0.75)).abs() < 0.05, "x0={x0} left={left}");
    let expect_w = (72.0 * 4.0 - 72.0) * 0.75;
    assert!((w0 - expect_w).abs() < 0.01, "w0={w0} expect={expect_w}");
}

#[test]
fn picture_offsets_shift_and_resize_the_printed_rect() {
    let base = image_rect(&wb_with([0; 4]));
    let off = [9525 * 2, 9525 * 3, 9525 * 5, 9525 * 7];
    let moved = image_rect(&wb_with(off));
    let e = |v: i64| v as f32 / 9525.0 * 0.75;
    assert!((moved.0 - base.0 - e(off[0])).abs() < 0.02, "x {} -> {}", base.0, moved.0);
    assert!((moved.1 - base.1 + e(off[3])).abs() < 0.02, "y 反向：底边下移");
    let expect_w = base.2 + e(off[2]) - e(off[0]);
    let expect_h = base.3 + e(off[3]) - e(off[1]);
    assert!((moved.2 - expect_w).abs() < 0.02, "w {} expect {}", moved.2, expect_w);
    assert!((moved.3 - expect_h).abs() < 0.02, "h {} expect {}", moved.3, expect_h);
}
