//! 老式 .xls（BIFF8）的单元格样式（含字体颜色、单元格填充底色、自定义调色板、边框、整行/整列样式）。
//!
//! calamine 只给值，不给 .xls 样式。这里解析 PALETTE / FONT / FORMAT / XF 四类全局记录，
//! 再把每个工作表里的 COLINFO / ROW / 单元格记录的 XF 索引接上，完整还原字体、字号、
//! 粗斜体/下划线/删除线、字体颜色、填充背景色、四边边框、对齐、换行和数字格式。

use crate::style::CellStyle;
use std::collections::HashMap;
use std::path::Path;

const BOUNDSHEET: u16 = 0x0085;
const FONT: u16 = 0x0031;
const FORMAT: u16 = 0x041E;
const XF: u16 = 0x00E0;
const PALETTE_REC: u16 = 0x0092;
const COLINFO: u16 = 0x007D;
const ROW_REC: u16 = 0x0208;

const LABELSST: u16 = 0x00FD;
const LABEL: u16 = 0x0204;
const NUMBER: u16 = 0x0203;
const RK: u16 = 0x027E;
const MULRK: u16 = 0x00BD;
const BOOLERR: u16 = 0x0205;
const BLANK: u16 = 0x0201;
const MULBLANK: u16 = 0x00BE;
const FORMULA: u16 = 0x0006;

fn u16le(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes([*b.get(at)?, *b.get(at + 1)?]))
}
fn u32le(b: &[u8], at: usize) -> Option<u32> {
    let mut v = [0u8; 4];
    for i in 0..4 {
        v[i] = *b.get(at + i)?;
    }
    Some(u32::from_le_bytes(v))
}

/// BIFF8 内置数字格式（常用的一部分）
pub fn builtin_format(id: u16) -> Option<&'static str> {
    Some(match id {
        0 => "",
        1 => "0",
        2 => "0.00",
        3 => "#,##0",
        4 => "#,##0.00",
        5 => "¥#,##0",
        6 => "¥#,##0.00",
        7 => "¥#,##0.00",
        8 => "¥#,##0.00",
        9 => "0%",
        10 => "0.00%",
        11 => "0.00E+00",
        12 => "# ?/?",
        13 => "# ??/??",
        14 => "yyyy-mm-dd",
        15 => "d-mmm-yy",
        16 => "d-mmm",
        17 => "mmm-yy",
        18 => "h:mm AM/PM",
        19 => "h:mm:ss AM/PM",
        20 => "h:mm",
        21 => "h:mm:ss",
        22 => "yyyy-mm-dd h:mm",
        37 => "#,##0;(#,##0)",
        38 => "#,##0;[Red](#,##0)",
        39 => "#,##0.00;(#,##0.00)",
        40 => "#,##0.00;[Red](#,##0.00)",
        45 => "mm:ss",
        46 => "[h]:mm:ss",
        47 => "mm:ss.0",
        48 => "##0.0E+0",
        49 => "@",
        _ => return None,
    })
}

