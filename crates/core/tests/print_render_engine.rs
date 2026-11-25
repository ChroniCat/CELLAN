//! 渲染与打印通道全链路深度测试（覆盖分页、合并格、页眉页脚、13 种边框、富文本、
//! 条件格式图标、迷你图、图表折线、跨页对象、黑白/草稿/批注/错误值、矢量水印、
//! PNG 索引色/BMP/GIF 解码、`.ttc` 多字体回退链、整簿 `/Outlines` 书签目录与 OOXML Schema 序）。

use std::path::PathBuf;
use std::sync::Arc;

use sheetcore::cf::{CfBlock, CfKind, CfOp, CfRule, Cfvo, Dxf, DxfBorder};
use sheetcore::charts::{Chart, ChartKind, LabelOpts, Series};
use sheetcore::formula::ErrKind;
use sheetcore::image_embed;
use sheetcore::model::{Cell, CellRef, Sheet, Value, Workbook};
use sheetcore::notes::Note;
use sheetcore::page::{paper_mm, paper_pt, PageSetup, WatermarkConfig};
use sheetcore::pdf::Font;
use sheetcore::pic::Picture;
use sheetcore::print::{
    comment_summary_pages, export_workbook_pdf_with_pack, header_parts, header_parts_with_ctx,
    page_ops_with, paginate, parse_page_ranges, HeaderContext, PdfFontPack,
};
use sheetcore::richtext::RichRun;
use sheetcore::sparkline::{EmptyAs, SparkKind, Sparkline};
use sheetcore::style::{Border, CellStyle};

fn embedded_fonts() -> (Font, Font) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let reg = Font::load(&root.join("assets/fonts/subset/NotoSansSC-Regular.subset.ttf"))
        .expect("load subset regular");
    let bold = Font::load(&root.join("assets/fonts/subset/NotoSansSC-Bold.subset.ttf"))
        .expect("load subset bold");
    (reg, bold)
}

fn push_style(wb: &mut Workbook, sheet_idx: usize, r: u32, c: u32, st: CellStyle) {
    let sid = wb.styles.intern(st);
    wb.sheets[sheet_idx].set_style(r, c, sid);
}

#[test]
fn multi_row_merged_cell_expands_downward_not_upward() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].rows.set(0, 24.0);
    wb.sheets[0].rows.set(1, 24.0);
    wb.sheets[0].rows.set(2, 24.0);
    wb.sheets[0].cols.set(0, 100.0);
    wb.sheets[0].cols.set(1, 100.0);
    wb.sheets[0].merges.push((0, 0, 2, 1)); // A1:B3
    push_style(
        &mut wb,
        0,
        0,
        0,
        CellStyle {
            fill: Some([220, 235, 250]),
            ..CellStyle::default()
        },
    );
    wb.sheets[0].set(0, 0, Cell { value: Value::Text("跨三行标题".into()), formula: None });
    wb.sheets[0].set(3, 0, Cell { value: Value::Text("第四行正文".into()), formula: None });

    let layout = paginate(&wb.sheets[0], &wb.sheets[0].page);
    let out = page_ops_with(&wb, 0, &layout, 0, None);
    let (_, ph) = layout.page_size;
    let top_margin_y = ph - wb.sheets[0].page.margins[2] * 72.0;

    // 合并格底色矩形的顶边 y + h 必须对齐表顶（绝不向上溢出页边距），总高等于 3 行 * 24px * 0.75 = 54pt
    let bg = out
        .rects
        .iter()
        .find(|r| r.4 == [220, 235, 250])
        .expect("merged cell fill rect");
    assert!(
        (bg.1 + bg.3 - top_margin_y).abs() < 1.0,
        "merged cell top should align with row 1 top ({top_margin_y}), got {}",
        bg.1 + bg.3
    );
    assert!(
        (bg.3 - 54.0).abs() < 1.0,
        "merged cell height should be ~54pt (3 rows), got {}",
        bg.3
    );
    assert!(
        bg.1 < top_margin_y - 50.0,
        "merged cell bottom must extend downward, got y={}",
        bg.1
    );
    let txt = out
        .texts
        .iter()
        .find(|t| t.text.contains("跨三行"))
        .expect("merged cell text");
    assert!(
        txt.baseline >= bg.1 && txt.baseline <= bg.1 + bg.3,
        "merged text baseline {} must stay inside box [{}, {}]",
        txt.baseline,
        bg.1,
        bg.1 + bg.3
    );
}

