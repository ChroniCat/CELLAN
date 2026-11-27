//! 页面设置与打印模型（ISO/IEC 29500 完整口径 + 财务出版级扩展）：
//! 纸张、方向、边距、缩放、适应页数、打印区域、重复标题行/列、
//! 手动分页符、页面顺序、黑白/草稿打印、批注/错误值打印策略、
//! 首页/奇偶页不同页眉页脚，以及原生矢量水印。

use crate::model::Sheet;

/// 矢量水印配置（超越 Excel：原生支持斜角居中或满页平铺半透明水印）
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct WatermarkConfig {
    pub text: String,
    pub font_size: f32,
    pub angle_deg: f32,
    pub color: [u8; 3],
    pub opacity: f32,
    pub tiled: bool,
}

impl Default for WatermarkConfig {
    fn default() -> Self {
        Self {
            text: String::new(),
            font_size: 46.0,
            angle_deg: 35.0,
            color: [148, 163, 184],
            opacity: 0.18,
            tiled: false,
        }
    }
}

impl WatermarkConfig {
    pub fn is_active(&self) -> bool {
        !self.text.trim().is_empty() && self.opacity > 0.01
    }
}

/// 标准 OOXML 纸张列表（用于页面设置与打印工作室下拉框）
pub const PAPER_SIZES: &[(u16, &str)] = &[
    (9, "A4 (210×297 mm)"),
    (8, "A3 (297×420 mm)"),
    (11, "A5 (148×210 mm)"),
    (12, "B4 JIS (257×364 mm)"),
    (13, "B5 JIS (182×257 mm)"),
    (1, "Letter (8.5×11 in)"),
    (5, "Legal (8.5×14 in)"),
    (3, "Tabloid (11×17 in)"),
    (6, "Statement (5.5×8.5 in)"),
    (70, "A6 (105×148 mm)"),
];

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct PageSetup {
    pub landscape: bool,
    /// ISO/IEC 29500 OOXML paperSize：9 = A4，8 = A3，11 = A5，1 = Letter，5 = Legal，12 = B4，13 = B5
    pub paper: u16,
    /// 缩放百分比（fit_to_page 为 false 时生效，10..=400）
    pub scale: u16,
    /// 是否「调整为 N 页宽 × M 页高」
    pub fit_to_page: bool,
    pub fit_width: u16,
    pub fit_height: u16,
    /// 边距（英寸）：左 右 上 下 页眉 页脚
    pub margins: [f32; 6],
    pub gridlines: bool,
    pub headings: bool,
    pub center_h: bool,
    pub center_v: bool,
    pub header: String,
    pub footer: String,
    /// 首页不同页眉页脚
    pub different_first_page: bool,
    pub first_header: String,
    pub first_footer: String,
    /// 奇偶页不同页眉页脚
    pub different_odd_even: bool,
    pub even_header: String,
    pub even_footer: String,
    /// 打印区域（0 基闭区间）
    pub print_area: Option<(u32, u32, u32, u32)>,
    /// 顶端标题行（重复打印）：(起始行, 结束行)
    pub title_rows: Option<(u32, u32)>,
    /// 左侧标题列
    pub title_cols: Option<(u32, u32)>,
    /// 手动分页：在这些列之前分页（0 基）
    pub col_breaks: Vec<u32>,
    /// 手动分页：在这些行之前分页（0 基）
    pub row_breaks: Vec<u32>,
    /// 分页顺序：false = 先列后行（downThenOver），true = 先行后列（overThenDown）
    pub page_order_over_then_down: bool,
    /// 起始页码（None = 自动从 1 开始）
    pub first_page_number: Option<u32>,
    /// 黑白单色打印（白底黑字高对比，适合财务激光打印机）
    pub black_and_white: bool,
    /// 草稿质量（跳过背景色、图片与图表以极速校对）
    pub draft: bool,
    /// 批注打印方式：0 = 不打印，1 = 工作表末尾汇总（atEnd），2 = 如工作表所示（asDisplayed）
    pub print_comments: u8,
    /// 错误值打印方式：0 = 显示值，1 = 空白，2 = `--`，3 = `#N/A`
    pub print_errors: u8,
    /// 原生矢量水印（居中斜角或平铺）
    pub watermark: WatermarkConfig,
}