/// BIFF8 标准 64 项调色板（0..7 为基础 8 色，8..63 为可由 PALETTE 记录重定义的 56 色）
pub(super) const DEFAULT_PALETTE: [[u8; 3]; 64] = [
    [0, 0, 0],       [255, 255, 255], [255, 0, 0],     [0, 255, 0],
    [0, 0, 255],     [255, 255, 0],   [255, 0, 255],   [0, 255, 255],
    [0, 0, 0],       [255, 255, 255], [255, 0, 0],     [0, 255, 0],
    [0, 0, 255],     [255, 255, 0],   [255, 0, 255],   [0, 255, 255],
    [128, 0, 0],     [0, 128, 0],     [0, 0, 128],     [128, 128, 0],
    [128, 0, 128],   [0, 128, 128],   [192, 192, 192], [128, 128, 128],
    [153, 153, 255], [153, 51, 102],  [255, 255, 204], [204, 255, 255],
    [102, 0, 102],   [255, 128, 128], [0, 102, 204],   [204, 204, 255],
    [0, 0, 128],     [255, 0, 255],   [255, 255, 0],   [0, 255, 255],
    [128, 0, 128],   [128, 0, 0],     [0, 128, 128],   [0, 0, 255],
    [0, 204, 255],   [204, 255, 255], [204, 255, 204], [255, 255, 153],
    [153, 204, 255], [255, 153, 204], [204, 153, 255], [255, 204, 153],
    [51, 102, 255],  [51, 204, 204],  [153, 204, 0],   [255, 204, 0],
    [255, 153, 0],   [255, 102, 0],   [102, 102, 153], [150, 150, 150],
    [0, 51, 102],    [51, 153, 102],  [0, 51, 0],      [51, 51, 0],
    [153, 51, 0],    [153, 51, 102],  [51, 51, 153],   [51, 51, 51],
];

fn lookup_palette(palette: &[[u8; 3]; 64], idx: u16) -> [u8; 3] {
    if (idx as usize) < palette.len() {
        palette[idx as usize]
    } else {
        // 64 / 0x7FFF = 系统窗口前景色（默认黑）
        [0, 0, 0]
    }
}

#[derive(Clone, Debug, Default)]
struct FontRec {
    height_twips: u16,
    bold: bool,
    italic: bool,
    underline: bool,
    double_underline: bool,
    strikethrough: bool,
    color: [u8; 3],
    name: String,
}

#[derive(Clone, Debug, Default)]
struct XfRec {
    font: u16,
    format: u16,
    halign: u8,
    valign: u8,
    wrap: bool,
    shrink_to_fit: bool,
    indent: u8,
    /// 四条边的 BIFF 线型码，顺序：左、右、上、下
    borders: [u8; 4],
    /// 四条边的颜色（RGB），顺序同上
    border_colors: [[u8; 3]; 4],
    diagonal: Option<crate::style_ext::DiagBorder>,
    /// 单元格背景填充色
    fill: Option<[u8; 3]>,
    /// XF 保护位 bit0（true=锁定）
    locked: bool,
}

/// BIFF 线型 → 我们的 0 无 / 1 细 / 2 中 / 3 粗 / 4 双线 / 5 虚线 / 6 点线
fn map_border(code: u8) -> u8 {
    match code {
        0 => 0,
        1 => 1,
        2 => 2,
        5 => 3,
        6 => 4,
        3 => 5,
        8 => 10,
        9 => 8,
        10 => 11,
        11 => 9,
        12 | 13 => code,
        4 => 6,
        7 => 7,
        _ => 1,
    }
}

#[derive(Clone, Debug, Default)]
pub struct SheetStyles {
    /// (行, 列, 样式)
    pub cells: Vec<(u32, u32, CellStyle)>,
    /// 整行样式 (行, 样式)
    pub rows: Vec<(u32, CellStyle)>,
    /// 整列样式 (列, 样式)
    pub cols: Vec<(u32, CellStyle)>,
}

#[derive(Clone, Debug, Default)]
pub struct XlsStyles {
    pub sheets: Vec<SheetStyles>,
}

