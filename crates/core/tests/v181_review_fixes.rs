//! 审阅报告（CELLAN v1.81 / e2ce265 复测）核心引擎缺陷定向回归测试。

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use sheetcore::cf::{CfBlock, CfKind, CfRule, Dxf};
use sheetcore::charts::{Chart, ChartKind, Series};
use sheetcore::formula;
use sheetcore::image_embed;
use sheetcore::model::{Cell, CellRef, Value, Workbook};
use sheetcore::notes::{is_valid_ooxml_guid, new_ooxml_guid, ThreadEntry, ThreadedComment};
use sheetcore::pic::Picture;
use sheetcore::pivot::Agg;
use sheetcore::pivot_engine::{self, PivotTableModel};
use sheetcore::pivot_multi::{AxisField, PivotLayout, PivotSpec2, ShowAs, ValueField};
use sheetcore::print::{self, SheetPageRenderer};
use sheetcore::sort_filter::advanced_filter;
use sheetcore::undo::{MetaEdit, UndoStack};
use sheetcore::xlsx;

fn num(v: Value) -> f64 {
    match v {
        Value::Number(n) => n,
        other => panic!("expected number, got {other:?}"),
    }
}

fn temp_file(name: &str, ext: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "cellan_v181_core_{}_{}",
        name,
        std::process::id()
    ));
    let _ = std::fs::create_dir_all(&dir);
    dir.join(format!("{name}.{ext}"))
}

fn font_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets/fonts/subset/NotoSansSC-Regular.subset.ttf")
}

