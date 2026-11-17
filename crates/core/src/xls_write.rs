//! 写老式 .xls（BIFF8）。
//!
//! 写值、格式和 BIFF 可表达的公式（数字、文本、布尔、字体/字号/加粗/颜色、
//! 数字格式、对齐、换行、四边边框、底色、合并区、列宽、行高）。
//! 公式必须编译成 BIFF 的 RPN 记号流；`@` 隐式交集、动态数组 `#` 等
//! `.xls` 不能表达的语法会在写盘前报“兼容提示”，原文件和模型里的公式、
//! 缓存值保持不变，调用方应引导用户另存为 `.xlsx`。
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
const CALCMODE: u16 = 0x000D;
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
const FORMULA: u16 = 0x0006;
const STRING: u16 = 0x0207;
const SUPBOOK: u16 = 0x01AE;
const EXTERNSHEET: u16 = 0x0017;
const NAME: u16 = 0x0018;
const WINDOW1: u16 = 0x003D;
const STYLE: u16 = 0x0293;

/// 合法骨架：Excel 生成的空工作簿里的 全局流前缀 / 字体+XF 块 / STYLE 记录
const TPL_PREFIX: &[u8] = include_bytes!("xls_tpl/prefix.bin");
const TPL_FONTS_XFS: &[u8] = include_bytes!("xls_tpl/fonts_xfs.bin");
const TPL_STYLE: &[u8] = include_bytes!("xls_tpl/style.bin");

/// 骨架里已有的字体数 / XF 数：我们的表和单元格引用都从这之后开始编号
/// 把全局流里的 DATEMODE(0x0022) 改成指定值；没有就在 BOF 之后插一条。
fn set_datemode(buf: &mut Vec<u8>, is_1904: bool) {
    let want = if is_1904 { 1u16 } else { 0u16 };
    let mut i = 0usize;
    while i + 4 <= buf.len() {
        let t = u16::from_le_bytes([buf[i], buf[i + 1]]);
        let l = u16::from_le_bytes([buf[i + 2], buf[i + 3]]) as usize;
        if t == 0x0022 && i + 6 <= buf.len() {
            buf[i + 4] = (want & 0xFF) as u8;
            buf[i + 5] = (want >> 8) as u8;
            return;
        }
        let n = (i + 4 + l).min(buf.len());
        if n <= i {
            break;
        }
        i = n;
    }
    // 没找到：插在第一条记录（BOF）之后
    if buf.len() >= 4 {
        let l0 = u16::from_le_bytes([buf[2], buf[3]]) as usize;
        let at = (4 + l0).min(buf.len());
        let rec_bytes = [0x22u8, 0x00, 0x02, 0x00, (want & 0xFF) as u8, (want >> 8) as u8];
        buf.splice(at..at, rec_bytes);
    }
}

/// CALCMODE(0x000D) 的兼容写入位置：Excel/WPS 只认工作表流 BOF 之后的记录；
/// 具体写入由 sheet_stream 负责，本文件不再修改全局流里的记录。

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

/// FORMULA 记录里的 8 字节缓存值（MS-XLS Xnum）；文本还要跟一条 STRING 记录。
///
/// 布局：非数值结果把 **num[6..8] 写成 0xFFFF**，num[0] = 类型（0 串/1 布尔/2 错误/3 空），
/// num[2] = 取值。不能把标号放在 num[0..2]：那样普通数值只要低 16 位是 0xFFFF
/// （1.0000000000145517 就是）就会被当成特殊值读坏。
fn cached_value(v: &Value) -> ([u8; 8], Option<Vec<u8>>) {
    let mut b = [0u8; 8];
    match v {
        Value::Number(n) => b.copy_from_slice(&n.to_le_bytes()),
        Value::Text(t) => {
            b[0] = 0; // 字符串：值在随后的 STRING 记录里
            b[6] = 0xFF;
            b[7] = 0xFF;
            let mut sb = Vec::new();
            sb.extend_from_slice(&unicode_str(t));
            return (b, Some(sb));
        }
        Value::Bool(x) => {
            b[0] = 1;
            b[2] = if *x { 1 } else { 0 };
            b[6] = 0xFF;
            b[7] = 0xFF;
        }
        Value::Error(e) => {
            b[0] = 2;
            b[2] = biff_error_code(*e);
            b[6] = 0xFF;
            b[7] = 0xFF;
        }
        Value::Empty => {
            b[0] = 3;
            b[6] = 0xFF;
            b[7] = 0xFF;
        }
    }
    (b, None)
}