#[test]
fn header_footer_plain_centered_tokens_and_first_odd_even() {
    // 1. 无 &L/&C/&R 前缀时默认居中（不再错塞进左区）
    let (l, c, r) = header_parts("第 &P 页，共 &N 页", 2, 10);
    assert_eq!(l, "");
    assert_eq!(c, "第 2 页，共 10 页");
    assert_eq!(r, "");

    // 2. 全部动态令牌 + 忽略 Excel 内联字体/字号/颜色声明 + 页码偏移 &P+1 + 字面 &&
    let ctx = HeaderContext {
        file_name: "2026年财报.xlsx".into(),
        sheet_name: "资产负债表".into(),
        folder_path: r"F:\Reports".into(),
        date_str: "2026-11-25".into(),
        time_str: "19:30".into(),
    };
    let (l, c, r) = header_parts_with_ctx(
        r#"&L&"微软雅黑,常规"&12&K1A2B3C&F && &A&C第 &P+1/&N 页&R&D &T &Z"#,
        3,
        8,
        Some(&ctx),
    );
    assert_eq!(l, "2026年财报.xlsx & 资产负债表");
    assert_eq!(c, "第 4/8 页");
    assert_eq!(r, "2026-11-25 19:30 F:\\Reports");

    // 3. 首页不同 + 奇偶页不同
    let mut setup = PageSetup::default();
    setup.header = "&C常规奇数页 &P".into();
    setup.different_first_page = true;
    setup.first_header = "&C封面首页".into();
    setup.different_odd_even = true;
    setup.even_header = "&C偶数页 &P".into();
    assert_eq!(setup.header_footer_for_page(1).0, "&C封面首页");
    assert_eq!(setup.header_footer_for_page(2).0, "&C偶数页 &P");
    assert_eq!(setup.header_footer_for_page(3).0, "&C常规奇数页 &P");
}