/// 解析 `Workbook` 流（公开出来便于用内存字节做测试）
pub fn parse(stream: &[u8]) -> XlsStyles {
    // 第一趟先读 BOUNDSHEET 与自定义 PALETTE(0x0092)，确保后续解析 FONT/XF 时调色板已就绪
    let mut bounds: Vec<u32> = Vec::new();
    let mut palette = DEFAULT_PALETTE;
    let mut i = 0usize;
    while i + 4 <= stream.len() {
        let (typ, len) = (u16le(stream, i).unwrap(), u16le(stream, i + 2).unwrap() as usize);
        let body = &stream[(i + 4).min(stream.len())..(i + 4 + len).min(stream.len())];
        if typ == BOUNDSHEET {
            bounds.push(u32le(body, 0).unwrap_or(0));
            if std::env::var("SC_XLS_DEBUG").is_ok() && body.len() >= 8 {
                let cch = body[6] as usize;
                let flags = body[7];
                let raw = &body[8..(8 + if flags & 1 == 0 { cch } else { cch * 2 }).min(body.len())];
                let name = if flags & 1 == 0 {
                    String::from_utf8_lossy(raw).to_string()
                } else {
                    let u: Vec<u16> = raw
                        .chunks(2)
                        .map(|c| u16::from_le_bytes([c[0], *c.get(1).unwrap_or(&0)]))
                        .collect();
                    String::from_utf16_lossy(&u)
                };
                eprintln!("[sheet] off={} {:?}", u32le(body, 0).unwrap_or(0), name);
            }
        } else if typ == PALETTE_REC && body.len() >= 2 {
            let ccv = u16le(body, 0).unwrap_or(0) as usize;
            for k in 0..ccv {
                let off = 2 + k * 4;
                if off + 3 <= body.len() && 8 + k < palette.len() {
                    palette[8 + k] = [body[off], body[off + 1], body[off + 2]];
                }
            }
        }
        let next = (i + 4 + len).min(stream.len());
        if next <= i {
            break;
        }
        i = next;
    }
    bounds.sort_unstable();

    // 第二趟读全局记录：FONT / FORMAT / XF
    let mut fonts: Vec<FontRec> = Vec::new();
    let mut formats: HashMap<u16, String> = HashMap::new();
    let mut xfs: Vec<XfRec> = Vec::new();
    let mut i = 0usize;
    while i + 4 <= stream.len() {
        let (typ, len) = (u16le(stream, i).unwrap(), u16le(stream, i + 2).unwrap() as usize);
        let body = &stream[(i + 4).min(stream.len())..(i + 4 + len).min(stream.len())];
        match typ {
            FONT => {
                if let Some(f) = parse_font(body, &palette) {
                    fonts.push(f);
                }
            }
            FORMAT => {
                if body.len() >= 5 {
                    let id = u16le(body, 0).unwrap();
                    let cch = u16le(body, 2).unwrap() as usize;
                    let flags = body[4];
                    let raw = &body[5..(5 + if flags & 1 == 0 { cch } else { cch * 2 }).min(body.len())];
                    let s = if flags & 1 == 0 {
                        String::from_utf8_lossy(raw).to_string()
                    } else {
                        let units: Vec<u16> = raw
                            .chunks(2)
                            .map(|c| u16::from_le_bytes([c[0], *c.get(1).unwrap_or(&0)]))
                            .collect();
                        String::from_utf16_lossy(&units)
                    };
                    formats.insert(id, s);
                }
            }
            XF => {
                if let Some(x) = parse_xf(body, &palette) {
                    xfs.push(x);
                }
            }
            _ => {}
        }
        let next = (i + 4 + len).min(stream.len());
        if next <= i {
            break;
        }
        i = next;
    }

    // BIFF8 规范（MS-XLS 2.4.122）：工作簿流中前 4 条 FONT 对应索引 0..=3，
    // 索引 4 被保留跳过，第 5 条起对应索引 5..N。若合成流少于 4 条 FONT 且存在 1-based 引用，兼容插在槽 0。
    let mut implicit = None;
    if fonts.len() >= 4 {
        let base = fonts[0].clone();
        implicit = Some((base.name.clone(), base.height_twips));
        fonts.insert(4, base);
    } else if !fonts.is_empty() {
        let mut def = fonts[0].clone();
        def.bold = false;
        def.italic = false;
        let mid = fonts
            .iter()
            .position(|f| (f.height_twips as f32 - 220.0).abs() < 1.0 && !f.bold && !f.italic)
            .unwrap_or(0);
        let base = fonts[mid].clone();
        def.height_twips = base.height_twips;
        def.name = base.name.clone();
        implicit = Some((def.name.clone(), def.height_twips));
        fonts.insert(0, def);
    }

    let build_cell_style = |ixfe: u16| -> Option<CellStyle> {
        let xf = xfs.get(ixfe as usize)?;
        let f = fonts.get(xf.font as usize).cloned().unwrap_or_default();
        let mut st = CellStyle {
            bold: f.bold,
            italic: f.italic,
            underline: f.underline,
            double_underline: f.double_underline,
            strikethrough: f.strikethrough,
            color: f.color,
            fill: xf.fill,
            font: crate::style::normalize_font(&f.name),
            size: if f.height_twips > 0 {
                (f.height_twips as f32 / 20.0 * 4.0).round() / 4.0
            } else {
                11.0
            },
            halign: xf.halign,
            valign: xf.valign,
            wrap: xf.wrap,
            shrink_to_fit: xf.shrink_to_fit,
            indent: xf.indent,
            locked: xf.locked,
            ..Default::default()
        };
        st.diagonal = xf.diagonal.clone();
        for (idx, code) in xf.borders.iter().enumerate() {
            st.border[idx].style = *code;
            if *code != 0 {
                st.border[idx].color = xf.border_colors[idx];
            }
        }
        st.format = formats
            .get(&xf.format)
            .cloned()
            .or_else(|| builtin_format(xf.format).map(|s| s.to_string()))
            .unwrap_or_default();
        if st == CellStyle::default() {
            return None;
        }
        if let Some((name, tw)) = &implicit {
            let norm_name = crate::style::normalize_font(name);
            let same = st.font == norm_name
                && (st.size - (*tw as f32 / 20.0)).abs() < 0.01
                && !st.bold
                && !st.italic
                && !st.underline
                && !st.double_underline
                && !st.strikethrough
                && st.color == [0, 0, 0]
                && st.halign == 0
                && st.valign == 0
                && !st.wrap
                && !st.shrink_to_fit
                && st.indent == 0
                && st.format.is_empty()
                && st.locked
                && st.fill.is_none()
                && st.diagonal.is_none()
                && st.border.iter().all(|b| b.style == 0);
            if same {
                return None;
            }
        }
        Some(st)
    };

    let mut out = XlsStyles {
        sheets: vec![SheetStyles::default(); bounds.len()],
    };

    // 逐表扫 COLINFO / ROW / 单元格记录，解析 XF 索引
    let mut cur: Option<usize> = None;
    let mut next_bound = 0usize;
    let mut i = 0usize;
    while i + 4 <= stream.len() {
        while next_bound < bounds.len() && (i as u32) >= bounds[next_bound] {
            cur = Some(next_bound);
            next_bound += 1;
        }
        let (typ, len) = (u16le(stream, i).unwrap(), u16le(stream, i + 2).unwrap() as usize);
        let body = &stream[(i + 4).min(stream.len())..(i + 4 + len).min(stream.len())];
        if let Some(si) = cur {
            let mut push = |r: u32, c: u32, ixfe: u16| {
                if let Some(st) = build_cell_style(ixfe) {
                    out.sheets[si].cells.push((r, c, st));
                }
            };
            match typ {
                COLINFO if body.len() >= 8 => {
                    let ixfe = u16le(body, 6).unwrap_or(15);
                    if let Some((c1, c2)) = crate::xls_layout::physical_columns(body).filter(|_| ixfe != 0 && ixfe != 15) {
                        if let Some(st) = build_cell_style(ixfe) {
                            for col in c1..=c2 {
                                out.sheets[si].cols.push((col, st.clone()));
                            }
                        }
                    }
                }
                ROW_REC if body.len() >= 16 => {
                    let r = u16le(body, 0).unwrap_or(0) as u32;
                    let grbit = u16le(body, 12).unwrap_or(0);
                    if grbit & 0x0080 != 0 {
                        let ixfe = u16le(body, 14).unwrap_or(15) & 0x0FFF;
                        if let Some(st) = build_cell_style(ixfe) {
                            out.sheets[si].rows.push((r, st));
                        }
                    }
                }
                LABELSST | LABEL | NUMBER | RK | BOOLERR | BLANK | FORMULA if body.len() >= 6 => {
                    let r = u16le(body, 0).unwrap() as u32;
                    let c = u16le(body, 2).unwrap() as u32;
                    let ixfe = u16le(body, 4).unwrap();
                    push(r, c, ixfe);
                }
                MULRK if body.len() >= 6 => {
                    let r = u16le(body, 0).unwrap() as u32;
                    let c1 = u16le(body, 2).unwrap() as u32;
                    let n = (body.len().saturating_sub(6)) / 6;
                    for k in 0..n {
                        let ixfe = u16le(body, 4 + k * 6).unwrap();
                        push(r, c1 + k as u32, ixfe);
                    }
                }
                MULBLANK if body.len() >= 6 => {
                    let r = u16le(body, 0).unwrap() as u32;
                    let c1 = u16le(body, 2).unwrap() as u32;
                    let n = (body.len().saturating_sub(6)) / 2;
                    for k in 0..n {
                        let ixfe = u16le(body, 4 + k * 2).unwrap();
                        push(r, c1 + k as u32, ixfe);
                    }
                }
                _ => {}
            }
        }
        let next = (i + 4 + len).min(stream.len());
        if next <= i {
            break;
        }
        i = next;
    }
    out
}

