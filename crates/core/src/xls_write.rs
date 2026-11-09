//! 写老式 .xls（BIFF8）。
//!
//! 只写「值和格式」：数字、文本、布尔、字体/字号/加粗/颜色、数字格式、对齐、
//! 换行、四边边框、底色、合并区、列宽、行高。
//!
//! **公式不写**：BIFF 的公式是 RPN 记号流，把 `=A1+B1` 原样塞进文件会让 Excel
//! 认为文件损坏。带公式的工作簿直接报错，让调用方引导用户存 xlsx。
//!
//! 字符串用 LABEL 记录内联（不用 SST），省掉共享字符串表的分段逻辑；
//! Excel 一直支持 LABEL。

use crate::model::{Sheet, Value, Workbook};
use crate::style::CellStyle;
use anyhow::{bail, Context, Result};
use std::collections::HashMap;
use std::path::Path;

const BOF: u16 = 0x0809;
const EOF: u16 = 0x000A;
const CODEPAGE: u16 = 0x0042;
const FONT: u16 = 0x0031;
const FORMAT: u16 = 0x041E;
const XF: u16 = 0x00E0;
const BOUNDSHEET: u16 = 0x0085;
const DIMENSIONS: u16 = 0x0200;
const COLINFO: u16 = 0x007D;
const ROW: u16 = 0x0208;
const LABEL: u16 = 0x0204;
const NUMBER: u16 = 0x0203;
const BOOLERR: u16 = 0x0205;
const MERGEDCELLS: u16 = 0x00E5;
const WINDOW1: u16 = 0x003D;
const STYLE: u16 = 0x0293;

/// 合法骨架：Excel 生成的空工作簿里的 全局流前缀 / 字体+XF 块 / STYLE 记录
const TPL_PREFIX: &[u8] = include_bytes!("xls_tpl/prefix.bin");
const TPL_FONTS_XFS: &[u8] = include_bytes!("xls_tpl/fonts_xfs.bin");
const TPL_STYLE: &[u8] = include_bytes!("xls_tpl/style.bin");

/// 骨架里已有的字体数 / XF 数：我们的表和单元格引用都从这之后开始编号
fn tpl_counts() -> (u16, u16) {
    let mut fonts = 0u16;
    let mut xfs = 0u16;
    let mut i = 0usize;
    while i + 4 <= TPL_FONTS_XFS.len() {
        let t = u16::from_le_bytes([TPL_FONTS_XFS[i], TPL_FONTS_XFS[i + 1]]);
        let l = u16::from_le_bytes([TPL_FONTS_XFS[i + 2], TPL_FONTS_XFS[i + 3]]) as usize;
        match t {
            0x0031 => fonts += 1,
            0x00E0 => xfs += 1,
            _ => {}
        }
        let n = (i + 4 + l).min(TPL_FONTS_XFS.len());
        if n <= i {
            break;
        }
        i = n;
    }
    (fonts, xfs)
}