#[test]
fn canonical_ooxml_paper_sizes_and_strict_schema_order_roundtrip() {
    assert_eq!(paper_mm(9), (210.0, 297.0)); // A4
    assert_eq!(paper_mm(8), (297.0, 420.0)); // A3
    assert_eq!(paper_mm(11), (148.0, 210.0)); // A5
    assert_eq!(paper_mm(12), (257.0, 364.0)); // B4 JIS
    assert_eq!(paper_mm(13), (182.0, 257.0)); // B5 JIS
    assert_eq!(paper_mm(1), (215.9, 279.4)); // Letter
    assert_eq!(paper_mm(5), (215.9, 355.6)); // Legal
    assert_eq!(paper_mm(70), (105.0, 148.0)); // A6
    assert!((paper_pt(9).0 - 595.28).abs() < 0.2);

    let mut wb = Workbook::new_blank();
    let sh = &mut wb.sheets[0];
    sh.set(0, 0, Cell { value: Value::Text("测试表".into()), formula: None });
    sh.page.paper = 11; // A5
    sh.page.landscape = true;
    sh.page.page_order_over_then_down = true;
    sh.page.black_and_white = true;
    sh.page.draft = true;
    sh.page.first_page_number = Some(5);
    sh.page.print_comments = 1;
    sh.page.print_errors = 2;
    sh.page.different_first_page = true;
    sh.page.first_header = "&C首页眉".into();
    sh.page.first_footer = "&R首脚".into();
    sh.page.different_odd_even = true;
    sh.page.even_header = "&L偶页眉".into();
    sh.page.even_footer = "&C偶脚".into();
    sh.page.row_breaks = vec![10];
    sh.page.col_breaks = vec![4];
    sh.page_dirty = true;

    let tmp = std::env::temp_dir().join(format!("cellan_schema_rt_{}.xlsx", std::process::id()));
    sheetcore::xlsx::save_atomic(&tmp, &wb).expect("save xlsx");

    // 校验 worksheet XML 中各打印标签严格满足 ISO/IEC 29500 Schema 顺序
    let f = std::fs::File::open(&tmp).expect("open zip");
    let mut zip = zip::ZipArchive::new(f).expect("read zip");
    let mut xml = String::new();
    std::io::Read::read_to_string(&mut zip.by_name("xl/worksheets/sheet1.xml").unwrap(), &mut xml)
        .unwrap();
    let p_po = xml.find("<printOptions").expect("printOptions");
    let p_pm = xml.find("<pageMargins").expect("pageMargins");
    let p_ps = xml.find("<pageSetup").expect("pageSetup");
    let p_hf = xml.find("<headerFooter").expect("headerFooter");
    let p_rb = xml.find("<rowBreaks").expect("rowBreaks");
    let p_cb = xml.find("<colBreaks").expect("colBreaks");
    assert!(p_po < p_pm && p_pm < p_ps && p_ps < p_hf && p_hf < p_rb && p_rb < p_cb);
    if let Some(p_ext) = xml.find("<extLst") {
        assert!(p_cb < p_ext, "page setup tags must appear before <extLst>");
    }

    // 读回校验全部字段无损还原
    let loaded = sheetcore::xlsx::load(&tmp).expect("reload xlsx");
    let _ = std::fs::remove_file(&tmp);
    let lp = &loaded.sheets[0].page;
    assert_eq!(lp.paper, 11);
    assert!(lp.landscape);
    assert!(lp.page_order_over_then_down);
    assert!(lp.black_and_white);
    assert!(lp.draft);
    assert_eq!(lp.first_page_number, Some(5));
    assert_eq!(lp.print_comments, 1);
    assert_eq!(lp.print_errors, 2);
    assert!(lp.different_first_page);
    assert_eq!(lp.first_header, "&C首页眉");
    assert_eq!(lp.first_footer, "&R首脚");
    assert!(lp.different_odd_even);
    assert_eq!(lp.even_header, "&L偶页眉");
    assert_eq!(lp.even_footer, "&C偶脚");
    assert_eq!(lp.row_breaks, vec![10]);
    assert_eq!(lp.col_breaks, vec![4]);
}

#[test]
fn center_headings_title_corner_and_page_order() {
    let mut wb = Workbook::new_blank();
    wb.sheets[0].set(0, 0, Cell { value: Value::Text("角标".into()), formula: None });
    wb.sheets[0].set(0, 1, Cell { value: Value::Text("列B".into()), formula: None });
    wb.sheets[0].set(1, 0, Cell { value: Value::Text("行2".into()), formula: None });
    wb.sheets[0].set(1, 1, Cell { value: Value::Number(99.0), formula: None });

    // 1. 居中偏移校验
    let l0 = paginate(&wb.sheets[0], &wb.sheets[0].page);
    let o0 = page_ops_with(&wb, 0, &l0, 0, None);
    let x0 = o0.texts.iter().find(|t| t.text == "角标").unwrap().x;
    let y0 = o0.texts.iter().find(|t| t.text == "角标").unwrap().baseline;

    wb.sheets[0].page.center_h = true;
    wb.sheets[0].page.center_v = true;
    let l1 = paginate(&wb.sheets[0], &wb.sheets[0].page);
    let o1 = page_ops_with(&wb, 0, &l1, 0, None);
    let x1 = o1.texts.iter().find(|t| t.text == "角标").unwrap().x;
    let y1 = o1.texts.iter().find(|t| t.text == "角标").unwrap().baseline;
    assert!(x1 > x0 + 50.0, "center_h must shift content right: {x0} -> {x1}");
    assert!(y1 < y0 - 100.0, "center_v must shift content down: {y0} -> {y1}");

    // 2. 行号列标 + 标题行×标题列交叉角
    wb.sheets[0].page.center_h = false;
    wb.sheets[0].page.center_v = false;
    wb.sheets[0].page.headings = true;
    wb.sheets[0].page.title_rows = Some((0, 0));
    wb.sheets[0].page.title_cols = Some((0, 0));
    wb.sheets[0].page.row_breaks = vec![2];
    wb.sheets[0].page.col_breaks = vec![2];
    wb.sheets[0].set(3, 3, Cell { value: Value::Text("右下页内容".into()), formula: None });

    // 先列后行（downThenOver）vs 先行后列（overThenDown）
    wb.sheets[0].page.page_order_over_then_down = false;
    let ld = paginate(&wb.sheets[0], &wb.sheets[0].page);
    assert_eq!(ld.pages.len(), 4);
    assert_eq!(ld.pages[1].cols, ld.pages[0].cols, "downThenOver moves down rows first");

    wb.sheets[0].page.page_order_over_then_down = true;
    let lo = paginate(&wb.sheets[0], &wb.sheets[0].page);
    assert_eq!(lo.pages.len(), 4);
    assert_eq!(lo.pages[1].rows, lo.pages[0].rows, "overThenDown moves across cols first");

    // 第 4 页（右下角）同时应有行号列标 "D"/"4" 与重复标题交叉角 "角标"
    let o_last = page_ops_with(&wb, 0, &lo, 3, None);
    assert!(o_last.texts.iter().any(|t| t.text == "D"), "column heading D");
    assert!(o_last.texts.iter().any(|t| t.text == "4"), "row heading 4");
    assert!(
        o_last.texts.iter().any(|t| t.text == "角标"),
        "title_rows ∩ title_cols corner must repeat on page 4"
    );
}