/// BIFF8 的单元格错误码（与 OOXML 的 `t="e"` 文本一一对应）
fn biff_error_code(e: crate::formula::ErrKind) -> u8 {
    use crate::formula::ErrKind as K;
    match e {
        K::Null => 0x00,
        K::Div0 => 0x07,
        K::Value => 0x0F,
        K::Ref | K::Circular => 0x17,
        K::Name => 0x1D,
        K::Num => 0x24,
        K::Na => 0x2A,
        K::Spill => 0x0F, // BIFF8 没有 #SPILL!，用 #VALUE! 兜底
    }
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
    // 边框颜色：Excel 的"自动"色是 40/20 交替（照它自己的样）
    b[12] = 0x40;
    b[13] = 0x20;
    b[14] = 0x40;
    b[15] = 0x20;
    // 填充：Excel 用 17=0x06 表示实心、0x02 表示无填充（实测字节，不是教科书）
    b[16] = 0x00;
    match x.fill {
        Some(idx) => {
            b[17] = 0x06;
            b[18] = idx;
        }
        None => {
            b[17] = 0x02;
            b[18] = 0xC0;
        }
    }
    b[19] = 0x20;
    rec_body(XF, b)
}

fn sheet_stream(
    sheet: &Sheet,
    sheet_index: usize,
    all_sheets: &[String],
    externs: &mut crate::xls_formula::Externs,
    names: &mut crate::xls_formula::NameSpace,
    books: &[crate::xls_formula::BookSheets],
    style_table: &crate::style::StyleTable,
    tables: &mut Tables,
    styles_cache: &mut HashMap<u16, u16>,
    calc_manual: bool,
) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut bof = Vec::new();
    bof.extend_from_slice(&w16(0x0600));
    bof.extend_from_slice(&w16(0x0010)); // 工作表
    bof.extend_from_slice(&w16(0x0DBB));
    bof.extend_from_slice(&w16(0x07CC));
    bof.extend_from_slice(&w32(0));
    bof.extend_from_slice(&w32(0));
    rec(&mut out, BOF, &bof);
    // Excel 16 的实际行为：CALCMODE 放在工作表 BOF 之后才生效；放在全局流里
    // Excel/WPS 都按自动计算打开（缓存值虽在，模式提示错误）。0=手动，1=自动。
    rec(&mut out, CALCMODE, &w16(if calc_manual { 0 } else { 1 }));

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
        // 像素 → 字符数（内部 px 含 5px 内边距：BIFF 只存字符数）
        let chars = ((w - 5.0) / 8.0).max(0.0);
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
        b.extend_from_slice(&w16(0x0140)); // 自定义行高（照 Excel 自己的样）
        b.extend_from_slice(&w16(0x0F));
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
        if let Some(text) = cell.formula.as_deref() {
            let node = crate::formula::parse(text.trim_start_matches('='))
                .map_err(|e| anyhow::anyhow!(
                    "{}!{} 的公式 {text} 解析失败（{e:?}），无法写入 .xls",
                    sheet.name,
                    crate::model::cell_name(r, c)
                ))?;
            let mut comp = crate::xls_formula::Compiler {
                sheets: all_sheets,
                cur_sheet: sheet_index,
                externs,
                names,
                books,
                extra: Vec::new(),
            };
            let tokens = comp.compile(&node).map_err(|e| {
                anyhow::anyhow!(
                    "{}!{} 的公式 {text} 写不进 .xls：{e}",
                    sheet.name,
                    crate::model::cell_name(r, c)
                )
            })?;
            let (cached, follow) = cached_value(&cell.value);
            b.extend_from_slice(&cached);
            b.extend_from_slice(&w16(0)); // grbit
            b.extend_from_slice(&w32(0)); // chn
            b.extend_from_slice(&w16(tokens.len() as u16));
            b.extend_from_slice(&tokens);
            // 数组常量的值跟在记号流后面（rgcb）
            b.extend_from_slice(&comp.extra);
            rec(&mut out, FORMULA, &b);
            if let Some(sr) = follow {
                rec(&mut out, STRING, &sr);
            }
            continue;
        }
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
            Value::Error(e) => {
                // BOOLERR 的字节序是 (bBoolErr, fError)：错误时第 1 字节放错误码、
                // 第 2 字节置 1。写反了读端就会把 1 当错误码，整格丢掉。
                b.push(biff_error_code(*e));
                b.push(1);
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
    Ok(out)
}