fn parse_font(b: &[u8], palette: &[[u8; 3]; 64]) -> Option<FontRec> {
    if b.len() < 16 {
        return None;
    }
    let height = u16le(b, 0)?;
    let grbit = u16le(b, 2)?;
    let icv = u16le(b, 4)?;
    let weight = u16le(b, 6)?;
    let uls = b[10];
    // BIFF8 FONT：... uls(10) family(11) bCharSet(12) reserved(13) cch(14) flags(15) name(16..)
    let cch = b[14] as usize;
    let flags = b[15];
    let raw = &b[(16).min(b.len())..(16 + if flags & 1 == 0 { cch } else { cch * 2 }).min(b.len())];
    let name = if flags & 1 == 0 {
        String::from_utf8_lossy(raw).to_string()
    } else {
        let units: Vec<u16> = raw
            .chunks(2)
            .map(|c| u16::from_le_bytes([c[0], *c.get(1).unwrap_or(&0)]))
            .collect();
        String::from_utf16_lossy(&units)
    };
    Some(FontRec {
        height_twips: height,
        bold: grbit & 1 != 0 || weight >= 700,
        italic: grbit & 2 != 0,
        strikethrough: grbit & 8 != 0,
        underline: uls != 0,
        double_underline: uls == 2 || uls == 0x22,
        color: lookup_palette(palette, icv),
        name,
    })
}