/// Excel 默认调色板（BIFF8 索引 0..55）
const PALETTE: [[u8; 3]; 56] = [
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

fn near_index(rgb: [u8; 3]) -> u8 {
    let mut best = 0usize;
    let mut bd = i32::MAX;
    for (i, p) in PALETTE.iter().enumerate() {
        let d = (p[0] as i32 - rgb[0] as i32).pow(2)
            + (p[1] as i32 - rgb[1] as i32).pow(2)
            + (p[2] as i32 - rgb[2] as i32).pow(2);
        if d < bd {
            bd = d;
            best = i;
        }
    }
    best as u8
}

fn rec(out: &mut Vec<u8>, typ: u16, body: &[u8]) {
    out.extend_from_slice(&typ.to_le_bytes());
    out.extend_from_slice(&(body.len() as u16).to_le_bytes());
    out.extend_from_slice(body);
}

fn w16(v: u16) -> Vec<u8> {
    v.to_le_bytes().to_vec()
}
fn w32(v: u32) -> Vec<u8> {
    v.to_le_bytes().to_vec()
}

/// XLUnicodeString（BIFF8）：cch(2) + flags(1) + UTF-16
fn unicode_str(s: &str) -> Vec<u8> {
    let units: Vec<u16> = s.encode_utf16().collect();
    let mut out = Vec::with_capacity(3 + units.len() * 2);
    out.extend_from_slice(&(units.len() as u16).to_le_bytes());
    out.push(1);
    for u in units {
        out.extend_from_slice(&u.to_le_bytes());
    }
    out
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct FontKey {
    name: String,
    size: u16,
    bold: bool,
    italic: bool,
    color: u8,
}

#[derive(Clone, PartialEq, Eq, Hash)]
struct XfKey {
    font: u16,
    format: u16,
    halign: u8,
    wrap: bool,
    borders: [u8; 4],
    fill: Option<u8>,
}

fn builtin_id(code: &str) -> Option<u16> {
    Some(match code {
        "" => 0,
        "0" => 1,
        "0.00" => 2,
        "#,##0" => 3,
        "#,##0.00" => 4,
        "0%" => 9,
        "0.00%" => 10,
        "0.00E+00" => 11,
        "yyyy-mm-dd" => 14,
        "d-mmm-yy" => 15,
        "h:mm" => 20,
        "h:mm:ss" => 21,
        "yyyy-mm-dd h:mm" => 22,
        "@" => 49,
        _ => return None,
    })
}

fn biff_border(style: u8) -> u8 {
    // 我们的 1 细 / 2 中 / 3 粗 / 4 双线 / 5 虚线 / 6 点线 → BIFF 码
    match style {
        1 => 1,
        2 => 2,
        3 => 5,
        4 => 6,
        5 => 3,
        6 => 4,
        _ => 0,
    }
}

struct Tables {
    fonts: Vec<FontKey>,
    font_ids: HashMap<FontKey, u16>,
    formats: Vec<(u16, String)>,
    xf_ids: HashMap<XfKey, u16>,
    xfs: Vec<XfKey>,
}

impl Tables {
    fn new() -> Self {
        Self {
            fonts: Vec::new(),
            font_ids: HashMap::new(),
            formats: Vec::new(),
            xf_ids: HashMap::new(),
            xfs: Vec::new(),
        }
    }

    fn font(&mut self, st: &CellStyle) -> u16 {
        let key = FontKey {
            name: if st.font.is_empty() { "宋体".into() } else { st.font.clone() },
            size: (st.size * 20.0).round().max(20.0) as u16,
            bold: st.bold,
            italic: st.italic,
            color: near_index(st.color),
        };
        if let Some(id) = self.font_ids.get(&key) {
            return *id;
        }
        // Excel 的字体索引是 1-based：第 p 条 FONT 记录用索引 p+1 引用
        let id = tpl_counts().0 + 1 + self.fonts.len() as u16;
        self.fonts.push(key.clone());
        self.font_ids.insert(key, id);
        id
    }

    fn format(&mut self, code: &str) -> u16 {
        if let Some(id) = builtin_id(code) {
            return id;
        }
        if let Some((id, _)) = self.formats.iter().find(|(_, c)| c == code) {
            return *id;
        }
        let id = 164 + self.formats.len() as u16;
        self.formats.push((id, code.to_string()));
        id
    }

    fn xf(&mut self, st: &CellStyle) -> u16 {
        let font = self.font(st);
        let format = self.format(&st.format);
        let key = XfKey {
            font,
            format,
            halign: st.halign,
            wrap: st.wrap,
            borders: [
                biff_border(st.border[0].style),
                biff_border(st.border[1].style),
                biff_border(st.border[2].style),
                biff_border(st.border[3].style),
            ],
            fill: st.fill.map(near_index),
        };
        if let Some(id) = self.xf_ids.get(&key) {
            return *id;
        }
        let id = tpl_counts().1 + self.xfs.len() as u16;
        self.xfs.push(key.clone());
        self.xf_ids.insert(key, id);
        id
    }
}

fn font_record(f: &FontKey) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&f.size.to_le_bytes());
    b.extend_from_slice(&(((f.bold as u16) | ((f.italic as u16) << 1)) as u16).to_le_bytes());
    b.extend_from_slice(&w16(0x7FFF)); // icv = 自动
    b.extend_from_slice(&w16(if f.bold { 700 } else { 400 }));
    b.extend_from_slice(&w16(0)); // sss
    b.push(0); // uls
    b.push(2); // bFamily
    b.push(0x86); // bCharSet = GB2312
    b.push(0); // reserved
    b.push(f.name.encode_utf16().count().min(255) as u8);
    b.push(1); // UTF-16
    for u in f.name.encode_utf16() {
        b.extend_from_slice(&u.to_le_bytes());
    }
    rec_body(FONT, b)
}