pub fn write(path: &Path, wb: &Workbook) -> Result<()> {
    if wb.sheets.len() > 255 {
        bail!(".xls 最多 255 张工作表");
    }

    let mut tables = Tables::new();
    // XF 0 必须是"常规"样式
    let _ = tables.xf(&CellStyle::default());

    let mut cache: HashMap<u16, u16> = HashMap::new();
    let mut sheets: Vec<Vec<u8>> = Vec::new();
    let sheet_names: Vec<String> = wb.sheets.iter().map(|s| s.name.clone()).collect();
    let mut externs = crate::xls_formula::Externs::default();

    // 外部簿的表清单：跨簿引用要靠它写 SUPBOOK 和表号
    let books: Vec<crate::xls_formula::BookSheets> = wb
        .links
        .iter()
        .map(|l| crate::xls_formula::BookSheets {
            file: l.file_name(),
            sheets: l.sheets.clone(),
        })
        .collect();

    // 定义名称：先把索引排好（1 起），再编译各自的定义
    let mut names = crate::xls_formula::NameSpace::default();
    let mut order: Vec<(u16, String)> = Vec::new();
    for (name, scope, text) in &wb.names {
        let idx = names.push_user(name.clone(), *scope);
        order.push((idx, text.clone()));
    }
    for (idx, text) in order {
        let node = match crate::formula::parse(text.trim_start_matches('=')) {
            Ok(n) => n,
            Err(e) => {
                eprintln!("[xls] 名称定义 {text} 解析失败（{e:?}），跳过");
                continue;
            }
        };
        let rgce = {
            let mut c = crate::xls_formula::Compiler {
                sheets: &sheet_names,
                cur_sheet: 0,
                externs: &mut externs,
                names: &mut names,
                books: &books,
                extra: Vec::new(),
            };
            c.compile(&node)?
        };
        names.set_rgce(idx, rgce);
    }

    for (i, s) in wb.sheets.iter().enumerate() {
        let stream = sheet_stream(
            s,
            i,
            &sheet_names,
            &mut externs,
            &mut names,
            &books,
            &wb.styles,
            &mut tables,
            &mut cache,
            wb.calc_manual,
        )
        .with_context(|| {
            format!(
                "兼容提示：.xls 不能完整保存该公式/引用组合（工作表 {}）；\
                 原文件和单元格里的公式、缓存值都保留，请另存为 .xlsx 或修改公式后重试",
                s.name
            )
        })?;
        sheets.push(stream);
    }

    // 全局流：前缀、标准字体/XF 块都来自内嵌的合法骨架（Excel 生成），
    // 我们只在后面追加自己的格式/字体/XF 和 BOUNDSHEET。
    let mut g: Vec<u8> = Vec::new();
    // DATEMODE：骨架前缀里本来就有（Excel 生成的空簿写的是 0），改值而不是追加，
    // 否则读端先撞上骨架里的 0。
    let mut prefix = TPL_PREFIX.to_vec();
    set_datemode(&mut prefix, wb.date_system.is_1904());
    g.extend_from_slice(&prefix);
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
    // 跨表引用要先有 SUPBOOK（本簿自引用）和 EXTERNSHEET 条目表
    if !externs.is_empty() {
        // 0 = 本簿自引用
        let mut sb = Vec::new();
        sb.extend_from_slice(&w16(wb.sheets.len().min(65535) as u16));
        sb.extend_from_slice(&w16(0x0401)); // 自引用标记
        rec(&mut g, SUPBOOK, &sb);

        // 1.. 外部簿：ctab + 文件名 + 各表名
        for b in &externs.books {
            let mut sb = Vec::new();
            sb.extend_from_slice(&w16(b.sheets.len().min(65535) as u16));
            sb.extend_from_slice(&unicode_str(&b.file));
            for s in &b.sheets {
                sb.extend_from_slice(&unicode_str(s));
            }
            rec(&mut g, SUPBOOK, &sb);
        }
        // 末尾是 add-in（2007+ 的 _xlfn 函数名靠它挂）
        if externs.addin {
            let mut sb = Vec::new();
            sb.extend_from_slice(&w16(1));
            // 加插件 SUPBOOK 的标记字节是 0x3A01（实测，不是 0x013A）
            sb.extend_from_slice(&w16(0x3A01));
            rec(&mut g, SUPBOOK, &sb);
        }

        let mut es = Vec::new();
        es.extend_from_slice(&w16(externs.entries.len() as u16));
        for (sup, first, last) in externs.entries.iter() {
            es.extend_from_slice(&w16(*sup));
            es.extend_from_slice(&w16(*first));
            es.extend_from_slice(&w16(*last));
        }
        rec(&mut g, EXTERNSHEET, &es);
    }

    // 定义名称 + _xlfn 名（索引必须与 ptgName 一致：先用户名称，再未来函数）
    for (name, scope, rgce) in names.user.iter() {
        let body = crate::xls_formula::name_record(name, *scope, rgce, false);
        rec(&mut g, NAME, &body);
    }
    for f in names.future.iter() {
        let body = crate::xls_formula::name_record(f, None, &crate::xls_formula::future_rgce(), true);
        rec(&mut g, NAME, &body);
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
    fn writes_formula_records() {
        let mut wb = sample();
        wb.sheets[0].set(2, 0, crate::model::Cell {
            value: Value::Number(3.0),
            formula: Some("=1+2".into()),
        });
        let dir = std::env::temp_dir().join("sc_xls_write");
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("f.xls");
        write(&p, &wb).unwrap();

        // 文件里必须真的有一条 FORMULA 记录（0x0006），而不是把公式降级成值
        let cf = crate::cfb::Compound::open(std::fs::read(&p).unwrap()).unwrap();
        let stream = cf.stream_by_path("Workbook").unwrap();
        let mut i = 0usize;
        let mut found = false;
        while i + 4 <= stream.len() {
            let t = u16::from_le_bytes([stream[i], stream[i + 1]]);
            let l = u16::from_le_bytes([stream[i + 2], stream[i + 3]]) as usize;
            if t == 0x0006 {
                found = true;
                break;
            }
            let n = (i + 4 + l).min(stream.len());
            if n <= i {
                break;
            }
            i = n;
        }
        assert!(found, "应写出 FORMULA 记录");

        // 再读回来：值仍是公式算出来的 3
        let back = crate::xlsx::load(&p).unwrap();
        match &back.sheets[0].get(2, 0).unwrap().value {
            Value::Number(n) => assert!((n - 3.0).abs() < 1e-9),
            v => panic!("应为 3，得到 {v:?}"),
        }
    }

    #[test]
    fn round_trips_calc_mode_through_xls() {
        let dir = std::env::temp_dir().join("sc_xls_calc_mode");
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join(format!("calc_{}.xls", std::process::id()));
        let mut wb = sample();
        wb.calc_manual = true;
        write(&p, &wb).unwrap();
        let back = crate::xlsx::load(&p).unwrap();
        assert!(back.calc_manual, "BIFF CALCMODE 手动模式应读回");
        // 旧实现把 CALCMODE 插在全局流 BOF 之后；Excel/WPS 都忽略，按自动模式打开。
        // 这里额外锁定记录位置：必须位于第一张工作表 BOF 之后；这是 Excel/WPS
        // 识别逐表 CALCMODE 的必要条件（本断言不声称已验证每条记录都紧邻）。
        let bytes = std::fs::read(&p).unwrap();
        let cf = crate::cfb::Compound::open(bytes).unwrap();
        let stream = cf.stream_by_path("Workbook").unwrap();
        let mut i = 0usize;
        let mut sheet_bof = None;
        let mut calc_at = None;
        while i + 4 <= stream.len() {
            let t = u16::from_le_bytes([stream[i], stream[i + 1]]);
            let l = u16::from_le_bytes([stream[i + 2], stream[i + 3]]) as usize;
            if t == BOF && i + 8 <= stream.len() {
                let dt = u16::from_le_bytes([stream[i + 6], stream[i + 7]]);
                if dt == 0x0010 && sheet_bof.is_none() {
                    sheet_bof = Some(i);
                }
            }
            if t == CALCMODE && calc_at.is_none() {
                calc_at = Some(i);
            }
            let n = (i + 4 + l).min(stream.len());
            if n <= i {
                break;
            }
            i = n;
        }
        assert!(
            sheet_bof.is_some() && calc_at.is_some() && calc_at > sheet_bof,
            "CALCMODE 必须在工作表 BOF 之后（Excel/WPS 才认）：bof={sheet_bof:?} calc={calc_at:?}"
        );
        wb.calc_manual = false;
        write(&p, &wb).unwrap();
        let back = crate::xlsx::load(&p).unwrap();
        assert!(!back.calc_manual, "BIFF CALCMODE 自动模式应读回");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn unsupported_formula_save_keeps_target_and_reports_compatibility() {
        let dir = std::env::temp_dir().join(format!(
            "sc_xls_unsupported_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::create_dir_all(&dir);
        let target = dir.join("keep.xls");
        std::fs::write(&target, b"ORIGINAL").unwrap();
        let mut wb = Workbook::new_blank();
        wb.sheets[0].name = "Data".into();
        wb.sheets[0].set(0, 0, crate::model::Cell {
            value: Value::Number(7.0),
            formula: Some("=@A1".into()),
        });
        let err = write(&target, &wb).expect_err("隐式交集不能写 .xls");
        let text = format!("{err:#}");
        assert!(text.contains("兼容提示"), "应给兼容提示：{text}");
        assert!(text.contains("Data!A1"), "应指出单元格：{text}");
        assert_eq!(std::fs::read(&target).unwrap(), b"ORIGINAL", "失败不得覆盖目标");
        assert_eq!(wb.sheets[0].get(0, 0).unwrap().formula.as_deref(), Some("=@A1"));
        assert_eq!(wb.sheets[0].value(0, 0), Value::Number(7.0));

        // 同一工作簿用 xlsx 保存仍保留公式和缓存，不是只在内存里假装。
        let xlsx = dir.join("keep.xlsx");
        crate::xlsx::save(&xlsx, &wb).unwrap();
        let back = crate::xlsx::load(&xlsx).unwrap();
        assert_eq!(back.sheets[0].get(0, 0).unwrap().formula.as_deref(), Some("=@A1"));
        assert_eq!(back.sheets[0].value(0, 0), Value::Number(7.0));
        let _ = std::fs::remove_dir_all(&dir);
    }

}