impl Default for PageSetup {
    fn default() -> Self {
        Self {
            landscape: false,
            paper: 9, // A4
            scale: 100,
            fit_to_page: false,
            fit_width: 0,
            fit_height: 0,
            margins: [0.7, 0.7, 0.75, 0.75, 0.3, 0.3],
            gridlines: false,
            headings: false,
            center_h: false,
            center_v: false,
            header: String::new(),
            footer: String::new(),
            different_first_page: false,
            first_header: String::new(),
            first_footer: String::new(),
            different_odd_even: false,
            even_header: String::new(),
            even_footer: String::new(),
            print_area: None,
            title_rows: None,
            title_cols: None,
            col_breaks: Vec::new(),
            row_breaks: Vec::new(),
            page_order_over_then_down: false,
            first_page_number: None,
            black_and_white: false,
            draft: false,
            print_comments: 0,
            print_errors: 0,
            watermark: WatermarkConfig::default(),
        }
    }
}

/// 标准 ISO/IEC 29500 纸张尺寸（毫米，纵向 `(宽, 高)`）
pub fn paper_mm(paper: u16) -> (f32, f32) {
    match paper {
        1 => (215.9, 279.4),  // Letter
        3 => (279.4, 431.8),  // Tabloid
        4 => (431.8, 279.4),  // Ledger
        5 => (215.9, 355.6),  // Legal
        6 => (139.7, 215.9),  // Statement
        7 => (184.15, 266.7), // Executive
        8 => (297.0, 420.0),  // A3
        9 | 10 => (210.0, 297.0), // A4 / A4 Small
        11 => (148.0, 210.0), // A5
        12 => (257.0, 364.0), // B4 (JIS)
        13 => (182.0, 257.0), // B5 (JIS)
        70 => (105.0, 148.0), // A6
        _ => (210.0, 297.0),
    }
}

/// 纸张尺寸（PDF 点，1 in = 72 pt，纵向 `(宽, 高)`）
pub fn paper_pt(paper: u16) -> (f32, f32) {
    let (w_mm, h_mm) = paper_mm(paper);
    let k = 72.0 / 25.4;
    (w_mm * k, h_mm * k)
}

impl PageSetup {
    pub fn paper_label(&self) -> &'static str {
        match self.paper {
            1 => "Letter",
            3 => "Tabloid",
            4 => "Ledger",
            5 => "Legal",
            6 => "Statement",
            7 => "Executive",
            8 => "A3",
            9 | 10 => "A4",
            11 => "A5",
            12 => "B4",
            13 => "B5",
            70 => "A6",
            _ => "A4",
        }
    }

    /// 可打印区域（像素，96 DPI），已扣掉边距
    pub fn printable_px(&self) -> (f32, f32) {
        let (mut w_mm, mut h_mm) = paper_mm(self.paper);
        if self.landscape {
            std::mem::swap(&mut w_mm, &mut h_mm);
        }
        let mm_to_px = 96.0 / 25.4;
        let w = w_mm * mm_to_px - (self.margins[0] + self.margins[1]) * 96.0;
        let h = h_mm * mm_to_px - (self.margins[2] + self.margins[3]) * 96.0;
        (w.max(80.0), h.max(80.0))
    }

    /// 整页尺寸（像素，96 DPI）
    pub fn page_px(&self) -> (f32, f32) {
        let (mut w_mm, mut h_mm) = paper_mm(self.paper);
        if self.landscape {
            std::mem::swap(&mut w_mm, &mut h_mm);
        }
        let k = 96.0 / 25.4;
        (w_mm * k, h_mm * k)
    }

    /// 打印内容区的像素尺寸（有打印区域按打印区域，否则按已用区域；剔除隐藏行列）
    pub fn content_px(&self, sheet: &Sheet) -> (f32, f32) {
        let rect = match self.print_area {
            Some(r) => Some(r),
            None => sheet.used_bounds(),
        };
        match rect {
            Some((r1, c1, r2, c2)) => (
                cols_width(sheet, c1, c2).max(1.0),
                rows_height(sheet, r1, r2).max(1.0),
            ),
            None => (1.0, 1.0),
        }
    }

    /// 缩放系数。fit_to_page 时按「宽 N 页 / 高 M 页」两个方向分别算，取更小的那个；
    /// 页数为 0 表示该方向不限（Excel 的「自动」）。只缩不放，上限 100%。
    pub fn scale_factor(&self, sheet: &Sheet) -> f32 {
        if self.fit_to_page {
            let (pw, ph) = self.printable_px();
            let (aw, ah) = self.content_px(sheet);
            let mut best = f32::INFINITY;
            if self.fit_width > 0 && aw > 0.0 {
                best = best.min(pw * self.fit_width as f32 / aw);
            }
            if self.fit_height > 0 && ah > 0.0 {
                best = best.min(ph * self.fit_height as f32 / ah);
            }
            if best.is_finite() {
                best.clamp(0.1, 1.0)
            } else {
                1.0
            }
        } else {
            (self.scale as f32 / 100.0).clamp(0.1, 4.0)
        }
    }

    /// 取出第 `page_no`（1 基）页对应的页眉与页脚原始模板串
    pub fn header_footer_for_page(&self, page_no_1based: usize) -> (&str, &str) {
        if self.different_first_page && page_no_1based == 1 {
            return (&self.first_header, &self.first_footer);
        }
        if self.different_odd_even && page_no_1based.is_multiple_of(2) {
            return (&self.even_header, &self.even_footer);
        }
        (&self.header, &self.footer)
    }
}