fn rec_body(typ: u16, body: Vec<u8>) -> Vec<u8> {
    let mut out = Vec::new();
    rec(&mut out, typ, &body);
    out
}

fn font_record_abs(f: &FontKey, base: u16) -> Vec<u8> {
    let mut out = font_record(f);
    // 字体记录本身不带索引，基址只影响引用方；这里保留函数以便以后加扩展
    let _ = base;
    out.shrink_to_fit();
    out
}

fn xf_record_abs(x: &XfKey, _base: u16) -> Vec<u8> {
    xf_record(x)
}

fn xf_record(x: &XfKey) -> Vec<u8> {
    let mut b = vec![0u8; 20];
    b[0..2].copy_from_slice(&x.font.to_le_bytes());
    b[2..4].copy_from_slice(&x.format.to_le_bytes());
    b[4..6].copy_from_slice(&w16(1)); // flags: locked
    b[6] = (x.halign & 0x07) | if x.wrap { 0x08 } else { 0 };
    b[9] = 0x20;
    // 线型：byte10 低四位=左、高四位=下；byte11 低四位=右、高四位=上
    b[10] = (x.borders[0] & 0x0F) | ((x.borders[3] & 0x0F) << 4);
    b[11] = (x.borders[1] & 0x0F) | ((x.borders[2] & 0x0F) << 4);
    if x.borders.iter().any(|c| *c != 0) {
        // 颜色：跟 Excel 一样用自动色（64）
        b[12] = 0x40;
        b[13] = 0x40;
        b[14] = 0x40;
        b[15] = 0x40;
    }
    match x.fill {
        Some(idx) => {
            b[16] = 0x01; // 实心填充
            b[17] = 0x00;
            b[18] = 0x40;
            b[19] = idx;
        }
        None => {
            b[16] = 0x00;
            b[17] = 0x00;
            b[18] = 0xC0;
            b[19] = 0x20;
        }
    }
    rec_body(XF, b)
}

