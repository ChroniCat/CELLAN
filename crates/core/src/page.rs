//! 页面设置与打印：纸张、方向、边距、缩放、打印区域、顶端标题行。
//!
//! 打印预览不做真正的分页渲染，而是在网格上画出**分页虚线**——
//! 和 Excel 的「分页预览」一个思路，改列宽/页面设置时立刻能看到分页位置。

use crate::model::Sheet;

#[derive(Clone, Debug, PartialEq)]
pub struct PageSetup {
    pub landscape: bool,
    /// OOXML paperSize：9 = A4，1 = Letter
    pub paper: u16,
    /// 缩放百分比（fit_to_page 为 false 时生效）
    pub scale: u16,
    /// 是否「调整为 1 页宽 × N 页高」
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
    /// 打印区域（0 基）
    pub print_area: Option<(u32, u32, u32, u32)>,
    /// 顶端标题行（重复打印）：(起始行, 结束行)
    pub title_rows: Option<(u32, u32)>,
    /// 左侧标题列
    pub title_cols: Option<(u32, u32)>,
}

impl Default for PageSetup {
    fn default() -> Self {
        Self {
            landscape: false,
            paper: 9, // A4
            scale: 100,
            fit_to_page: false,
            fit_width: 1,
            fit_height: 0,
            margins: [0.7, 0.7, 0.75, 0.75, 0.3, 0.3],
            gridlines: false,
            headings: false,
            center_h: false,
            center_v: false,
            header: String::new(),
            footer: String::new(),
            print_area: None,
            title_rows: None,
            title_cols: None,
        }
    }
}

/// 纸张尺寸（毫米）
pub fn paper_mm(paper: u16) -> (f32, f32) {
    match paper {
        1 => (215.9, 279.4),  // Letter
        3 => (279.4, 431.8),  // Tabloid
        4 => (215.9, 355.6),  // Legal
        5 => (139.7, 215.9),  // Statement
        8 => (297.0, 420.0),  // A3
        9 => (210.0, 297.0),  // A4
        10 => (148.0, 210.0), // A5
        _ => (210.0, 297.0),
    }
}