fn rows_height(sheet: &Sheet, r1: u32, r2: u32) -> f32 {
    let mut h = 0.0;
    for r in r1..=r2.min(r1 + 20000) {
        if !sheet.hidden_rows.contains(&r) && !sheet.hidden_rows_user.contains(&r) {
            h += sheet.rows.size(r);
        }
    }
    h
}

fn cols_width(sheet: &Sheet, c1: u32, c2: u32) -> f32 {
    let mut w = 0.0;
    for c in c1..=c2.min(c1 + 2000) {
        if !sheet.hidden_cols.contains(&c) {
            w += sheet.cols.size(c);
        }
    }
    w
}

/// 分页线位置（内容坐标，未缩放）：返回竖线 x 列表与横线 y 列表
pub fn page_breaks(sheet: &Sheet) -> (Vec<f32>, Vec<f32>) {
    let ps = &sheet.page;
    let (pw, ph) = ps.printable_px();
    let scale = ps.scale_factor(sheet);
    let usable_w = pw / scale;
    let usable_h = ph / scale;

    let (r1, c1, r2, c2) = match ps.print_area.or_else(|| sheet.used_bounds()) {
        Some((ar1, ac1, ar2, ac2)) => {
            if (ar2.saturating_sub(ar1) as u64 + 1) * (ac2.saturating_sub(ac1) as u64 + 1) > 500_000 {
                if let Some((_, _, ur2, uc2)) = sheet.used_bounds() {
                    (ar1, ac1, ar2.min(ur2.max(ar1)), ac2.min(uc2.max(ac1)))
                } else {
                    return (Vec::new(), Vec::new());
                }
            } else {
                (ar1, ac1, ar2, ac2)
            }
        }
        None => return (Vec::new(), Vec::new()),
    };

    let mut xs = Vec::new();
    let mut acc = 0.0;
    let mut cum_x = 0.0;
    for c in c1..=c2 {
        let w = if sheet.hidden_cols.contains(&c) {
            0.0
        } else {
            sheet.cols.size(c)
        };
        if c > c1 && ps.col_breaks.contains(&c) {
            xs.push(cum_x);
            acc = w;
        } else if acc + w > usable_w && acc > 0.0 {
            xs.push(cum_x);
            acc = w;
        } else {
            acc += w;
        }
        cum_x += w;
    }

    let mut ys = Vec::new();
    let mut ay = 0.0;
    let mut cum_y = 0.0;
    for r in r1..=r2 {
        let h = if sheet.hidden_rows.contains(&r) || sheet.hidden_rows_user.contains(&r) {
            0.0
        } else {
            sheet.rows.size(r)
        };
        if r > r1 && ps.row_breaks.contains(&r) {
            ys.push(cum_y);
            ay = h;
        } else if ay + h > usable_h && ay > 0.0 {
            ys.push(cum_y);
            ay = h;
        } else {
            ay += h;
        }
        cum_y += h;
    }

    xs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    xs.dedup_by(|a, b| (*a - *b).abs() < 0.5);
    ys.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    ys.dedup_by(|a, b| (*a - *b).abs() < 0.5);
    (xs, ys)
}