#[test]
fn all_13_border_styles_diagonals_and_rich_decorations_icons_sparklines() {
    let mut wb = Workbook::new_blank();

    // 13 种 OOXML 边框线型 + 对角斜线
    for style_code in 1u8..=13 {
        let mut st = CellStyle::default();
        st.border = [
            Border { style: style_code, color: [20, 40, 80] },
            Border { style: style_code, color: [20, 40, 80] },
            Border { style: style_code, color: [20, 40, 80] },
            Border { style: style_code, color: [20, 40, 80] },
        ];
        if style_code == 1 {
            st.diagonal = Some(sheetcore::style_ext::DiagBorder {
                style: "thin".into(),
                color: [180, 0, 0],
                color_theme: None,
                up: true,
                down: true,
            });
        }
        push_style(&mut wb, 0, style_code as u32 - 1, 0, st);
        wb.sheets[0].set(
            style_code as u32 - 1,
            0,
            Cell { value: Value::Number(style_code as f64), formula: None },
        );
    }

    // 富文本 + 下划线/双下划线/删除线 + 缩小字体填充 + 竖排文字
    wb.sheets[0].set(0, 1, Cell { value: Value::Text("红绿富文本".into()), formula: None });
    wb.sheets[0].rich.insert(
        (0, 1),
        vec![
            RichRun { text: "红字".into(), bold: true, italic: false, color: Some([220, 20, 20]), size: None, font: None },
            RichRun { text: "蓝斜".into(), bold: false, italic: true, color: Some([20, 60, 200]), size: None, font: None },
        ],
    );
    let mut st_dec = CellStyle::default();
    st_dec.underline = true;
    st_dec.double_underline = true;
    st_dec.strikethrough = true;
    push_style(&mut wb, 0, 1, 1, st_dec);
    wb.sheets[0].set(1, 1, Cell { value: Value::Text("双下划删除".into()), formula: None });

    let mut st_shrink = CellStyle::default();
    st_shrink.shrink_to_fit = true;
    st_shrink.size = 14.0;
    wb.sheets[0].cols.set(2, 48.0);
    push_style(&mut wb, 0, 0, 2, st_shrink);
    wb.sheets[0].set(0, 2, Cell { value: Value::Text("非常非常长的缩小适应文本内容".into()), formula: None });

    let mut st_vert = CellStyle::default();
    st_vert.vertical_text = true;
    wb.sheets[0].rows.set(2, 80.0);
    push_style(&mut wb, 0, 2, 1, st_vert);
    wb.sheets[0].set(2, 1, Cell { value: Value::Text("竖排".into()), formula: None });

    // 条件格式图标集 + 差异边框
    wb.sheets[0].set(0, 3, Cell { value: Value::Number(10.0), formula: None });
    wb.sheets[0].set(1, 3, Cell { value: Value::Number(50.0), formula: None });
    wb.sheets[0].set(2, 3, Cell { value: Value::Number(90.0), formula: None });
    wb.sheets[0].cf.dxfs.push(Dxf {
        bold: true,
        italic: false,
        color: None,
        fill: None,
        font: None,
        num_fmt: None,
        border: Some(DxfBorder {
            sides: [Some(("dashed".into(), [0, 128, 0])), None, None, None],
        }),
    });
    wb.sheets[0].cf.blocks.push(CfBlock {
        ranges: vec![(0, 3, 2, 3)],
        rules: vec![
            CfRule {
                priority: 1,
                stop: false,
                kind: CfKind::IconSet {
                    set: "3TrafficLights1".into(),
                    reverse: false,
                    thresholds: vec![
                        Cfvo::from_type("percent", Some(0.0)),
                        Cfvo::from_type("percent", Some(33.0)),
                        Cfvo::from_type("percent", Some(67.0)),
                    ],
                    show_value: true,
                },
            },
            CfRule {
                priority: 2,
                stop: false,
                kind: CfKind::CellIs {
                    op: CfOp::Greater,
                    a: "80".into(),
                    b: String::new(),
                    dxf: 0,
                },
            },
        ],
    });

    // 迷你图
    wb.sheets[0].sparklines.push(Sparkline {
        r: 3,
        c: 3,
        rect: (0, 3, 2, 3),
        source_sheet: None,
        kind: SparkKind::Line,
        color: [30, 90, 200],
        markers: true,
        high: true,
        low: true,
        first: false,
        last: false,
        negative: false,
        axis: false,
        empty: EmptyAs::Gap,
    });

    let layout = paginate(&wb.sheets[0], &wb.sheets[0].page);
    let out = page_ops_with(&wb, 0, &layout, 0, None);

    assert!(
        out.styled_lines.len() >= 10,
        "dashed/dotted/dash-dot OOXML border styles should emit styled_lines, got {}",
        out.styled_lines.len()
    );
    assert!(
        out.lines.iter().any(|l| l.5 == [180, 0, 0]),
        "diagonal borders should be emitted"
    );
    assert!(
        out.texts.iter().any(|t| t.text == "红字" && t.bold && t.color == [220, 20, 20]),
        "rich run 1 should preserve bold and red color"
    );
    assert!(
        out.texts.iter().any(|t| t.text == "蓝斜" && t.italic && t.color == [20, 60, 200]),
        "rich run 2 should preserve italic and blue color"
    );
    assert!(
        out.texts.iter().any(|t| t.text == "双下划删除" && t.double_underline && t.strikethrough),
        "cell double_underline & strikethrough"
    );
    let shrunk = out.texts.iter().find(|t| t.text.contains("缩小适应")).unwrap();
    assert!(shrunk.size < 10.0, "shrink_to_fit should reduce font size, got {}", shrunk.size);
    assert!(out.texts.iter().any(|t| t.text == "竖") && out.texts.iter().any(|t| t.text == "排"));
    // CF 高档绿色 & 低档红色矢量图标
    assert!(out.polys.iter().any(|(_, c)| *c == [34, 154, 78]), "CF high green icon");
    assert!(out.polys.iter().any(|(_, c)| *c == [217, 63, 56]), "CF low red icon");
    // 迷你图折线
    assert!(!out.stroke_polys.is_empty(), "sparkline should emit stroke_poly");
}