/// 生成 4×2 像素、左半纯红 (255,0,0) 右半纯蓝 (0,0,255) 的 24 位 BMP 图片字节。
fn make_two_color_bmp() -> Vec<u8> {
    let w: u32 = 4;
    let h: u32 = 2;
    let row_stride = (w * 3 + 3) & !3; // 12 bytes
    let pixel_size = row_stride * h;
    let file_size = 54 + pixel_size;
    let mut out = Vec::with_capacity(file_size as usize);
    // BITMAPFILEHEADER (14 bytes)
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&file_size.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&54u32.to_le_bytes());
    // BITMAPINFOHEADER (40 bytes)
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&(h as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // planes
    out.extend_from_slice(&24u16.to_le_bytes()); // bpp
    out.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    out.extend_from_slice(&pixel_size.to_le_bytes());
    out.extend_from_slice(&2835i32.to_le_bytes());
    out.extend_from_slice(&2835i32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    // Pixel data (bottom-up BGR): left 2 cols red (0,0,255), right 2 cols blue (255,0,0)
    for _y in 0..h {
        for x in 0..w {
            if x < 2 {
                out.extend_from_slice(&[0, 0, 255]); // red in BGR
            } else {
                out.extend_from_slice(&[255, 0, 0]); // blue in BGR
            }
        }
    }
    out
}

/// 审阅项 3：同一笔差额，比较能识别，相减也必须得到精确结果 1，而非被归零阈值清成 0。
#[test]
fn v181_item3_exact_integer_subtraction_and_large_diff_not_snapped_to_zero() {
    let mut wb = Workbook::new_blank();
    let set_f = |wb: &mut Workbook, r: u32, f: &str| {
        wb.sheets[0].set(r, 0, Cell { value: Value::Empty, formula: Some(f.into()) });
    };
    set_f(&mut wb, 0, "=1000000000000000-999999999999999");
    set_f(&mut wb, 1, "=1000000000000000=999999999999999");
    set_f(&mut wb, 2, "=4500000000000000-4499999999999999");
    set_f(&mut wb, 3, "=1000000000000000+(-999999999999999)");
    set_f(&mut wb, 4, "=2000000000000.01-2000000000000");
    set_f(&mut wb, 5, "=0.1+0.2-0.3");
    formula::recalc_all(&mut wb);

    assert_eq!(num(wb.sheets[0].value(0, 0)), 1.0, "1e15 - (1e15-1) 必须等于 1");
    assert_eq!(wb.sheets[0].value(1, 0), Value::Bool(false), "1e15 != 1e15-1");
    assert_eq!(num(wb.sheets[0].value(2, 0)), 1.0, "4.5e15 精确整数差必须等于 1");
    assert_eq!(num(wb.sheets[0].value(3, 0)), 1.0, "加法抵消大整数也必须等于 1");
    assert!(
        (num(wb.sheets[0].value(4, 0)) - (2_000_000_000_000.01_f64 - 2_000_000_000_000.0_f64)).abs() < 1e-9
            && num(wb.sheets[0].value(4, 0)) > 0.005,
        "2万亿+0.01 减 2万亿应保留非零 IEEE 差值 (~0.01)，实际 {:?}",
        wb.sheets[0].value(4, 0)
    );
    assert_eq!(num(wb.sheets[0].value(5, 0)), 0.0, "0.1+0.2-0.3 浮点噪声仍应归零");
}

/// 审阅项 7：多级透视表的「占父行汇总%」和「占父列汇总%」必须使用直接父级小计作为分母。
#[test]
fn v181_item7_multilevel_pivot_percent_of_parent_row_and_col_use_immediate_parent() {
    let mut wb = Workbook::new_blank();
    {
        let sh = wb.sheet_mut(0);
        // 国家 → 区域 → 城市，金额：东区(10, 30)、西区(60)
        sh.set(0, 0, Cell { value: Value::Text("国家".into()), formula: None });
        sh.set(0, 1, Cell { value: Value::Text("区域".into()), formula: None });
        sh.set(0, 2, Cell { value: Value::Text("城市".into()), formula: None });
        sh.set(0, 3, Cell { value: Value::Text("金额".into()), formula: None });

        let rows = [
            ("中国", "东区", "上海", 10.0),
            ("中国", "东区", "杭州", 30.0),
            ("中国", "西区", "成都", 60.0),
        ];
        for (i, (country, region, city, amt)) in rows.iter().enumerate() {
            let r = (i + 1) as u32;
            sh.set(r, 0, Cell { value: Value::Text((*country).into()), formula: None });
            sh.set(r, 1, Cell { value: Value::Text((*region).into()), formula: None });
            sh.set(r, 2, Cell { value: Value::Text((*city).into()), formula: None });
            sh.set(r, 3, Cell { value: Value::Number(*amt), formula: None });
        }
    }

    // 1. 行字段三层：国家 -> 区域 -> 城市，ShowAs::PercentOfParentRow
    let row_spec = PivotSpec2 {
        source: (0, 0, 3, 3),
        rows: vec![AxisField::plain(0), AxisField::plain(1), AxisField::plain(2)],
        cols: vec![],
        filters: vec![],
        values: vec![ValueField {
            col: 3,
            agg: Agg::Sum,
            show_as: ShowAs::PercentOfParentRow,
            label: Some("占父行%".into()),
            base: None,
        }],
        layout: PivotLayout::default(),
    };
    let row_model = PivotTableModel::new(
        "透视行".into(),
        "Sheet1".into(),
        Some(wb.sheets[0].identity),
        0,
        (0, 5),
        row_spec,
    );
    let computed_row = pivot_engine::compute_table(&wb.sheets[0], &row_model);
    let val_col = computed_row.grid.row_depth;
    let mut city_pcts: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
    for r in &computed_row.grid.rows {
        for cell in &r[..val_col] {
            if let Value::Text(label) = cell {
                let trimmed = label.trim();
                if matches!(trimmed, "上海" | "杭州" | "成都") {
                    city_pcts.insert(trimmed.to_string(), num(r[val_col].clone()));
                }
            }
        }
    }
    assert!((city_pcts["上海"] - 0.25).abs() < 1e-9, "上海应为 25%，实际 {:?}", city_pcts);
    assert!((city_pcts["杭州"] - 0.75).abs() < 1e-9, "杭州应为 75%，实际 {:?}", city_pcts);
    assert!((city_pcts["成都"] - 1.00).abs() < 1e-9, "成都应为 100%，实际 {:?}", city_pcts);

    // 2. 列字段三层：国家 -> 区域 -> 城市，ShowAs::PercentOfParentCol
    let col_spec = PivotSpec2 {
        source: (0, 0, 3, 3),
        rows: vec![],
        cols: vec![AxisField::plain(0), AxisField::plain(1), AxisField::plain(2)],
        filters: vec![],
        values: vec![ValueField {
            col: 3,
            agg: Agg::Sum,
            show_as: ShowAs::PercentOfParentCol,
            label: Some("占父列%".into()),
            base: None,
        }],
        layout: PivotLayout::default(),
    };
    let col_model = PivotTableModel::new(
        "透视列".into(),
        "Sheet1".into(),
        Some(wb.sheets[0].identity),
        0,
        (10, 5),
        col_spec,
    );
    let computed_col = pivot_engine::compute_table(&wb.sheets[0], &col_model);
    // 列头与数据行匹配（忽略总计列）
    let data_row = &computed_col.grid.rows[0];
    let mut col_pcts: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
    for hdr_row in &computed_col.grid.headers {
        for (ci, label) in hdr_row.iter().enumerate() {
            let trimmed = label.trim();
            if matches!(trimmed, "上海" | "杭州" | "成都") {
                col_pcts.insert(trimmed.to_string(), num(data_row[ci].clone()));
            }
        }
    }
    assert!((col_pcts["上海"] - 0.25).abs() < 1e-9, "列布局上海应为 25%，实际 {:?}", col_pcts);
    assert!((col_pcts["杭州"] - 0.75).abs() < 1e-9, "列布局杭州应为 75%，实际 {:?}", col_pcts);
    assert!((col_pcts["成都"] - 1.00).abs() < 1e-9, "列布局成都应为 100%，实际 {:?}", col_pcts);
}

fn make_sample_chart(title: &str) -> Chart {
    let mut ch = Chart::default();
    ch.kind = ChartKind::Column;
    ch.title = title.into();
    ch.anchor = (0, 3, 12, 9);
    ch.series.push(Series {
        name: "值".into(),
        cats: vec!["1月".into(), "2月".into()],
        vals: vec![10.0, 20.0],
        val_ref: "Sheet1!$B$2:$B$3".into(),
        cat_ref: "Sheet1!$A$2:$A$3".into(),
        ..Default::default()
    });
    ch.content_dirty = true;
    ch
}

fn seed_chart_wb() -> Workbook {
    let mut wb = Workbook::new_blank();
    let sh = wb.sheet_mut(0);
    sh.set(0, 0, Cell { value: Value::Text("月".into()), formula: None });
    sh.set(0, 1, Cell { value: Value::Text("值".into()), formula: None });
    sh.set(1, 0, Cell { value: Value::Text("1月".into()), formula: None });
    sh.set(1, 1, Cell { value: Value::Number(10.0), formula: None });
    sh.set(2, 0, Cell { value: Value::Text("2月".into()), formula: None });
    sh.set(2, 1, Cell { value: Value::Number(20.0), formula: None });
    wb
}

/// 审阅项 8a：新建图表在首次后台保存期间被改标题，再保存并重开后仍只有 1 张新标题图表（不能变 2 张）。
#[test]
fn v181_item8_new_chart_edited_during_first_bg_save_does_not_duplicate() {
    let path = temp_file("item8_edit", "xlsx");
    let mut live = seed_chart_wb();
    live.sheets[0].charts.push(make_sample_chart("旧标题"));
    live.sheets[0].charts_dirty = true;
    live.dirty = true;

    // 首次后台保存快照（此时 chart.part == ""）
    let snap = live.clone();

    // 后台保存进行中，用户修改了这张新建图表的标题
    live.sheets[0].charts[0].title = "新标题".into();
    live.sheets[0].charts[0].content_dirty = true;
    live.sheets[0].charts_dirty = true;

    let (receipt, _) = xlsx::save_snapshot_with_receipt(&path, snap).expect("first bg save");
    receipt.adopt_document(&mut live, &path, false);

    assert!(
        !live.sheets[0].charts[0].part.is_empty(),
        "首次保存回执必须把已落盘的 part 绑定到 live 新图表上"
    );
    assert!(
        live.sheets[0].charts[0].content_dirty && live.sheets[0].charts_dirty,
        "标题已变，脏标必须保留以便二次保存落盘"
    );

    // 二次保存并重开
    xlsx::save_atomic(&path, &live).expect("second save");
    let reopened = xlsx::load(&path).expect("reopen");
    assert_eq!(reopened.sheets[0].charts.len(), 1, "重开后绝不能出现旧标题+新标题两张图");
    assert_eq!(reopened.sheets[0].charts[0].title, "新标题");
    let _ = std::fs::remove_file(&path);
}

/// 审阅项 8b：新建图表在首次后台保存期间被删除，再保存并重开后图表数为 0（不能复活）。
#[test]
fn v181_item8_new_chart_deleted_during_first_bg_save_does_not_resurrect() {
    let path = temp_file("item8_delete", "xlsx");
    let mut live = seed_chart_wb();
    live.sheets[0].charts.push(make_sample_chart("待删图表"));
    live.sheets[0].charts_dirty = true;
    live.dirty = true;

    // 首次后台保存快照（此时 chart.part == ""）
    let snap = live.clone();

    // 后台保存进行中，用户删除了这张尚未回写 part 的新图表
    live.sheets[0].charts.clear();
    live.sheets[0].charts_dirty = true;

    let (receipt, _) = xlsx::save_snapshot_with_receipt(&path, snap).expect("first bg save");
    receipt.adopt_document(&mut live, &path, false);

    assert!(
        !live.sheets[0].deleted_charts.is_empty(),
        "后台保存写入但已在 live 删除的新图表必须登记到 deleted_charts"
    );
    assert!(live.sheets[0].charts_dirty && live.dirty, "文档必须保持脏态以便下次保存清掉磁盘上的孤儿图表");

    // 二次保存并重开
    xlsx::save_atomic(&path, &live).expect("second save");
    let reopened = xlsx::load(&path).expect("reopen");
    assert_eq!(reopened.sheets[0].charts.len(), 0, "重开后被删除的图表绝不能复活");
    let _ = std::fs::remove_file(&path);
}

/// 审阅项 9：富文本撤销/重做必须执行 guard 冲突检查，不得覆盖同事后写的新值，且不进入待广播清单。
#[test]
fn v181_item9_rich_text_undo_respects_collab_guard_and_skips_overwritten_cell() {
    let mut wb = Workbook::new_blank();
    let old_c = Cell { value: Value::Text("甲改前".into()), formula: None };
    let new_c = Cell { value: Value::Text("甲富文本".into()), formula: None };
    let peer_c = Cell { value: Value::Text("乙后写的新值".into()), formula: None };

    // 甲写入富文本并入撤销栈
    wb.sheets[0].set(0, 0, new_c.clone());
    let mut undo = UndoStack::new();
    undo.push_meta(
        "富文本格式",
        (0, 0, 0),
        MetaEdit {
            sheet: 0,
            rich: vec![((0, 0), old_c.clone(), new_c.clone(), None, None)],
            ..Default::default()
        },
    );

    // 乙随后覆盖 A1 为新值
    wb.sheets[0].set(0, 0, peer_c.clone());

    // 甲执行撤销，并提供拒绝覆盖的 guard
    let guard_calls = AtomicUsize::new(0);
    let applied = undo
        .undo(&mut wb.sheets, |si, r, c, cur| {
            guard_calls.fetch_add(1, Ordering::SeqCst);
            assert_eq!((si, r, c), (0, 0, 0));
            assert_eq!(cur.value, peer_c.value);
            false
        })
        .expect("undo should return Applied");

    assert_eq!(guard_calls.load(Ordering::SeqCst), 1, "富文本撤销必须调用 guard");
    assert_eq!(applied.skipped, 1, "被同事覆盖的富文本格应计入 skipped");
    assert!(applied.touched.is_empty(), "被跳过的富文本格绝不能进入待广播 touched 清单");
    assert_eq!(wb.sheets[0].value(0, 0), peer_c.value, "同事后写的值绝不能被甲的撤销覆盖");

    // 同样验证：即使 guard 返回 true，但当前值与预期 new_c 不同，也不得静默覆盖
    let mut undo2 = UndoStack::new();
    undo2.push_meta(
        "富文本格式",
        (0, 0, 0),
        MetaEdit {
            sheet: 0,
            rich: vec![((0, 0), old_c, new_c, None, None)],
            ..Default::default()
        },
    );
    let applied2 = undo2.undo(&mut wb.sheets, |_, _, _, _| true).expect("undo2");
    assert_eq!(applied2.skipped, 1);
    assert!(applied2.touched.is_empty());
    assert_eq!(wb.sheets[0].value(0, 0), peer_c.value);
}

/// 审阅项 10：PDF 导出必须真实应用图片裁剪（物理剔除被裁像素防敏感信息泄露）与旋转矩阵。
#[test]
fn v181_item10_pdf_export_applies_picture_crop_and_rotation() {
    let bmp = make_two_color_bmp();

    // 1. 底层 image_embed：裁掉左半红色，输出尺寸减半且像素全是蓝色 (0,0,255)，不含任何红色像素
    let (full_emb, full_baked) = image_embed::make_with_crop("full", "bmp", &bmp, [0.0; 4]).expect("full embed");
    let (half_emb, half_baked) = image_embed::make_with_crop("half", "bmp", &bmp, [0.5, 0.0, 0.0, 0.0]).expect("cropped embed");
    assert!(!full_baked);
    assert_eq!((full_emb.width, full_emb.height), (4, 2));
    assert_eq!((half_emb.width, half_emb.height), (2, 2), "裁掉左半后宽度应减半为 2");
    assert!(half_baked, "BMP 裁剪应物理重编码剔除被裁区域");
    let mut raw_rgb = Vec::new();
    flate2::read::ZlibDecoder::new(half_emb.data.as_slice())
        .read_to_end(&mut raw_rgb)
        .expect("inflate cropped rgb");
    assert_eq!(raw_rgb.len(), 2 * 2 * 3);
    for px in raw_rgb.chunks_exact(3) {
        assert_eq!(px, &[0, 0, 255], "裁掉左半红色后仅应保留右半纯蓝像素，绝不能残留红色");
    }

    // 2. 端到端 PDF 导出：对比原图 / 裁掉左半 / 旋转 90° 三份 PDF 的内容流与图像对象
    let export_variant = |tag: &str, crop: [f32; 4], rot_deg: f32| -> Vec<u8> {
        let mut wb = Workbook::new_blank();
        wb.sheets[0].set(0, 0, Cell { value: Value::Text("图片测试".into()), formula: None });
        wb.sheets[0].pictures.push(Picture {
            rect: (1, 0, 5, 3),
            off: [0; 4],
            part: format!("xl/media/{tag}.bmp"),
            data: Some(std::sync::Arc::new(bmp.clone())),
            ext: "bmp".into(),
            name: tag.into(),
            rot_deg,
            crop,
        });
        let pdf_path = temp_file(&format!("item10_{tag}"), "pdf");
        let _ = std::fs::remove_file(&pdf_path);
        print::export_pdf_from(&wb, 0, &pdf_path, &font_path(), None).expect("export pdf");
        let bytes = std::fs::read(&pdf_path).expect("read pdf");
        let _ = std::fs::remove_file(&pdf_path);
        bytes
    };

    let pdf_orig = export_variant("orig", [0.0; 4], 0.0);
    let pdf_half = export_variant("half", [0.5, 0.0, 0.0, 0.0], 0.0);
    let pdf_rot = export_variant("rot90", [0.0; 4], 90.0);

    let s_orig = String::from_utf8_lossy(&pdf_orig);
    let s_half = String::from_utf8_lossy(&pdf_half);
    let s_rot = String::from_utf8_lossy(&pdf_rot);

    assert!(s_orig.contains("/Width 4 /Height 2"), "原图 PDF Image XObject 应为 4x2");
    assert!(s_half.contains("/Width 2 /Height 2"), "裁掉左半后 PDF Image XObject 应为 2x2（被裁像素已物理剔除）");
    assert_ne!(pdf_orig, pdf_half, "裁剪后的 PDF 不得与原图相同");
    assert_ne!(pdf_orig, pdf_rot, "旋转 90° 后的 PDF 不得与原图相同");
    // 旋转 90° 时 PDF 变换矩阵为 `q 0.0000 -w h 0.0000 e f cm /Imrot90 Do Q`（对角线 a/d 归零，非对角线 b<0, c>0）
    assert!(
        s_rot.contains("q 0.0000 -") && s_rot.contains(" 0.0000 ") && s_rot.contains(" cm /Imrot90 Do Q"),
        "旋转 90° 后的 PDF 内容流必须包含 90° 旋转 cm 矩阵"
    );
    assert!(
        !s_orig.contains("q 0.0000 -"),
        "原图 PDF 不应带有旋转矩阵"
    );
}

/// 审阅项 11：PDF/打印图表渲染必须正确表达 100% 堆积柱形图堆叠关系与多系列散点图统一 X 轴。
#[test]
fn v181_item11_pdf_chart_stacked100_and_multiseries_scatter_x_axis() {
    // 1. 100% 堆积柱形图：两个系列 (25, 75) 应在同一横坐标垂直堆叠，总高度占满绘图区且不并排
    let mut wb = Workbook::new_blank();
    let mut stacked = Chart::default();
    stacked.kind = ChartKind::ColumnStacked100;
    stacked.title = "百分百堆积".into();
    stacked.anchor = (0, 0, 14, 7);
    stacked.series.push(Series {
        name: "S1".into(),
        cats: vec!["一月".into()],
        vals: vec![25.0],
        ..Default::default()
    });
    stacked.series.push(Series {
        name: "S2".into(),
        cats: vec!["一月".into()],
        vals: vec![75.0],
        ..Default::default()
    });
    wb.sheets[0].charts.push(stacked);

    let layout = print::paginate(&wb.sheets[0], &wb.sheets[0].page);
    let out = print::page_ops(&wb, 0, &layout, 0);
    // 系列 0 颜色 [68, 114, 196]，系列 1 颜色 [237, 125, 49]（取高度 > 10pt 的柱体，排除图例色块）
    let s1_bar = out
        .rects
        .iter()
        .find(|(_, _, _, h, col)| *col == [68, 114, 196] && *h > 10.0)
        .expect("应找到 S1 柱体");
    let s2_bar = out
        .rects
        .iter()
        .find(|(_, _, _, h, col)| *col == [237, 125, 49] && *h > 10.0)
        .expect("应找到 S2 柱体");

    assert!(
        (s1_bar.0 - s2_bar.0).abs() < 0.05,
        "100% 堆积柱形图各系列必须在同一 X 坐标堆叠，不应并排：S1={s1_bar:?} S2={s2_bar:?}"
    );
    assert!(
        ((s1_bar.1 + s1_bar.3) - s2_bar.1).abs() < 0.1,
        "S2 底部必须紧接 S1 顶部：S1={s1_bar:?} S2={s2_bar:?}"
    );
    let ratio = s2_bar.3 / s1_bar.3;
    assert!(
        (ratio - 3.0).abs() < 0.05,
        "25% 与 75% 堆叠高度比应为 1:3，实际 {ratio}"
    );
    assert!(
        out.texts.iter().any(|t| t.text == "100%"),
        "100% 堆积柱形图纵轴刻度应显示百分比"
    );

    // 2. 多系列散点图：系列 1 x=[1, 2]，系列 2 x=[10, 20]，横坐标必须共享 [1, 20] 区间
    let mut wb_sc = Workbook::new_blank();
    let mut scatter = Chart::default();
    scatter.kind = ChartKind::Scatter;
    scatter.title = "多系列散点".into();
    scatter.anchor = (0, 0, 14, 7);
    scatter.series.push(Series {
        name: "低区间".into(),
        vals: vec![10.0, 20.0],
        xv: vec![1.0, 2.0],
        ..Default::default()
    });
    scatter.series.push(Series {
        name: "高区间".into(),
        vals: vec![10.0, 20.0],
        xv: vec![10.0, 20.0],
        ..Default::default()
    });
    wb_sc.sheets[0].charts.push(scatter);

    let layout_sc = print::paginate(&wb_sc.sheets[0], &wb_sc.sheets[0].page);
    let out_sc = print::page_ops(&wb_sc, 0, &layout_sc, 0);
    // 散点标记为 4×4 正方形（w==4, h==4），图例为 7×7，按颜色区分两个系列
    let mut pts_s1: Vec<f32> = out_sc
        .rects
        .iter()
        .filter(|(_, _, w, h, col)| *col == [68, 114, 196] && (*w - 4.0).abs() < 0.1 && (*h - 4.0).abs() < 0.1)
        .map(|(x, _, _, _, _)| *x)
        .collect();
    let mut pts_s2: Vec<f32> = out_sc
        .rects
        .iter()
        .filter(|(_, _, w, h, col)| *col == [237, 125, 49] && (*w - 4.0).abs() < 0.1 && (*h - 4.0).abs() < 0.1)
        .map(|(x, _, _, _, _)| *x)
        .collect();
    pts_s1.sort_by(|a, b| a.total_cmp(b));
    pts_s2.sort_by(|a, b| a.total_cmp(b));
    assert_eq!(pts_s1.len(), 2, "S1 应有 2 个散点: {pts_s1:?}");
    assert_eq!(pts_s2.len(), 2, "S2 应有 2 个散点: {pts_s2:?}");
    assert!(
        pts_s1[1] + 5.0 < pts_s2[0],
        "散点 x=[1,2] 的最大横坐标必须明显小于 x=[10,20] 的最小横坐标：S1={pts_s1:?} S2={pts_s2:?}"
    );
    // 比例校验：(x(10)-x(1)) / (x(20)-x(1)) 应接近 9/19 ≈ 0.4737
    let rel_10 = (pts_s2[0] - pts_s1[0]) / (pts_s2[1] - pts_s1[0]);
    assert!(
        (rel_10 - 9.0 / 19.0).abs() < 0.05,
        "统一 X 轴上 10 相对 [1,20] 的位置应约 0.474，实际 {rel_10}"
    );
}

/// 审阅项 12：高级筛选复制到 A1048576 且含表头 + 2 条记录时，必须在写入任何单元格前报错拒绝。
#[test]
fn v181_item12_advanced_filter_copy_to_rejects_row_overflow_before_writing() {
    let mut wb = Workbook::new_blank();
    let sh = wb.sheet_mut(0);
    sh.set(0, 0, Cell { value: Value::Text("名称".into()), formula: None });
    sh.set(1, 0, Cell { value: Value::Text("甲".into()), formula: None });
    sh.set(2, 0, Cell { value: Value::Text("乙".into()), formula: None });
    // 在最后一行 A1048576 放一个哨兵值，溢出失败时不能被改写
    sh.set(1_048_575, 0, Cell { value: Value::Text("哨兵".into()), formula: None });

    let err = advanced_filter(sh, (0, 0, 2, 0), None, Some((1_048_575, 0)), false)
        .expect_err("复制到 A1048576 含 3 行结果必须报错拒绝");
    assert!(
        err.contains("上限") && err.contains("1048576"),
        "错误信息应明确指出超出工作表行数上限：{err}"
    );
    assert_eq!(
        sh.value(1_048_575, 0),
        Value::Text("哨兵".into()),
        "校验失败前绝不能改写 A1048576"
    );
    assert!(sh.get(1_048_576, 0).is_none(), "绝不能写出越界行 1048577");
    assert!(sh.get(1_048_577, 0).is_none(), "绝不能写出越界行 1048578");

    // 刚好贴边（A1048574 起写 3 行 -> A1048574:A1048576）应正常成功
    let ok = advanced_filter(sh, (0, 0, 2, 0), None, Some((1_048_573, 0)), false)
        .expect("贴边 3 行应允许写入");
    assert_eq!(ok.matched, 2);
    assert_eq!(sh.value(1_048_573, 0), Value::Text("名称".into()));
    assert_eq!(sh.value(1_048_574, 0), Value::Text("甲".into()));
    assert_eq!(sh.value(1_048_575, 0), Value::Text("乙".into()));
}

/// 审阅项 16：线程批注写入 XLSX 的 person/@id 与 threadedComment/@id、parentId 均须符合 MS-XLSX ST_Guid 规范。
#[test]
fn v181_item16_threaded_comments_write_valid_ooxml_guids() {
    let path = temp_file("item16_threaded_guid", "xlsx");
    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(0, 0, Cell { value: Value::Text("审阅单元格".into()), formula: None });
    wb.sheets[0].threads.insert(
        CellRef::new(0, 0),
        ThreadedComment {
            r: 0,
            c: 0,
            resolved: false,
            entries: vec![
                ThreadEntry {
                    id: new_ooxml_guid(),
                    author: "张三".into(),
                    timestamp: "2026-11-26T10:00:00Z".into(),
                    text: "请核对口径".into(),
                },
                ThreadEntry {
                    // 即使内存残留旧版非 GUID 标识，写盘时也应规范化为合法 GUID
                    id: "{CELLAN-LEGACY-1}".into(),
                    author: "李四".into(),
                    timestamp: "2026-11-26T10:05:00Z".into(),
                    text: "已核对完成".into(),
                },
            ],
        },
    );
    wb.sheets[0].threads_dirty = true;
    wb.dirty = true;
    xlsx::save_atomic(&path, &wb).expect("save with threaded comments");

    // 直接解包检验 xl/persons/person.xml 与 xl/threadedComments/threadedComment1.xml
    let f = std::fs::File::open(&path).expect("open xlsx");
    let mut z = zip::ZipArchive::new(f).expect("zip");
    let mut person_xml = String::new();
    z.by_name("xl/persons/person.xml")
        .expect("person.xml must exist")
        .read_to_string(&mut person_xml)
        .unwrap();
    let mut tc_xml = String::new();
    z.by_name("xl/threadedComments/threadedComment1.xml")
        .expect("threadedComment1.xml must exist")
        .read_to_string(&mut tc_xml)
        .unwrap();

    assert!(!person_xml.contains("{CELLAN-"), "person.xml 不得出现自定义非 GUID 标识：{person_xml}");
    assert!(!tc_xml.contains("{CELLAN-"), "threadedComment1.xml 不得出现自定义非 GUID 标识：{tc_xml}");

    let pdoc = roxmltree::Document::parse(&person_xml).unwrap();
    let persons: Vec<_> = pdoc
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "person")
        .collect();
    assert_eq!(persons.len(), 2);
    for p in &persons {
        let id = p.attribute("id").expect("person/@id");
        assert!(is_valid_ooxml_guid(id), "非法 person/@id: {id}");
    }

    let tdoc = roxmltree::Document::parse(&tc_xml).unwrap();
    let tcs: Vec<_> = tdoc
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "threadedComment")
        .collect();
    assert_eq!(tcs.len(), 2);
    let root_id = tcs[0].attribute("id").expect("root id");
    let reply_id = tcs[1].attribute("id").expect("reply id");
    let parent_id = tcs[1].attribute("parentId").expect("reply parentId");
    assert!(is_valid_ooxml_guid(root_id), "非法 root id: {root_id}");
    assert!(is_valid_ooxml_guid(reply_id), "非法 reply id: {reply_id}");
    assert_eq!(parent_id, root_id, "回复的 parentId 必须指向主贴的合法 GUID");
    for tc in &tcs {
        let pid = tc.attribute("personId").expect("personId");
        assert!(is_valid_ooxml_guid(pid), "非法 personId: {pid}");
    }

    // 重开后线程批注结构完整读回
    let reloaded = xlsx::load(&path).expect("reload");
    let tc = reloaded.sheets[0]
        .threads
        .get(&CellRef::new(0, 0))
        .expect("reloaded threaded comment");
    assert_eq!(tc.entries.len(), 2);
    assert_eq!(tc.entries[0].author, "张三");
    assert_eq!(tc.entries[1].text, "已核对完成");
    let _ = std::fs::remove_file(&path);
}