fn parse_xf(b: &[u8], palette: &[[u8; 3]; 64]) -> Option<XfRec> {
    if b.len() < 16 {
        return None;
    }
    let ifnt = u16le(b, 0)?;
    let ifmt = u16le(b, 2)?;
    let alc = b[6] & 0x07;
    // 0 常规 1 左 2 中 3 右 4 填充 5 两端 6 跨列居中 7 分散
    let halign = match alc {
        1 => 1,
        2 => 2,
        3 => 3,
        6 => 4,
        7 => 5,
        _ => 0,
    };
    let valign = if b[9] & 0x10 != 0 {
        match (b[6] >> 4) & 0x07 {
            0 => 1,
            1 => 2,
            2 => 3,
            _ => 0,
        }
    } else {
        0
    };
    let wrap = b[6] & 0x08 != 0;
    let indent = b[8] & 0x0F;
    let shrink_to_fit = b[8] & 0x10 != 0;

    // 线型与颜色：MS-XLS 2.4.353 标准字段 + 兼容本引擎写出布局
    let b_u32_10 = u32le(b, 10).unwrap_or(0);
    let b_u32_14 = if b.len() >= 18 { u32le(b, 14).unwrap_or(0) } else { 0 };
    // MS-XLS 标准位：bits 0..3=左, 4..7=右, 8..11=上, 12..15=下；
    let borders = [
        map_border((b_u32_10 & 0x0F) as u8),
        map_border(((b_u32_10 >> 4) & 0x0F) as u8),
        map_border(((b_u32_10 >> 8) & 0x0F) as u8),
        map_border(((b_u32_10 >> 12) & 0x0F) as u8),
    ];
    let icv_left = ((b_u32_10 >> 16) & 0x7F) as u16;
    let icv_right = ((b_u32_10 >> 23) & 0x7F) as u16;
    let icv_top = (b_u32_14 & 0x7F) as u16;
    let icv_bottom = ((b_u32_14 >> 7) & 0x7F) as u16;
    let border_colors = [
        lookup_palette(palette, icv_left),
        lookup_palette(palette, icv_right),
        lookup_palette(palette, icv_top),
        lookup_palette(palette, icv_bottom),
    ];

    // 填充底色：b[17] 高 6 位（即 u32@14 >> 26）为 fls 图案码；u16@18 低 7 位为 icvFore，7..13 位为 icvBack
    let fill = if b.len() >= 20 {
        let fls = b[17] >> 2;
        let fill_word = u16le(b, 18).unwrap_or(0);
        let icv_fore = fill_word & 0x007F;
        let icv_back = (fill_word >> 7) & 0x007F;
        if fls == 0 {
            None
        } else if fls == 1 {
            if (icv_fore as usize) < palette.len() {
                Some(palette[icv_fore as usize])
            } else if icv_fore == 64 {
                Some([0, 0, 0])
            } else {
                None
            }
        } else if (icv_fore as usize) < palette.len() {
            Some(palette[icv_fore as usize])
        } else if (icv_back as usize) < palette.len() {
            Some(palette[icv_back as usize])
        } else {
            None
        }
    } else {
        None
    };

    let locked = u16le(b, 4).unwrap_or(1) & 0x0001 != 0;
    Some(XfRec {
        font: ifnt,
        format: ifmt,
        halign,
        valign,
        wrap,
        shrink_to_fit,
        indent,
        borders,
        border_colors,
        diagonal: {
            let flags = b_u32_10 >> 30;
            let kind = ((b_u32_14 >> 21) & 15) as u8;
            (kind != 0 && flags != 0).then(|| crate::style_ext::DiagBorder {
                up: flags & 2 != 0,
                down: flags & 1 != 0,
                style: crate::style::Border::ooxml_name(map_border(kind)).into(),
                color: lookup_palette(palette, ((b_u32_14 >> 14) & 127) as u16),
                color_theme: None,
            })
        },
        fill,
        locked,
    })
}