#[test]
fn bw_draft_errors_comments_and_watermarks() {
    let mut wb = Workbook::new_blank();
    push_style(
        &mut wb,
        0,
        0,
        0,
        CellStyle {
            fill: Some([255, 220, 220]),
            color: [200, 0, 0],
            ..CellStyle::default()
        },
    );
    wb.sheets[0].set(0, 0, Cell { value: Value::Text("彩色告警".into()), formula: None });
    wb.sheets[0].set(1, 0, Cell { value: Value::Error(ErrKind::Div0), formula: Some("=1/0".into()) });
    wb.sheets[0].notes.insert(
        CellRef { row: 0, col: 0 },
        Note { r: 0, c: 0, author: "审计员".into(), text: "需核对凭证号".into() },
    );

    // 1. 黑白模式 + 错误值替换为 "--" + 原位批注角标 + 平铺水印
    wb.sheets[0].page.black_and_white = true;
    wb.sheets[0].page.print_errors = 2; // "--"
    wb.sheets[0].page.print_comments = 2; // AsDisplayed
    wb.sheets[0].page.watermark = WatermarkConfig {
        text: "绝密".into(),
        font_size: 40.0,
        angle_deg: 35.0,
        color: [120, 120, 120],
        opacity: 0.2,
        tiled: true,
    };
    let layout = paginate(&wb.sheets[0], &wb.sheets[0].page);
    let out = page_ops_with(&wb, 0, &layout, 0, None);
    let t_color = out.texts.iter().find(|t| t.text == "彩色告警").unwrap().color;
    assert_eq!(t_color, [0, 0, 0], "B&W mode must render text in pure black");
    assert!(
        !out.rects.iter().any(|r| r.4 == [255, 220, 220]),
        "B&W mode must suppress colored cell fill"
    );
    assert!(
        out.texts.iter().any(|t| t.text == "--"),
        "print_errors=2 must replace #DIV/0! with --"
    );
    assert!(!out.polys.is_empty(), "AsDisplayed comment should emit corner triangle");
    assert!(out.watermarks.len() >= 4, "tiled watermark should emit grid of watermarks");

    // 2. 表尾批注汇总页（AtEnd）
    wb.sheets[0].page.print_comments = 1;
    let layout_at_end = paginate(&wb.sheets[0], &wb.sheets[0].page);
    let cpages = comment_summary_pages(&wb, 0, &layout_at_end);
    assert_eq!(cpages.len(), 1);
    assert!(cpages[0].texts.iter().any(|t| t.text.contains("A1")));
    assert!(cpages[0].texts.iter().any(|t| t.text.contains("审计员")));
    assert!(cpages[0].texts.iter().any(|t| t.text.contains("需核对凭证号")));

    // 3. 草稿模式跳过背景与图片
    wb.sheets[0].page.black_and_white = false;
    wb.sheets[0].page.draft = true;
    let out_draft = page_ops_with(&wb, 0, &layout, 0, None);
    assert!(!out_draft.rects.iter().any(|r| r.4 == [255, 220, 220]));
}