/// 审阅项 17：按工作表复用的 SheetPageRenderer 多页输出必须与逐页 page_ops_with 完全一致（含重复值条件格式）。
#[test]
fn v181_item17_sheet_page_renderer_reuses_cf_state_with_identical_multipage_output() {
    let mut wb = Workbook::new_blank();
    {
        let sh = wb.sheet_mut(0);
        for r in 0..160u32 {
            // 故意构造重复值（偶数行同为 100）
            let v = if r % 2 == 0 { 100.0 } else { f64::from(r) };
            sh.set(r, 0, Cell { value: Value::Number(v), formula: None });
        }
        sh.cf.dxfs.push(Dxf {
            fill: Some([255, 210, 210]),
            ..Default::default()
        });
        sh.cf.blocks.push(CfBlock {
            ranges: vec![(0, 0, 159, 0)],
            rules: vec![CfRule {
                priority: 1,
                stop: false,
                kind: CfKind::Dup { unique: false, dxf: 0 },
            }],
        });
    }
    let layout = print::paginate(&wb.sheets[0], &wb.sheets[0].page);
    assert!(layout.pages.len() >= 3, "应至少分成 3 页以覆盖多页复用");

    let mut reused = SheetPageRenderer::new(&wb, 0);
    for pi in 0..layout.pages.len() {
        let a = reused.page_ops(&layout, pi, None);
        let b = print::page_ops_with(&wb, 0, &layout, pi, None);
        assert_eq!(a.rects, b.rects, "第 {pi} 页填充矩形应一致（含重复值条件格式）");
        assert_eq!(a.lines, b.lines, "第 {pi} 页线条应一致");
        assert_eq!(a.texts.len(), b.texts.len(), "第 {pi} 页文本数量应一致");
        assert!(
            a.rects.iter().any(|(_, _, _, _, col)| *col == [255, 210, 210]),
            "第 {pi} 页应命中重复值条件格式底色"
        );
    }
}