/// 调试用：打印 FONT / XF 表（SC_XLS_DUMP=1 时由例题调用）
pub fn debug_tables(stream: &[u8]) {
    let palette = DEFAULT_PALETTE;
    let mut i = 0usize;
    let mut fi = 0usize;
    let mut xi = 0usize;
    while i + 4 <= stream.len() {
        let (typ, len) = (u16le(stream, i).unwrap(), u16le(stream, i + 2).unwrap() as usize);
        let body = &stream[(i + 4).min(stream.len())..(i + 4 + len).min(stream.len())];
        if typ == FONT {
            match parse_font(body, &palette) {
                Some(f) => {
                    eprintln!(
                        "FONT[{fi}] len={} {:?} {}pt bold={} color={:?}",
                        body.len(),
                        f.name,
                        f.height_twips as f32 / 20.0,
                        f.bold,
                        f.color
                    );
                    fi += 1;
                }
                None => eprintln!("FONT[?] len={} 解析失败 {:02x?}", body.len(), &body[..body.len().min(16)]),
            }
        } else if typ == XF {
            if let Some(x) = parse_xf(body, &palette) {
                eprintln!(
                    "XF[{xi}] font={} fmt={} align={} wrap={} fill={:?}",
                    x.font, x.format, x.halign, x.wrap, x.fill
                );
                xi += 1;
            }
        }
        let next = (i + 4 + len).min(stream.len());
        if next <= i {
            break;
        }
        i = next;
    }
}