#[test]
fn multipage_object_anchor_and_line_chart_use_stroke_polys() {
    let mut wb = Workbook::new_blank();
    for r in 0..80 {
        wb.sheets[0].set(r, 0, Cell { value: Value::Number((r + 1) as f64), formula: None });
        wb.sheets[0].set(r, 1, Cell { value: Value::Number(((r + 1) * 10) as f64), formula: None });
    }
    // 把折线图锚定在第 2 页（行 55..68）
    wb.sheets[0].charts.push(Chart {
        kind: ChartKind::Line,
        title: "月度趋势".into(),
        series: vec![Series {
            name: "收入".into(),
            val_ref: "Sheet1!$B$1:$B$6".into(),
            cats: vec!["1月".into(), "2月".into(), "3月".into(), "4月".into(), "5月".into(), "6月".into()],
            vals: vec![10.0, 25.0, 18.0, 32.0, 29.0, 44.0],
            color: Some([37, 99, 235]),
            ..Series::default()
        }],
        legend: true,
        anchor: (55, 2, 68, 7),
        legend_pos: "b".into(),
        ..Chart::default()
    });
    // 同时在第 2 页锚定一张 BMP 图片
    wb.sheets[0].pictures.push(Picture {
        name: "PicPage2".into(),
        part: "xl/media/pic2.bmp".into(),
        ext: "bmp".into(),
        rect: (55, 0, 60, 1),
        off: [0; 4],
        data: Some(Arc::new(make_2x2_bmp())),
    });

    let layout = paginate(&wb.sheets[0], &wb.sheets[0].page);
    assert!(layout.pages.len() >= 2, "80 rows should span >= 2 pages");
    let (pw, ph) = layout.page_size;

    let out_p2 = page_ops_with(&wb, 0, &layout, 1, None);
    assert!(
        !out_p2.stroke_polys.is_empty(),
        "Line chart on page 2 must emit stroke_polys (not filled closed blobs)"
    );
    for sp in &out_p2.stroke_polys {
        for &(x, y) in &sp.pts {
            assert!(x >= 0.0 && x <= pw && y >= 0.0 && y <= ph, "chart point ({x},{y}) out of page");
        }
    }
    assert_eq!(out_p2.images.len(), 1, "picture anchored on page 2 should appear on page 2");
    let img = &out_p2.images[0];
    assert!(img.0 >= 0.0 && img.1 >= 0.0 && img.0 + img.2 <= pw && img.1 + img.3 <= ph);
}

fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &b in data {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

fn png_chunk(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(12 + payload.len());
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    let mut crc_in = Vec::with_capacity(4 + payload.len());
    crc_in.extend_from_slice(kind);
    crc_in.extend_from_slice(payload);
    out.extend_from_slice(&crc32(&crc_in).to_be_bytes());
    out
}

fn make_2x2_indexed_png() -> Vec<u8> {
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&2u32.to_be_bytes()); // w=2
    ihdr.extend_from_slice(&2u32.to_be_bytes()); // h=2
    ihdr.extend_from_slice(&[8, 3, 0, 0, 0]); // 8-bit Indexed (CT=3)
    png.extend_from_slice(&png_chunk(b"IHDR", &ihdr));
    // Palette: 0=red, 1=green
    png.extend_from_slice(&png_chunk(b"PLTE", &[255, 0, 0, 0, 255, 0]));
    // tRNS: idx 0 transparent, idx 1 opaque
    png.extend_from_slice(&png_chunk(b"tRNS", &[0, 255]));
    let raw = [0u8, 0, 1, 0, 1, 0]; // 2 rows with filter byte 0
    let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut enc, &raw).unwrap();
    let z = enc.finish().unwrap();
    png.extend_from_slice(&png_chunk(b"IDAT", &z));
    png.extend_from_slice(&png_chunk(b"IEND", &[]));
    png
}

fn make_2x2_bmp() -> Vec<u8> {
    // 2x2 24-bit bottom-up BMP (row stride = 8 bytes padded to 4)
    let mut bmp = Vec::new();
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&(54u32 + 16).to_le_bytes()); // file size
    bmp.extend_from_slice(&0u32.to_le_bytes());
    bmp.extend_from_slice(&54u32.to_le_bytes()); // pixel offset
    bmp.extend_from_slice(&40u32.to_le_bytes()); // DIB header size
    bmp.extend_from_slice(&2i32.to_le_bytes()); // w
    bmp.extend_from_slice(&2i32.to_le_bytes()); // h
    bmp.extend_from_slice(&1u16.to_le_bytes()); // planes
    bmp.extend_from_slice(&24u16.to_le_bytes()); // bpp
    bmp.extend_from_slice(&[0u8; 24]); // compression..important
    // Row 0 (bottom): Blue, Green + 2 pad bytes
    bmp.extend_from_slice(&[255, 0, 0, 0, 255, 0, 0, 0]);
    // Row 1 (top): Red, White + 2 pad bytes
    bmp.extend_from_slice(&[0, 0, 255, 255, 255, 255, 0, 0]);
    bmp
}

