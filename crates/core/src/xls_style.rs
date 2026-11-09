//! 老式 .xls（BIFF8）的单元格样式。
//!
//! calamine 只给值，不给样式。这里解析 FONT / FORMAT / XF 三类记录，
//! 再把每个单元格记录里的 XF 索引接上，得到字体名、字号、加粗/斜体、
//! 水平对齐、自动换行和数字格式。
//!
//! 只做「看得见」的部分：颜色和边框暂不解析（边框在渲染层单独做）。

use crate::style::CellStyle;
use std::collections::HashMap;
use std::path::Path;

const BOUNDSHEET: u16 = 0x0085;
const FONT: u16 = 0x0031;
const FORMAT: u16 = 0x041E;
const XF: u16 = 0x00E0;

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

#[derive(Clone, Debug, Default)]
struct FontRec {
    height_twips: u16,
    bold: bool,
    italic: bool,
    name: String,
}

#[derive(Clone, Debug, Default)]
struct XfRec {
    font: u16,
    format: u16,
    halign: u8,
    wrap: bool,
    /// 四条边的 BIFF 线型码，顺序：左、右、上、下
    borders: [u8; 4],
    /// 四条边的颜色（RGB），顺序同上
    border_colors: [[u8; 3]; 4],
}

/// 调色板索引 → RGB（BIFF8 默认 56 色；64=自动，其他越界按黑）
fn palette_rgb(idx: u8) -> [u8; 3] {
    const P: [[u8; 3]; 56] = [
        [0, 0, 0], [255, 255, 255], [255, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 0],
        [255, 0, 255], [0, 255, 255], [0, 0, 0], [255, 255, 255], [255, 0, 0], [0, 255, 0],
        [0, 0, 255], [255, 255, 0], [255, 0, 255], [0, 255, 255], [128, 0, 0], [0, 128, 0],
        [0, 0, 128], [128, 128, 0], [128, 0, 128], [0, 128, 128], [192, 192, 192], [128, 128, 128],
        [153, 153, 255], [153, 51, 102], [255, 255, 204], [204, 255, 255], [102, 0, 102], [255, 128, 128],
        [0, 102, 204], [204, 204, 255], [0, 0, 128], [255, 0, 255], [255, 255, 0], [0, 255, 255],
        [128, 0, 128], [128, 0, 0], [0, 128, 128], [0, 0, 255], [0, 204, 255], [204, 255, 255],
        [204, 255, 204], [255, 255, 153], [153, 204, 255], [255, 153, 204], [204, 153, 255], [255, 204, 153],
        [51, 102, 255], [51, 204, 204], [153, 204, 0], [255, 204, 0], [255, 153, 0], [255, 102, 0],
        [102, 102, 153], [150, 150, 150],
    ];
    if (idx as usize) < P.len() {
        P[idx as usize]
    } else {
        [0, 0, 0]
    }
}

/// BIFF 线型 → 我们的 0 无 / 1 细 / 2 中 / 3 粗 / 4 双线 / 5 虚线 / 6 点线
fn map_border(code: u8) -> u8 {
    match code {
        0 => 0,
        1 => 1,
        2 => 2,
        5 => 3,
        6 => 4,
        3 | 8 | 9 | 10 | 11 | 12 | 13 => 5,
        4 => 6,
        7 => 1,
        _ => 1,
    }
}

#[derive(Clone, Debug, Default)]
pub struct SheetStyles {
    /// (行, 列, 样式)
    pub cells: Vec<(u32, u32, CellStyle)>,
}

#[derive(Clone, Debug, Default)]
pub struct XlsStyles {
    pub sheets: Vec<SheetStyles>,
}