impl PageSetup {
    pub fn paper_label(&self) -> &'static str {
        match self.paper {
            1 => "Letter",
            3 => "Tabloid",
            4 => "Legal",
            5 => "Statement",
            8 => "A3",
            9 => "A4",
            10 => "A5",
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

    /// 整页尺寸（像素）
    pub fn page_px(&self) -> (f32, f32) {
        let (mut w_mm, mut h_mm) = paper_mm(self.paper);
        if self.landscape {
            std::mem::swap(&mut w_mm, &mut h_mm);
        }
        let k = 96.0 / 25.4;
        (w_mm * k, h_mm * k)
    }

    /// 缩放系数（fit_to_page 时按页宽反推）
    pub fn scale_factor(&self, sheet: &Sheet) -> f32 {
        if self.fit_to_page {
            let used_w: f32 = match self.print_area {
                Some((_, c1, _, c2)) => cols_width(sheet, c1, c2),
                None => match sheet.used_bounds() {
                    Some((_, c1, _, c2)) => cols_width(sheet, c1, c2),
                    None => 1.0,
                },
            };
            let (pw, _) = self.printable_px();
            if used_w > 1.0 {
                return (pw / used_w).clamp(0.1, 1.0);
            }
            1.0
        } else {
            (self.scale as f32 / 100.0).clamp(0.1, 4.0)
        }
    }
}

fn cols_width(sheet: &Sheet, c1: u32, c2: u32) -> f32 {
    let mut w = 0.0;
    for c in c1..=c2.min(c1 + 2000) {
        w += sheet.cols.size(c);
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
        Some(v) => v,
        None => return (Vec::new(), Vec::new()),
    };

    let mut xs = Vec::new();
    let mut acc = 0.0;
    let mut run = 0.0;
    for c in c1..=c2 {
        let w = sheet.cols.size(c);
        if acc + w > usable_w && acc > 0.0 {
            xs.push(acc);
            acc = w;
            run += w;
        } else {
            acc += w;
            run += w;
        }
        let _ = run;
    }

    let mut ys = Vec::new();
    let mut ay = 0.0;
    for r in r1..=r2 {
        let h = sheet.rows.size(r);
        if ay + h > usable_h && ay > 0.0 {
            ys.push(ay);
            ay = h;
        } else {
            ay += h;
        }
    }
    (xs, ys)
}


// ---------------------------------------------------------------------------
// XML
// ---------------------------------------------------------------------------

use quick_xml::events::Event;
use quick_xml::Reader;

/// 解析工作表里的页面设置（pageSetup / pageMargins / printOptions / headerFooter）
pub fn parse_page_setup(xml: &str) -> PageSetup {
    let mut ps = PageSetup::default();
    let mut r = Reader::from_str(xml);
    let mut in_header = false;
    let mut in_footer = false;
    let mut buf = String::new();
    loop {
        match r.read_event() {
            Ok(Event::Empty(e)) | Ok(Event::Start(e)) => {
                let nm = e.name();
                let name = nm.as_ref();
                if name.ends_with("pageSetup") {
                    for a in e.attributes().flatten() {
                        let k = a.key.as_ref();
                        let v = crate::model::xml_unescape(&a.value.to_string());
                        match k {
                            "orientation" => ps.landscape = v == "landscape",
                            "paperSize" => ps.paper = v.parse().unwrap_or(9),
                            "scale" => ps.scale = v.parse().unwrap_or(100),
                            "fitToWidth" => {
                                ps.fit_width = v.parse().unwrap_or(1);
                                ps.fit_to_page = true;
                            }
                            "fitToHeight" => {
                                ps.fit_height = v.parse().unwrap_or(0);
                                ps.fit_to_page = true;
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
                } else if name.ends_with("oddHeader") {
                    in_header = true;
                    buf.clear();
                } else if name.ends_with("oddFooter") {
                    in_footer = true;
                    buf.clear();
                }
            }
            Ok(Event::Text(t)) => {
                if in_header || in_footer {
                    let raw: &str = &t;
                    buf.push_str(&crate::model::xml_unescape(raw));
                }
            }
            Ok(Event::End(e)) => {
                let nm = e.name();
                let name = nm.as_ref();
                if name.ends_with("oddHeader") {
                    ps.header = buf.trim().to_string();
                    in_header = false;
                } else if name.ends_with("oddFooter") {
                    ps.footer = buf.trim().to_string();
                    in_footer = false;
                }
            }
            Ok(Event::GeneralRef(e)) => {
                if in_header || in_footer {
                    let nm: String = e.into_inner().to_string();
                    buf.push_str(&crate::model::resolve_entity(&nm));
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
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
    let Ok(file) = std::fs::File::open(path) else { return out };
    let Ok(mut zip) = zip::ZipArchive::new(file) else { return out };
    let mut xml = String::new();
    {
        use std::io::Read;
        let Ok(mut f) = zip.by_name("xl/workbook.xml") else { return out };
        let _ = f.read_to_string(&mut xml);
    }
    // 先看一共有几张表（sheet 元素数）
    let mut n = 0u32;
    let mut r = Reader::from_str(&xml);
    loop {
        match r.read_event() {
            Ok(Event::Empty(e)) | Ok(Event::Start(e)) if e.name().as_ref().ends_with("sheet") => n += 1,
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
    // 整行：只有数字
    if a.chars().all(|c| c.is_ascii_digit()) && b.chars().all(|c| c.is_ascii_digit()) {
        let r1: u32 = a.parse().ok()?;
        let r2: u32 = b.parse().ok()?;
        return Some((r1.min(r2).saturating_sub(1), 0, r2.max(r1).saturating_sub(1), 16_383));
    }
    // 整列：只有字母
    if a.chars().all(|c| c.is_ascii_alphabetic()) && b.chars().all(|c| c.is_ascii_alphabetic()) {
        let c1 = parse_a1(&format!("{a}1"))?.1;
        let c2 = parse_a1(&format!("{b}1"))?.1;
        return Some((0, c1.min(c2), 1_048_575, c1.max(c2)));
    }
    let pa = parse_a1(a)?;
    let pb = parse_a1(b)?;
    Some((pa.0.min(pb.0), pa.1.min(pb.1), pa.0.max(pb.0), pa.1.max(pb.1)))
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
        let xml = "<worksheet><printOptions gridLines=\"1\" headings=\"1\" horizontalCentered=\"1\"/><pageMargins left=\"0.5\" right=\"0.4\" top=\"0.6\" bottom=\"0.7\" header=\"0.2\" footer=\"0.25\"/><pageSetup orientation=\"landscape\" paperSize=\"8\" scale=\"85\" fitToWidth=\"1\" fitToHeight=\"0\"/><headerFooter><oddHeader>&amp;C页眉</oddHeader><oddFooter>&amp;C页脚</oddFooter></headerFooter></worksheet>";
        let ps = parse_page_setup(xml);
        assert!(ps.landscape);
        assert_eq!(ps.paper, 8);
        assert_eq!(ps.scale, 85);
        assert!(ps.fit_to_page);
        assert_eq!(ps.margins[0], 0.5);
        assert!(ps.gridlines && ps.headings && ps.center_h);
        assert_eq!(ps.header, "&C页眉");
        assert_eq!(ps.footer, "&C页脚");
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
                s.set(r, c, Cell { value: Value::Number(r as f64 + c as f64), formula: None });
            }
        }
        s
    }

    #[test]
    fn a4_portrait_printable() {
        let ps = PageSetup::default();
        let (w, h) = ps.printable_px();
        // A4 210×297mm，左右各 0.7 英寸 → 794 - 134 ≈ 660
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
        // 第一条横线大约是整页高
        assert!(ys[0] > 800.0 && ys[0] < 1000.0, "{ys:?}");
    }

    #[test]
    fn fit_to_page_shrinks_scale() {
        let mut ps = PageSetup::default();
        let s = sheet();
        let a = ps.scale_factor(&s);
        assert!((a - 1.0).abs() < 0.01, "100% 缩放");
        ps.fit_to_page = true;
        let b = ps.scale_factor(&s);
        assert!(b < 1.0, "调整为 1 页宽应缩小: {b}");
        assert!(b > 0.1);
    }
}