pub fn read(path: &Path) -> Option<XlsStyles> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.starts_with(b"PK") || bytes.len() < 512 {
        return None;
    }
    let c = crate::cfb::Compound::open(bytes).ok()?;
    let stream = c
        .stream_by_path("Workbook")
        .or_else(|| c.stream_by_path("Book"))?;
    Some(parse(&stream))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(typ: u16, body: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&typ.to_le_bytes());
        v.extend_from_slice(&(body.len() as u16).to_le_bytes());
        v.extend_from_slice(body);
        v
    }

    fn font_with_color(height: u16, grbit: u16, icv: u16, weight: u16, name: &str) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&height.to_le_bytes());
        b.extend_from_slice(&grbit.to_le_bytes());
        b.extend_from_slice(&icv.to_le_bytes());
        b.extend_from_slice(&weight.to_le_bytes());
        b.extend_from_slice(&[0u8; 6]); // sss uls family charset reserved
        b.push(name.encode_utf16().count() as u8);
        b.push(1); // UTF-16
        for u in name.encode_utf16() {
            b.extend_from_slice(&u.to_le_bytes());
        }
        rec(FONT, &b)
    }

    fn font(height: u16, grbit: u16, weight: u16, name: &str) -> Vec<u8> {
        font_with_color(height, grbit, 0x7FFF, weight, name)
    }

    fn boundsheet(offset: u32, name: &str) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&offset.to_le_bytes());
        b.extend_from_slice(&0u16.to_le_bytes());
        b.push(name.len() as u8);
        b.push(0); // 8-bit 名字
        b.extend_from_slice(name.as_bytes());
        rec(BOUNDSHEET, &b)
    }

    #[test]
    fn reads_font_xf_and_cell_style() {
        let mut s = Vec::new();
        s.extend(rec(0x0809, &[0u8; 4]));
        s.extend(font(440, 1, 700, "宋体")); // 22pt 粗体
        s.extend(font(220, 0, 400, "宋体")); // 11pt 常规
        let mut f = Vec::new();
        f.extend_from_slice(&176u16.to_le_bytes());
        f.extend_from_slice(&("yyyy-mm-dd".encode_utf16().count() as u16).to_le_bytes());
        f.push(1);
        for u in "yyyy-mm-dd".encode_utf16() {
            f.extend_from_slice(&u.to_le_bytes());
        }
        s.extend(rec(0x041E, &f));
        s.extend(rec(0x00E0, &[0u8; 20]));
        let mut xf = vec![0u8; 20];
        xf[0] = 1;
        xf[2] = 176;
        xf[6] = 2;
        s.extend(rec(0x00E0, &xf));
        let mut bs = boundsheet(0, "A1");
        let off = (s.len() + bs.len()) as u32;
        bs[4..8].copy_from_slice(&off.to_le_bytes());
        s.extend(bs);
        assert_eq!(s.len(), off as usize);
        s.extend(rec(0x000A, &[]));
        s.extend(rec(0x0809, &[0u8; 4]));
        let mut c = Vec::new();
        c.extend_from_slice(&0u16.to_le_bytes());
        c.extend_from_slice(&0u16.to_le_bytes());
        c.extend_from_slice(&1u16.to_le_bytes());
        c.extend_from_slice(&0u16.to_le_bytes());
        let mut ci = [0u8; 12];
        ci[..2].copy_from_slice(&254u16.to_le_bytes());
        ci[2..4].copy_from_slice(&256u16.to_le_bytes());
        ci[4..6].copy_from_slice(&(16u16 * 256).to_le_bytes());
        ci[6..8].copy_from_slice(&1u16.to_le_bytes());
        s.extend(rec(COLINFO, &ci));
        ci[..2].copy_from_slice(&256u16.to_le_bytes());
        s.extend(rec(COLINFO, &ci));
        s.extend(rec(0x00FD, &c));
        s.extend(rec(0x000A, &[]));

        let out = parse(&s);
        assert_eq!(out.sheets.len(), 1);
        assert_eq!(out.sheets[0].cells.len(), 1, "应解析出一个带样式的格子");
        let cols = &out.sheets[0].cols;
        assert_eq!(cols.iter().map(|(c, _)| *c).collect::<Vec<_>>(), [254, 255]);
        let layout = crate::xls_layout::parse(&s);
        assert_eq!(layout.sheets[0].cols.iter().map(|&(a,b,_)| (a,b)).collect::<Vec<_>>(), [(254,255)]);
        let (r, c, st) = &out.sheets[0].cells[0];
        assert_eq!((*r, *c), (0, 0));
        assert!(st.bold, "22pt 的那个字体是粗体");
        assert!((st.size - 22.0).abs() < 0.1, "字号 {}", st.size);
        assert_eq!(st.font, "宋体");
        assert_eq!(st.format, "yyyy-mm-dd");
        assert_eq!(st.halign, 2);
        assert_eq!(st.border[0].style, 0, "测试里的 XF 没设边框");
    }

    #[test]
    fn reads_biff8_font_color_fill_color_and_custom_palette() {
        let mut s = Vec::new();
        s.extend(rec(0x0809, &[0u8; 4]));
        // 4 条默认 FONT（0..=3），外加第 5 条自定义红色字体（BIFF8 索引 5）
        for _ in 0..4 {
            s.extend(font(220, 0, 400, "宋体"));
        }
        s.extend(font_with_color(220, 0, 8, 400, "宋体")); // icv=8（自定义调色板首项）
        // 自定义 PALETTE(0x0092)：把槽 8 设为珊瑚橙 [240, 90, 40]，槽 9 设为浅绿底 [210, 245, 220]
        let mut pal = Vec::new();
        pal.extend_from_slice(&2u16.to_le_bytes());
        pal.extend_from_slice(&[240, 90, 40, 0]);
        pal.extend_from_slice(&[210, 245, 220, 0]);
        s.extend(rec(PALETTE_REC, &pal));

        s.extend(rec(0x00E0, &[0u8; 20])); // XF 0
        let mut xf = vec![0u8; 20];
        xf[0] = 5; // 指向 BIFF8 字体索引 5
        xf[17] = 1 << 2; // fls = 1 (solid)
        xf[18] = 9; // icvFore = 9
        s.extend(rec(0x00E0, &xf)); // XF 1

        let mut bs = boundsheet(0, "Sheet1");
        let off = (s.len() + bs.len()) as u32;
        bs[4..8].copy_from_slice(&off.to_le_bytes());
        s.extend(bs);
        s.extend(rec(0x000A, &[]));

        s.extend(rec(0x0809, &[0u8; 4]));
        let mut cell = Vec::new();
        cell.extend_from_slice(&2u16.to_le_bytes()); // row 2
        cell.extend_from_slice(&3u16.to_le_bytes()); // col 3
        cell.extend_from_slice(&1u16.to_le_bytes()); // ixfe 1
        s.extend(rec(BLANK, &cell));
        s.extend(rec(0x000A, &[]));

        let out = parse(&s);
        assert_eq!(out.sheets[0].cells.len(), 1);
        let (r, c, st) = &out.sheets[0].cells[0];
        assert_eq!((*r, *c), (2, 3));
        assert_eq!(st.color, [240, 90, 40]);
        assert_eq!(st.fill, Some([210, 245, 220]));
    }
}