/// 解析 `Workbook` 流（公开出来便于用内存字节做测试）
pub fn parse(stream: &[u8]) -> XlsStyles {
    // 表顺序与 BOF 偏移
    let mut bounds: Vec<u32> = Vec::new();
    let mut i = 0usize;
    while i + 4 <= stream.len() {
        let (typ, len) = (u16le(stream, i).unwrap(), u16le(stream, i + 2).unwrap() as usize);
        let body = &stream[(i + 4).min(stream.len())..(i + 4 + len).min(stream.len())];
        if typ == BOUNDSHEET {
            bounds.push(u32le(body, 0).unwrap_or(0));
            if std::env::var("SC_XLS_DEBUG").is_ok() {
                let cch = body[6] as usize;
                let flags = body[7];
                let raw = &body[8..(8 + if flags & 1 == 0 { cch } else { cch * 2 }).min(body.len())];
                let name = if flags & 1 == 0 {
                    String::from_utf8_lossy(raw).to_string()
                } else {
                    let u: Vec<u16> = raw.chunks(2).map(|c| u16::from_le_bytes([c[0], *c.get(1).unwrap_or(&0)])).collect();
                    String::from_utf16_lossy(&u)
                };
                eprintln!("[sheet] off={} {:?}", u32le(body, 0).unwrap_or(0), name);
            }
        }
        let next = (i + 4 + len).min(stream.len());
        if next <= i {
            break;
        }
        i = next;
    }
    bounds.sort_unstable();

    // 全局记录：FONT / FORMAT / XF
    let mut fonts: Vec<FontRec> = Vec::new();
    let mut formats: HashMap<u16, String> = HashMap::new();
    let mut xfs: Vec<XfRec> = Vec::new();
    let mut i = 0usize;
    while i + 4 <= stream.len() {
        let (typ, len) = (u16le(stream, i).unwrap(), u16le(stream, i + 2).unwrap() as usize);
        let body = &stream[(i + 4).min(stream.len())..(i + 4 + len).min(stream.len())];
        match typ {
            FONT => {
                if let Some(f) = parse_font(body) {
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
                if let Some(x) = parse_xf(body) {
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

    // 有些写入器（Excel 另存 .xls 也一样）的 XF.ifnt 是 1-based：
    // 表里第一条 FONT 对应索引 1，索引 0 是"没写出来的默认字体"。
    // 判据：有 XF 引用了恰好 == 字体数 的索引（越界一项）。
    let max_ref = xfs.iter().map(|x| x.font as usize).max().unwrap_or(0);
    // Excel 写 .xls 时 FONT 记录对应索引 1..N，索引 0 是工作表默认字体（不写出来）。
    // 两个实测样本（Excel 365 另存、一张老发票）都是这个约定；只有当位移后
    // 出现越界引用（说明本来就是 0-based）才不位移。
    // 实测三个 Excel 写出的样本（新建另存 / 边框测试 / 老发票）都符合：
    // FONT 记录对应索引 1..N，索引 0 是工作表的默认字体，不单独写记录。
    let _ = max_ref;
    let mut implicit = None;
    if !fonts.is_empty() {
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

    let mut out = XlsStyles {
        sheets: vec![SheetStyles::default(); bounds.len()],
    };

    // 逐表扫单元格记录，拿 XF 索引
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
                if ixfe as usize >= xfs.len() {
                    return;
                }
                let xf = &xfs[ixfe as usize];
                let f = fonts.get(xf.font as usize).cloned().unwrap_or_default();
                let mut st = CellStyle {
                    bold: f.bold,
                    italic: f.italic,
                    font: f.name.clone(),
                    size: if f.height_twips > 0 {
                        (f.height_twips as f32 / 20.0 * 4.0).round() / 4.0
                    } else {
                        11.0
                    },
                    halign: xf.halign,
                    wrap: xf.wrap,
                    ..Default::default()
                };
                for (i, code) in xf.borders.iter().enumerate() {
                    st.border[i].style = *code;
                    if *code != 0 {
                        st.border[i].color = xf.border_colors[i];
                    }
                }
                st.format = formats
                    .get(&xf.format)
                    .cloned()
                    .or_else(|| builtin_format(xf.format).map(|s| s.to_string()))
                    .unwrap_or_default();
                if st == CellStyle::default() {
                    return;
                }
                // 跟"工作表默认字体"完全一样的格子不占样式槽，省内存
                if let Some((name, tw)) = &implicit {
                    let same = &st.font == name
                        && (st.size - (*tw as f32 / 20.0)).abs() < 0.01
                        && !st.bold
                        && !st.italic
                        && st.halign == 0
                        && st.valign == 0
                        && !st.wrap
                        && st.format.is_empty()
                        && st.fill.is_none()
                        && st.border.iter().all(|b| b.style == 0);
                    if same {
                        return;
                    }
                }
                out.sheets[si].cells.push((r, c, st));
            };
            match typ {
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

fn parse_font(b: &[u8]) -> Option<FontRec> {
    if b.len() < 16 {
        return None;
    }
    let height = u16le(b, 0)?;
    let grbit = u16le(b, 2)?;
    let weight = u16le(b, 6)?;
    // BIFF8 FONT：... bCharSet(12) reserved(13) cch(14) flags(15) name(16..)
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
        name,
    })
}

fn parse_xf(b: &[u8]) -> Option<XfRec> {
    if b.len() < 8 {
        return None;
    }
    let ifnt = u16le(b, 0)?;
    let ifmt = u16le(b, 2)?;
    let alc = b[6] & 0x07;
    // 0 常规 1 左 2 中 3 右 4 填充 5 两端 6 跨列居中 7 分散
    let halign = match alc {
        1 => 1,
        2 | 6 => 2,
        3 => 3,
        _ => 0,
    };
    // 自动换行是 alc 那个字节的 bit3
    let wrap = b[6] & 0x08 != 0;
    // 线型：byte10 低四位=左、高四位=下；byte11 低四位=右、高四位=上
    let borders = [
        map_border(b[10] & 0x0F),
        map_border(b[11] & 0x0F),
        map_border(b[11] >> 4),
        map_border(b[10] >> 4),
    ];
    // 颜色：byte12=左、13=下、14=上、15=右；低 7 位是调色板索引，
    // 高位 0x80 表示"自动"（按黑处理）
    let color = |v: u8| if v & 0x80 != 0 { 64 } else { v };
    let border_colors = [
        palette_rgb(color(b[12])),
        palette_rgb(color(b[15])),
        palette_rgb(color(b[14])),
        palette_rgb(color(b[13])),
    ];
    Some(XfRec {
        font: ifnt,
        format: ifmt,
        halign,
        wrap,
        borders,
        border_colors,
    })
}

/// 调试用：打印 FONT / XF 表（SC_XLS_DUMP=1 时由例题调用）
pub fn debug_tables(stream: &[u8]) {
    let mut i = 0usize;
    let mut fi = 0usize;
    let mut xi = 0usize;
    while i + 4 <= stream.len() {
        let (typ, len) = (u16le(stream, i).unwrap(), u16le(stream, i + 2).unwrap() as usize);
        let body = &stream[(i + 4).min(stream.len())..(i + 4 + len).min(stream.len())];
        if typ == FONT {
            match parse_font(body) {
                Some(f) => {
                    eprintln!("FONT[{fi}] len={} {:?} {}pt bold={}", body.len(), f.name, f.height_twips as f32 / 20.0, f.bold);
                    fi += 1;
                }
                None => eprintln!("FONT[?] len={} 解析失败 {:02x?}", body.len(), &body[..body.len().min(16)]),
            }
        } else if typ == XF {
            if let Some(x) = parse_xf(body) {
                eprintln!("XF[{xi}] font={} fmt={} align={} wrap={}", x.font, x.format, x.halign, x.wrap);
                xi += 1;
            }
        }
        let next = (i + 4 + len).min(stream.len());
        if next <= i { break }
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

    fn font(height: u16, grbit: u16, weight: u16, name: &str) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&height.to_le_bytes());
        b.extend_from_slice(&grbit.to_le_bytes());
        b.extend_from_slice(&0u16.to_le_bytes()); // icv
        b.extend_from_slice(&weight.to_le_bytes());
        b.extend_from_slice(&[0u8; 6]); // sss uls family charset reserved
        b.push(name.encode_utf16().count() as u8);
        b.push(1); // UTF-16
        for u in name.encode_utf16() {
            b.extend_from_slice(&u.to_le_bytes());
        }
        rec(FONT, &b)
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
        // 自定义格式 176
        let mut f = Vec::new();
        f.extend_from_slice(&176u16.to_le_bytes());
        f.extend_from_slice(&("yyyy-mm-dd".encode_utf16().count() as u16).to_le_bytes());
        f.push(1);
        for u in "yyyy-mm-dd".encode_utf16() {
            f.extend_from_slice(&u.to_le_bytes());
        }
        s.extend(rec(0x041E, &f));
        // XF 0 默认；XF 1 = 字体 0 + 格式 176 + 居中
        s.extend(rec(0x00E0, &[0u8; 20]));
        let mut xf = vec![0u8; 20];
        // XF 1 指向字体 1（1-based）
        xf[0] = 1; // 1-based：字体 1 = 22pt 粗体
        xf[2] = 176;
        xf[6] = 2;
        s.extend(rec(0x00E0, &xf));
        // BOUNDSHEET 放在末尾也行：解析只认 offset，不校验顺序
        let mut bs = boundsheet(0, "A1");
        let off = (s.len() + bs.len()) as u32;
        bs[4..8].copy_from_slice(&off.to_le_bytes());
        s.extend(bs);
        assert_eq!(s.len(), off as usize);
        s.extend(rec(0x000A, &[]));
        // 表子流
        s.extend(rec(0x0809, &[0u8; 4]));
        let mut c = Vec::new();
        c.extend_from_slice(&0u16.to_le_bytes());
        c.extend_from_slice(&0u16.to_le_bytes());
        c.extend_from_slice(&1u16.to_le_bytes());
        c.extend_from_slice(&0u16.to_le_bytes());
        s.extend(rec(0x00FD, &c));
        s.extend(rec(0x000A, &[]));

        let out = parse(&s);
        assert_eq!(out.sheets.len(), 1);
        assert_eq!(out.sheets[0].cells.len(), 1, "应解析出一个带样式的格子");
        let (r, c, st) = &out.sheets[0].cells[0];
        assert_eq!((*r, *c), (0, 0));
        assert!(st.bold, "22pt 的那个字体是粗体");
        assert!((st.size - 22.0).abs() < 0.1, "字号 {}", st.size);
        assert_eq!(st.font, "宋体");
        assert_eq!(st.format, "yyyy-mm-dd");
        assert_eq!(st.halign, 2);
        assert_eq!(st.border[0].style, 0, "测试里的 XF 没设边框");
    }
}