// ---------------------------------------------------------------------------
// XML 解析
// ---------------------------------------------------------------------------

use quick_xml::events::Event;
use quick_xml::Reader;

/// 解析工作表里的页面设置（pageSetup / pageMargins / printOptions / headerFooter / breaks）
pub fn parse_page_setup(xml: &str) -> PageSetup {
    let mut ps = PageSetup::default();
    let mut r = Reader::from_str(xml);
    let mut hf_slot: u8 = 0; // 1=oddH, 2=oddF, 3=firstH, 4=firstF, 5=evenH, 6=evenF
    let mut buf = String::new();
    let mut fit_flag = false;
    let mut saw_fit_width = false;
    let mut saw_fit_height = false;
    let mut use_first_page_num = false;
    let mut parsed_first_page_num: Option<u32> = None;

    loop {
        match r.read_event() {
            Ok(Event::Empty(e)) | Ok(Event::Start(e)) => {
                let nm = e.name();
                let name = nm.as_ref();
                if name.ends_with("pageSetUpPr") {
                    for a in e.attributes().flatten() {
                        if a.key.as_ref() == "fitToPage" {
                            let v = a.value.to_string();
                            fit_flag = v == "1" || v == "true";
                        }
                    }
                } else if name.ends_with("pageSetup") {
                    for a in e.attributes().flatten() {
                        let k = a.key.as_ref();
                        let v = crate::model::xml_unescape(&a.value.to_string());
                        match k {
                            "orientation" => ps.landscape = v == "landscape",
                            "paperSize" => ps.paper = v.parse().unwrap_or(9),
                            "scale" => ps.scale = v.parse().unwrap_or(100),
                            "fitToWidth" => {
                                ps.fit_width = v.parse().unwrap_or(0);
                                saw_fit_width = true;
                            }
                            "fitToHeight" => {
                                ps.fit_height = v.parse().unwrap_or(0);
                                saw_fit_height = true;
                            }
                            "pageOrder" => {
                                ps.page_order_over_then_down = v == "overThenDown";
                            }
                            "blackAndWhite" => {
                                ps.black_and_white = v == "1" || v == "true";
                            }
                            "draft" => {
                                ps.draft = v == "1" || v == "true";
                            }
                            "useFirstPageNumber" => {
                                use_first_page_num = v == "1" || v == "true";
                            }
                            "firstPageNumber" => {
                                parsed_first_page_num = v.parse::<u32>().ok();
                            }
                            "cellComments" => {
                                ps.print_comments = match v.as_str() {
                                    "atEnd" => 1,
                                    "asDisplayed" => 2,
                                    _ => 0,
                                };
                            }
                            "errors" => {
                                ps.print_errors = match v.as_str() {
                                    "blank" => 1,
                                    "dash" => 2,
                                    "NA" => 3,
                                    _ => 0,
                                };
                            }
                            _ => {}
                        }
                    }
                } else if name.ends_with("pageMargins") {
                    for a in e.attributes().flatten() {
                        let k = a.key.as_ref();
                        let v = crate::model::xml_unescape(&a.value.to_string());
                        let val = v.parse().unwrap_or(0.0);
                        match k {
                            "left" => ps.margins[0] = val,
                            "right" => ps.margins[1] = val,
                            "top" => ps.margins[2] = val,
                            "bottom" => ps.margins[3] = val,
                            "header" => ps.margins[4] = val,
                            "footer" => ps.margins[5] = val,
                            _ => {}
                        }
                    }
                } else if name.ends_with("printOptions") {
                    for a in e.attributes().flatten() {
                        let k = a.key.as_ref();
                        let v = crate::model::xml_unescape(&a.value.to_string());
                        let on = v == "1" || v == "true";
                        match k {
                            "gridLines" => ps.gridlines = on,
                            "headings" => ps.headings = on,
                            "horizontalCentered" => ps.center_h = on,
                            "verticalCentered" => ps.center_v = on,
                            _ => {}
                        }
                    }
                } else if name.ends_with("headerFooter") {
                    for a in e.attributes().flatten() {
                        let k = a.key.as_ref();
                        let v = a.value.to_string();
                        let on = v == "1" || v == "true";
                        match k {
                            "differentFirst" => ps.different_first_page = on,
                            "differentOddEven" => ps.different_odd_even = on,
                            _ => {}
                        }
                    }
                } else if name.ends_with("oddHeader") {
                    hf_slot = 1;
                    buf.clear();
                } else if name.ends_with("oddFooter") {
                    hf_slot = 2;
                    buf.clear();
                } else if name.ends_with("firstHeader") {
                    hf_slot = 3;
                    buf.clear();
                } else if name.ends_with("firstFooter") {
                    hf_slot = 4;
                    buf.clear();
                } else if name.ends_with("evenHeader") {
                    hf_slot = 5;
                    buf.clear();
                } else if name.ends_with("evenFooter") {
                    hf_slot = 6;
                    buf.clear();
                } else if name.ends_with("watermark") {
                    for a in e.attributes().flatten() {
                        let k = a.key.as_ref();
                        let v = crate::model::xml_unescape(&a.value.to_string());
                        match k {
                            "text" => ps.watermark.text = v,
                            "size" => ps.watermark.font_size = v.parse().unwrap_or(46.0),
                            "angle" => ps.watermark.angle_deg = v.parse().unwrap_or(35.0),
                            "opacity" => ps.watermark.opacity = v.parse().unwrap_or(0.18),
                            "tiled" => ps.watermark.tiled = v == "1" || v == "true",
                            "color" => {
                                if v.len() == 6
                                    && let Ok(n) = u32::from_str_radix(&v, 16)
                                {
                                    ps.watermark.color =
                                        [(n >> 16) as u8, (n >> 8) as u8, n as u8];
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            Ok(Event::Text(t)) => {
                if hf_slot != 0 {
                    let raw: &str = &t;
                    buf.push_str(&crate::model::xml_unescape(raw));
                }
            }
            Ok(Event::End(e)) => {
                let nm = e.name();
                let name = nm.as_ref();
                let trimmed = || buf.trim().to_string();
                if name.ends_with("oddHeader") {
                    ps.header = trimmed();
                    hf_slot = 0;
                } else if name.ends_with("oddFooter") {
                    ps.footer = trimmed();
                    hf_slot = 0;
                } else if name.ends_with("firstHeader") {
                    ps.first_header = trimmed();
                    hf_slot = 0;
                } else if name.ends_with("firstFooter") {
                    ps.first_footer = trimmed();
                    hf_slot = 0;
                } else if name.ends_with("evenHeader") {
                    ps.even_header = trimmed();
                    hf_slot = 0;
                } else if name.ends_with("evenFooter") {
                    ps.even_footer = trimmed();
                    hf_slot = 0;
                }
            }
            Ok(Event::GeneralRef(e)) => {
                if hf_slot != 0 {
                    let nm: String = e.into_inner().to_string();
                    buf.push_str(&crate::model::resolve_entity(&nm));
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }
    if use_first_page_num {
        ps.first_page_number = parsed_first_page_num;
    }
    if fit_flag {
        if !saw_fit_width {
            ps.fit_width = 1;
        }
        if !saw_fit_height {
            ps.fit_height = 1;
        }
    }
    ps.fit_to_page = fit_flag || ps.fit_width > 0 || ps.fit_height > 0;
    // 手动分页
    for (tag, is_row) in [("rowBreaks", true), ("colBreaks", false)] {
        let Some(start) = xml.find(&format!("<{tag}")) else {
            continue;
        };
        let rest = &xml[start..];
        let seg = rest
            .find(&format!("</{tag}>"))
            .map(|e| &rest[..e])
            .unwrap_or(rest);
        let mut v = Vec::new();
        let mut from = 0usize;
        while let Some(i) = seg[from..].find("<brk ") {
            let i = from + i;
            let Some(gt) = seg[i..].find('>').map(|e| i + e) else {
                break;
            };
            let tag = &seg[i..=gt];
            if let Some(p) = tag.find("id=\"") {
                let rest = &tag[p + 4..];
                if let Some(q) = rest.find('"') {
                    if let Ok(id) = rest[..q].parse::<u32>() {
                        v.push(id);
                    }
                }
            }
            from = gt + 1;
        }
        v.sort_unstable();
        v.dedup();
        if is_row {
            ps.row_breaks = v;
        } else {
            ps.col_breaks = v;
        }
    }
    ps
}

type Rect = (u32, u32, u32, u32);

/// 从文件里读每张表的打印区域/标题（key = 工作表序号）
pub fn load_defined_names(
    path: &std::path::Path,
) -> std::collections::HashMap<u32, (Option<Rect>, Option<(u32, u32)>, Option<(u32, u32)>)> {
    let mut out = std::collections::HashMap::new();
    let Ok(file) = std::fs::File::open(path) else {
        return out;
    };
    let Ok(mut zip) = zip::ZipArchive::new(file) else {
        return out;
    };
    let mut xml = String::new();
    {
        use std::io::Read;
        let Ok(mut f) = zip.by_name("xl/workbook.xml") else {
            return out;
        };
        let _ = f.read_to_string(&mut xml);
    }
    let mut n = 0u32;
    let mut r = Reader::from_str(&xml);
    loop {
        match r.read_event() {
            Ok(Event::Empty(e)) | Ok(Event::Start(e)) if e.name().as_ref().ends_with("sheet") => {
                n += 1
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }
    for i in 0..n {
        let (a, rows, cols) = parse_defined_names(&xml, i);
        if a.is_some() || rows.is_some() || cols.is_some() {
            out.insert(i, (a, rows, cols));
        }
    }
    out
}

/// 从 workbook.xml 的 definedNames 里取某张表的打印区域/标题行/标题列
pub fn parse_defined_names(
    xml: &str,
    sheet_index: u32,
) -> (Option<Rect>, Option<(u32, u32)>, Option<(u32, u32)>) {
    let mut area = None;
    let mut rows = None;
    let mut cols = None;
    let mut r = Reader::from_str(xml);
    let mut cur: Option<(String, u32)> = None;
    let mut text = String::new();
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) if e.name().as_ref().ends_with("definedName") => {
                let mut name = String::new();
                let mut local = 0u32;
                for a in e.attributes().flatten() {
                    let k = a.key.as_ref();
                    let v = crate::model::xml_unescape(&a.value.to_string());
                    if k == "name" {
                        name = v;
                    } else if k == "localSheetId" {
                        local = v.parse().unwrap_or(0);
                    }
                }
                cur = Some((name, local));
                text.clear();
            }
            Ok(Event::Text(t)) => {
                if cur.is_some() {
                    let raw: &str = &t;
                    text.push_str(&crate::model::xml_unescape(raw));
                }
            }
            Ok(Event::End(e)) if e.name().as_ref().ends_with("definedName") => {
                if let Some((name, local)) = cur.take() {
                    if local == sheet_index {
                        let v = text.trim();
                        if name.ends_with("Print_Area") {
                            if let Some(rect) = parse_ref_rect(v) {
                                area = Some(rect);
                            }
                        } else if name.ends_with("Print_Titles") {
                            for part in v.split(',') {
                                if let Some(rect) = parse_ref_rect(part.trim()) {
                                    if rect.3 == 16_383 {
                                        rows = Some((rect.0, rect.2));
                                    } else if rect.2 == 1_048_575 {
                                        cols = Some((rect.1, rect.3));
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }
    (area, rows, cols)
}

/// "'表名'!$A$1:$F$20" → (0,0,19,5)；也支持整行 `$1:$2` 和整列 `$A:$A`
fn parse_ref_rect(s: &str) -> Option<(u32, u32, u32, u32)> {
    let tail = match s.rsplit_once('!') {
        Some((_, t)) => t,
        None => s,
    };
    let t = tail.replace('$', "");
    let (a, b) = t.split_once(':')?;
    let a = a.trim();
    let b = b.trim();
    if a.chars().all(|c| c.is_ascii_digit()) && b.chars().all(|c| c.is_ascii_digit()) {
        let r1: u32 = a.parse().ok()?;
        let r2: u32 = b.parse().ok()?;
        return Some((
            r1.min(r2).saturating_sub(1),
            0,
            r2.max(r1).saturating_sub(1),
            16_383,
        ));
    }
    if a.chars().all(|c| c.is_ascii_alphabetic()) && b.chars().all(|c| c.is_ascii_alphabetic()) {
        let c1 = parse_a1(&format!("{a}1"))?.1;
        let c2 = parse_a1(&format!("{b}1"))?.1;
        return Some((0, c1.min(c2), 1_048_575, c1.max(c2)));
    }
    let pa = parse_a1(a)?;
    let pb = parse_a1(b)?;
    Some((
        pa.0.min(pb.0),
        pa.1.min(pb.1),
        pa.0.max(pb.0),
        pa.1.max(pb.1),
    ))
}

fn parse_a1(s: &str) -> Option<(u32, u32)> {
    let mut col = 0u32;
    let mut row = 0u32;
    let mut digits = false;
    for ch in s.trim().chars() {
        if ch.is_ascii_alphabetic() && !digits {
            col = col * 26 + (ch.to_ascii_uppercase() as u32 - 'A' as u32 + 1);
        } else if ch.is_ascii_digit() {
            digits = true;
            row = row * 10 + (ch as u32 - '0' as u32);
        } else {
            return None;
        }
    }
    if col == 0 || row == 0 {
        return None;
    }
    Some((row - 1, col - 1))
}

#[cfg(test)]
mod xml_tests {
    use super::*;

    #[test]
    fn parses_page_setup() {
        let xml = "<worksheet><printOptions gridLines=\"1\" headings=\"1\" horizontalCentered=\"1\"/><pageMargins left=\"0.5\" right=\"0.4\" top=\"0.6\" bottom=\"0.7\" header=\"0.2\" footer=\"0.25\"/><pageSetup orientation=\"landscape\" paperSize=\"8\" scale=\"85\" fitToWidth=\"1\" fitToHeight=\"0\" pageOrder=\"overThenDown\" blackAndWhite=\"1\" draft=\"1\" useFirstPageNumber=\"1\" firstPageNumber=\"5\" cellComments=\"atEnd\" errors=\"dash\"/><headerFooter differentFirst=\"1\" differentOddEven=\"1\"><oddHeader>&amp;C页眉</oddHeader><oddFooter>&amp;C页脚</oddFooter><firstHeader>&amp;C首页眉</firstHeader><evenFooter>&amp;R偶页脚</evenFooter></headerFooter></worksheet>";
        let ps = parse_page_setup(xml);
        assert!(ps.landscape);
        assert_eq!(ps.paper, 8);
        assert_eq!(ps.scale, 85);
        assert!(ps.fit_to_page);
        assert_eq!(ps.margins[0], 0.5);
        assert!(ps.gridlines && ps.headings && ps.center_h);
        assert_eq!(ps.header, "&C页眉");
        assert_eq!(ps.footer, "&C页脚");
        assert!(ps.page_order_over_then_down);
        assert!(ps.black_and_white && ps.draft);
        assert_eq!(ps.first_page_number, Some(5));
        assert_eq!(ps.print_comments, 1);
        assert_eq!(ps.print_errors, 2);
        assert!(ps.different_first_page && ps.different_odd_even);
        assert_eq!(ps.first_header, "&C首页眉");
        assert_eq!(ps.even_footer, "&R偶页脚");
    }

    #[test]
    fn parses_defined_names() {
        let xml = "<workbook><definedNames><definedName name=\"_xlnm.Print_Area\" localSheetId=\"0\">'数据'!$A$1:$F$20</definedName><definedName name=\"_xlnm.Print_Titles\" localSheetId=\"0\">'数据'!$1:$2,'数据'!$A:$A</definedName></definedNames></workbook>";
        let (area, rows, cols) = parse_defined_names(xml, 0);
        assert_eq!(area, Some((0, 0, 19, 5)));
        assert_eq!(rows, Some((0, 1)));
        assert_eq!(cols, Some((0, 0)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Cell, Value};

    fn sheet() -> Sheet {
        let mut s = Sheet::new("T");
        for r in 0..100u32 {
            for c in 0..12u32 {
                s.set(
                    r,
                    c,
                    Cell {
                        value: Value::Number(r as f64 + c as f64),
                        formula: None,
                    },
                );
            }
        }
        s
    }

    #[test]
    fn a4_portrait_printable() {
        let ps = PageSetup::default();
        let (w, h) = ps.printable_px();
        assert!((w - 660.0).abs() < 12.0, "宽 {w}");
        assert!((h - 978.0).abs() < 16.0, "高 {h}");
        let (pw, ph) = ps.page_px();
        assert!(pw < ph, "A4 竖版应是窄高：{pw}x{ph}");
    }

    #[test]
    fn landscape_swaps() {
        let mut ps = PageSetup::default();
        let (pw0, ph0) = ps.page_px();
        ps.landscape = true;
        let (pw1, ph1) = ps.page_px();
        assert!((pw1 - ph0).abs() < 0.5 && (ph1 - pw0).abs() < 0.5);
        assert!(pw1 > ph1);
    }

    #[test]
    fn page_breaks_split_long_table() {
        let s = sheet();
        let (xs, ys) = page_breaks(&s);
        assert!(!ys.is_empty(), "100 行 22px 应该分成多页");
        assert!(!xs.is_empty(), "12 列 104px 在 A4 竖版应分成多页");
        assert!(ys[0] > 800.0 && ys[0] < 1000.0, "{ys:?}");
    }

    #[test]
    fn manual_row_break_is_included() {
        let mut s = sheet();
        s.page.row_breaks = vec![5];
        let (_, ys) = page_breaks(&s);
        let want = s.rows.pos(5) - s.rows.pos(0);
        assert!(
            ys.iter().any(|y| (y - want).abs() < 0.6),
            "手动分页应出现在 {want} 附近：{ys:?}"
        );
    }

    #[test]
    fn fit_to_page_shrinks_scale() {
        let mut ps = PageSetup::default();
        let s = sheet();
        let a = ps.scale_factor(&s);
        assert!((a - 1.0).abs() < 0.01, "100% 缩放");
        ps.fit_to_page = true;
        ps.fit_width = 1;
        let b = ps.scale_factor(&s);
        assert!(b < 1.0, "调整为 1 页宽应缩小: {b}");
        assert!(b > 0.1);
    }

    #[test]
    fn fit_parsing_keeps_auto_direction() {
        let ps = parse_page_setup(
            "<worksheet><pageSetup fitToWidth=\"1\" fitToHeight=\"0\"/></worksheet>",
        );
        assert!(ps.fit_to_page);
        assert_eq!(ps.fit_width, 1);
        assert_eq!(ps.fit_height, 0, "0 是高度自动，不能当成 0 页");
        let ps2 = parse_page_setup(
            "<worksheet><pageSetup fitToWidth=\"0\" fitToHeight=\"0\" scale=\"80\"/></worksheet>",
        );
        assert!(!ps2.fit_to_page, "两个方向都自动时不是 fit-to-page");
        assert_eq!(ps2.scale, 80);
    }

    #[test]
    fn two_pages_wide_is_looser_than_one() {
        let s = sheet();
        let mut ps = PageSetup::default();
        ps.fit_to_page = true;
        ps.fit_width = 1;
        let one = ps.scale_factor(&s);
        ps.fit_width = 2;
        let two = ps.scale_factor(&s);
        assert!(two > one + 0.1, "2 页宽应更宽松: {one} -> {two}");
        assert!(two <= 1.0);
    }

    #[test]
    fn fit_height_limits_scale() {
        let s = sheet();
        let mut ps = PageSetup::default();
        ps.fit_to_page = true;
        ps.fit_width = 0;
        ps.fit_height = 1;
        let by_h = ps.scale_factor(&s);
        assert!(by_h < 1.0, "100 行必须缩到一页: {by_h}");
    }
}

#[cfg(test)]
mod fit_flag_tests {
    use super::*;

    #[test]
    fn excel_fit_flag_without_fit_width_means_one_page_wide() {
        let xml = "<worksheet><sheetPr><pageSetUpPr fitToPage=\"1\"/></sheetPr>\
            <pageSetup paperSize=\"9\" fitToHeight=\"0\" orientation=\"landscape\"/>\
            </worksheet>";
        let ps = parse_page_setup(xml);
        assert!(ps.landscape);
        assert!(ps.fit_to_page);
        assert_eq!(ps.fit_width, 1, "Excel 省略默认的 fitToWidth=1");
        assert_eq!(ps.fit_height, 0, "fitToHeight=0 是自动");
    }
}