fn make_1x1_gif() -> Vec<u8> {
    // 标准最小合法 1×1 红色 GIF89a
    vec![
        b'G', b'I', b'F', b'8', b'9', b'a',
        1, 0, 1, 0, // 1x1
        0x80, 0, 0, // GCT 2 colors
        255, 0, 0,  // color 0 = red
        0, 0, 255,  // color 1 = blue
        0x2C, 0, 0, 0, 0, 1, 0, 1, 0, 0, // image descriptor
        2, // LZW min code size = 2
        2, 0x44, 0x01, // sub-block (clear=4, idx=0, eoi=5)
        0, // block terminator
        0x3B, // trailer
    ]
}

#[test]
fn universal_image_codec_supports_indexed_png_bmp_and_gif() {
    let png = make_2x2_indexed_png();
    let img_png = image_embed::make("ImPng", "png", &png).expect("decode indexed PNG (CT=3 + tRNS)");
    assert_eq!((img_png.width, img_png.height), (2, 2));
    assert!(!img_png.dct);

    let bmp = make_2x2_bmp();
    let img_bmp = image_embed::make("ImBmp", "bmp", &bmp).expect("decode 24-bit BMP");
    assert_eq!((img_bmp.width, img_bmp.height), (2, 2));

    let gif = make_1x1_gif();
    let img_gif = image_embed::make("ImGif", "gif", &gif).expect("decode GIF89a");
    assert_eq!((img_gif.width, img_gif.height), (1, 1));
}

#[test]
fn page_ranges_parser_and_workbook_pdf_with_bookmarks_and_cjk_fallback() {
    assert_eq!(parse_page_ranges("1, 3-5, 8, 3", 10), vec![0, 2, 3, 4, 7]);
    assert_eq!(parse_page_ranges("", 3), vec![0, 1, 2]);
    assert_eq!(parse_page_ranges("5-3", 6), vec![2, 3, 4]);

    let mut wb = Workbook::new_blank();
    wb.sheets[0].name = "资产负债表".into();
    wb.sheets[0].set(0, 0, Cell { value: Value::Text("生僻字验证：堃喆镕✓★§".into()), formula: None });
    let mut sh2 = Sheet::new("利润表");
    sh2.set(0, 0, Cell { value: Value::Text("净利润 8,888,888.00".into()), formula: None });
    wb.sheets.push(sh2);

    let (regular, bold) = embedded_fonts();
    // 若运行在 Windows 上且存在 msyh.ttc，则同时验证 .ttc 拆包与 /F3 回退链
    let ttc_path = PathBuf::from(r"C:\Windows\Fonts\msyh.ttc");
    let fallback = if ttc_path.exists() {
        Some(Font::load(&ttc_path).expect("unpack and parse Windows msyh.ttc"))
    } else {
        None
    };
    let had_fallback = fallback.is_some();
    let pack = PdfFontPack {
        regular,
        bold: Some(bold),
        fallback,
        fallback_bold: None,
    };

    let tmp = std::env::temp_dir().join(format!("cellan_wb_bookmarks_{}.pdf", std::process::id()));
    let n = export_workbook_pdf_with_pack(&wb, &tmp, pack, None).expect("export workbook pdf");
    assert_eq!(n, 2);
    let pdf_bytes = std::fs::read(&tmp).expect("read pdf");
    let _ = std::fs::remove_file(&tmp);
    let pdf_txt = String::from_utf8_lossy(&pdf_bytes);
    assert!(pdf_bytes.starts_with(b"%PDF-1."));
    assert!(pdf_txt.contains("/Outlines"), "workbook PDF must emit /Outlines bookmark tree");
    assert!(pdf_txt.contains("/Info"), "PDF must emit /Info metadata dictionary");
    assert!(pdf_txt.contains("/Producer"), "PDF /Info must include /Producer metadata");
    assert!(
        pdf_txt.contains("00430045004C004C0041004E") || pdf_txt.contains("CELLAN"),
        "PDF Producer metadata should contain CELLAN (UTF-16BE or ASCII)"
    );
    if had_fallback {
        assert!(
            pdf_txt.contains("/F3 "),
            "rare glyphs missing from embedded subset should route to /F3 TTC fallback font"
        );
    }
}