fn sheet_stream(
    sheet: &Sheet,
    style_table: &crate::style::StyleTable,
    tables: &mut Tables,
    styles_cache: &mut HashMap<u16, u16>,
) -> Vec<u8> {
    let mut out = Vec::new();
    let mut bof = Vec::new();
    bof.extend_from_slice(&w16(0x0600));
    bof.extend_from_slice(&w16(0x0010)); // 工作表
    bof.extend_from_slice(&w16(0x0DBB));
    bof.extend_from_slice(&w16(0x07CC));
    bof.extend_from_slice(&w32(0));
    bof.extend_from_slice(&w32(0));
    rec(&mut out, BOF, &bof);

    // 范围
    let (mut rmax, mut cmax) = (0u32, 0u32);
    for (cref, _) in sheet.iter() {
        rmax = rmax.max(cref.row);
        cmax = cmax.max(cref.col);
    }
    let mut dim = Vec::new();
    dim.extend_from_slice(&w32(0));
    dim.extend_from_slice(&w32(rmax + 1));
    dim.extend_from_slice(&w16(0));
    dim.extend_from_slice(&w16((cmax + 1).min(255) as u16));
    dim.extend_from_slice(&w16(0));
    rec(&mut out, DIMENSIONS, &dim);

    // 列宽
    for (c, w) in sheet.cols.custom_pairs() {
        let mut b = Vec::new();
        b.extend_from_slice(&w16(c as u16));
        b.extend_from_slice(&w16(c as u16));
        // 像素 → 字符数（excel_width_to_px 的逆运算：(w-5)/7）
        let chars = ((w - 5.0) / 7.0).max(0.0);
        b.extend_from_slice(&w16((chars * 256.0).round().min(65535.0) as u16));
        b.extend_from_slice(&w16(0x0F));
        b.extend_from_slice(&w16(0));
        b.extend_from_slice(&w16(0));
        rec(&mut out, COLINFO, &b);
    }

    // 行高
    for (r, h) in sheet.rows.custom_pairs() {
        let mut b = Vec::new();
        b.extend_from_slice(&w16(r as u16));
        b.extend_from_slice(&w16(0));
        b.extend_from_slice(&w16((cmax + 1).min(255) as u16));
        b.extend_from_slice(&w16((h * 15.0).round().min(65535.0) as u16)); // px → twips (px*15 = pt*20)
        b.extend_from_slice(&w16(0));
        b.extend_from_slice(&w16(0));
        b.extend_from_slice(&w16(0x0100));
        b.extend_from_slice(&w16(0x0F));
        b.extend_from_slice(&w32(0));
        rec(&mut out, ROW, &b);
    }

    // 单元格
    let mut cells: Vec<(u32, u32, &crate::model::Cell)> = Vec::new();
    for (cref, cell) in sheet.iter() {
        if cell.is_blank() {
            continue;
        }
        cells.push((cref.row, cref.col, cell));
    }
    cells.sort_by_key(|(r, c, _)| (*r, *c));
    for (r, c, cell) in cells {
        let sid = sheet.style_of(r, c);
        let xf = match styles_cache.get(&sid) {
            Some(v) => *v,
            None => {
                let v = tables.xf(style_table.get(sid));
                styles_cache.insert(sid, v);
                v
            }
        };
        let mut b = Vec::new();
        b.extend_from_slice(&w16(r as u16));
        b.extend_from_slice(&w16(c as u16));
        b.extend_from_slice(&w16(xf));
        match &cell.value {
            Value::Number(n) => {
                b.extend_from_slice(&n.to_le_bytes());
                rec(&mut out, NUMBER, &b);
            }
            Value::Text(t) => {
                let s = unicode_str(t);
                b.extend_from_slice(&s);
                rec(&mut out, LABEL, &b);
            }
            Value::Bool(v) => {
                b.push(if *v { 1 } else { 0 });
                b.push(0);
                rec(&mut out, BOOLERR, &b);
            }
            Value::Empty => {}
        }
    }

    // 合并区
    if !sheet.merges.is_empty() {
        let mut b = Vec::new();
        b.extend_from_slice(&w16(sheet.merges.len().min(1027) as u16));
        for (r1, c1, r2, c2) in sheet.merges.iter().take(1027) {
            b.extend_from_slice(&w16(*r1 as u16));
            b.extend_from_slice(&w16(*r2 as u16));
            b.extend_from_slice(&w16(*c1 as u16));
            b.extend_from_slice(&w16(*c2 as u16));
        }
        rec(&mut out, MERGEDCELLS, &b);
    }

    rec(&mut out, EOF, &[]);
    out
}

pub fn write(path: &Path, wb: &Workbook) -> Result<()> {
    for s in &wb.sheets {
        for (cref, cell) in s.iter() {
            if cell.formula.is_some() {
                bail!(
                    "「{}」!{} 带公式，.xls 只写值不写公式；请用「另存为 xlsx」保留公式",
                    s.name,
                    crate::model::cell_name(cref.row, cref.col)
                );
            }
        }
    }
    if wb.sheets.len() > 255 {
        bail!(".xls 最多 255 张工作表");
    }

    let mut tables = Tables::new();
    // XF 0 必须是"常规"样式
    let _ = tables.xf(&CellStyle::default());

    let mut cache: HashMap<u16, u16> = HashMap::new();
    let mut sheets: Vec<Vec<u8>> = Vec::new();
    for s in &wb.sheets {
        sheets.push(sheet_stream(s, &wb.styles, &mut tables, &mut cache));
    }

    // 全局流：前缀、标准字体/XF 块都来自内嵌的合法骨架（Excel 生成），
    // 我们只在后面追加自己的格式/字体/XF 和 BOUNDSHEET。
    let mut g: Vec<u8> = Vec::new();
    g.extend_from_slice(TPL_PREFIX);
    g.extend_from_slice(TPL_FONTS_XFS);

    let (base_fonts, base_xfs) = tpl_counts();
    for (id, code) in tables.formats.clone() {
        let mut b = Vec::new();
        b.extend_from_slice(&w16(id));
        b.extend_from_slice(&unicode_str(&code));
        rec(&mut g, FORMAT, &b);
    }
    for f in tables.fonts.clone() {
        g.extend_from_slice(&font_record_abs(&f, base_fonts));
    }
    for x in tables.xfs.clone() {
        g.extend_from_slice(&xf_record_abs(&x, base_xfs));
    }
    g.extend_from_slice(TPL_STYLE);

    let mut patch_at: Vec<usize> = Vec::new();
    for s in &wb.sheets {
        let mut b = Vec::new();
        b.extend_from_slice(&w32(0)); // 占位
        b.extend_from_slice(&w16(0));
        b.push(s.name.encode_utf16().count().min(31) as u8);
        b.push(1);
        for u in s.name.encode_utf16().take(31) {
            b.extend_from_slice(&u.to_le_bytes());
        }
        patch_at.push(g.len() + 4);
        rec(&mut g, BOUNDSHEET, &b);
    }
    rec(&mut g, EOF, &[]);

    // 回填每张表的 BOF 偏移
    let mut base = g.len() as u32;
    for (i, s) in sheets.iter().enumerate() {
        g[patch_at[i]..patch_at[i] + 4].copy_from_slice(&base.to_le_bytes());
        base += s.len() as u32;
    }

    let mut stream = g;
    for s in sheets {
        stream.extend_from_slice(&s);
    }
    // 小于 4096 字节的流会落进 CFB 的 mini-stream，部分读取器（calamine/旧版 Excel）
    // 处理得不好；补零到 4096 之后，EOF 后面的字节会被忽略。
    while stream.len() < 4096 {
        stream.push(0);
    }

    crate::ole_write::write(path, &stream)
        .with_context(|| format!("写 {} 失败", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Sheet, Value};

    fn sample() -> Workbook {
        let mut wb = Workbook::new_blank();
        let mut sh = Sheet::new("发票");
        sh.set(0, 0, crate::model::Cell {
            value: Value::Text("标题".into()),
            formula: None,
        });
        let st = CellStyle {
            bold: true,
            size: 22.0,
            ..Default::default()
        };
        let id = wb.styles.intern(st);
        sh.set_style(0, 0, id);
        sh.set(1, 0, crate::model::Cell {
            value: Value::Number(42.5),
            formula: None,
        });
        sh.merges.push((0, 0, 0, 1));
        sh.cols.set(0, 140.0);
        wb.sheets = vec![sh];
        wb
    }

    #[test]
    fn round_trips_values_and_style_through_xls() {
        let dir = std::env::temp_dir().join("sc_xls_write");
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("t.xls");
        write(&p, &sample()).unwrap();
        let back = crate::xlsx::load(&p).unwrap();
        let sh = &back.sheets[0];
        assert_eq!(sh.name, "发票");
        match &sh.get(0, 0).unwrap().value {
            Value::Text(t) => assert_eq!(&**t, "标题"),
            v => panic!("A1 应为文本，得到 {v:?}"),
        }
        let st = back.styles.get(sh.style_of(0, 0));
        assert!(st.bold, "粗体应保留");
        assert!((st.size - 22.0).abs() < 0.1, "字号 {}", st.size);
        match &sh.get(1, 0).unwrap().value {
            Value::Number(n) => assert!((n - 42.5).abs() < 1e-9),
            v => panic!("A2 应为数字，得到 {v:?}"),
        }
        assert_eq!(sh.merges, vec![(0, 0, 0, 1)], "合并区应保留");
        assert!((sh.cols.size(0) - 140.0).abs() < 1.5, "列宽 {}", sh.cols.size(0));
    }

    #[test]
    fn refuses_formulas() {
        let mut wb = sample();
        wb.sheets[0].set(2, 0, crate::model::Cell {
            value: Value::Number(1.0),
            formula: Some("=1+1".into()),
        });
        let dir = std::env::temp_dir().join("sc_xls_write");
        let _ = std::fs::create_dir_all(&dir);
        let e = write(&dir.join("f.xls"), &wb).unwrap_err();
        assert!(format!("{e}").contains("公式"), "错误信息应说明公式：{e}");
    }
}
