//! 主题色、索引色与 RGB+tint：完整解析 `xl/styles.xml` 与 `xl/theme/*.xml`。
//!
//! 读盘时把主题索引 + tint、索引色（含自定义 `<indexedColors>`）及带 tint 的 RGB
//! 统一解析成准确 RGB，同时把「主题色第几号、tint 多少」记在样式表里；
//! 保存时按主题写回，改别的属性不会把主题色降级成固定 RGB。

use crate::style::{CellStyle, StyleTable};
use quick_xml::events::Event;
use quick_xml::Reader;
use std::collections::HashMap;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ThemeRef {
    pub idx: u8,
    pub tint: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct XfTheme {
    pub font: Option<ThemeRef>,
    pub fill: Option<ThemeRef>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct XfResolved {
    pub font_rgb: Option<[u8; 3]>,
    pub fill_rgb: Option<[u8; 3]>,
}

/// 一份工作簿的主题与颜色解析信息：调色板 + 每个 cellXfs 的主题引用与实算颜色 + 每张表每格的 xf 序号
#[derive(Clone, Debug, Default)]
pub struct ThemeBook {
    pub palette: Vec<[u8; 3]>,
    pub xfs: Vec<XfTheme>,
    pub resolved: Vec<XfResolved>,
    /// 工作表部件名 → (行,列) → cellXfs 序号
    pub cells: HashMap<String, HashMap<(u32, u32), u16>>,
}

/// Office 默认主题 12 色（当工作簿未内嵌 theme1.xml 时的标准回退：dk1, lt1, dk2, lt2, accent1..6, hlink, folHlink）
pub(crate) const DEFAULT_THEME_PALETTE: [[u8; 3]; 12] = [
    [0x00, 0x00, 0x00], // dk1
    [0xFF, 0xFF, 0xFF], // lt1
    [0x44, 0x54, 0x6A], // dk2
    [0xE7, 0xE6, 0xE6], // lt2
    [0x44, 0x72, 0xC4], // accent1
    [0xED, 0x7D, 0x31], // accent2
    [0xA5, 0xA5, 0xA5], // accent3
    [0xFF, 0xC0, 0x00], // accent4
    [0x5B, 0x9B, 0xD5], // accent5
    [0x70, 0xAD, 0x47], // accent6
    [0x05, 0x63, 0xC1], // hlink
    [0x95, 0x4F, 0x72], // folHlink
];

/// OOXML 标准 64 项索引调色板（ECMA-376 §18.8.27，WPS 与旧模板大量使用 indexed="0..63"）
pub(crate) const DEFAULT_INDEXED_PALETTE: [[u8; 3]; 64] = [
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
    [51, 102, 255],  [51, 204, 204],  [153, 204, 0],   [255, 255, 0],
    [255, 153, 0],   [255, 102, 0],   [102, 102, 153], [150, 150, 150],
    [0, 51, 102],    [51, 153, 102],  [0, 51, 0],      [51, 51, 0],
    [153, 51, 0],    [153, 51, 102],  [51, 51, 153],   [51, 51, 51],
];

fn local_name(raw: &str) -> &str {
    raw.rsplit_once(':').map(|(_, l)| l).unwrap_or(raw)
}

fn read_zip_str(zip: &mut zip::ZipArchive<std::fs::File>, name: &str) -> Option<String> {
    use std::io::Read;
    let mut f = zip.by_name(name).ok()?;
    let mut s = String::new();
    f.read_to_string(&mut s).ok()?;
    Some(s)
}

/// 解析 xl/styles.xml + xl/theme/*.xml，并读取每张表的 `s=` 序号
pub fn load(path: &Path, sheet_parts: &[String]) -> ThemeBook {
    let mut out = ThemeBook::default();
    let Ok(file) = std::fs::File::open(path) else { return out };
    let Ok(mut zip) = zip::ZipArchive::new(file) else { return out };
    let theme_xml = read_zip_str(&mut zip, "xl/theme/theme1.xml").or_else(|| {
        let alt_name = (0..zip.len()).find_map(|i| {
            let n = zip.by_index(i).ok()?.name().to_string();
            if n.starts_with("xl/theme/") && n.ends_with(".xml") {
                Some(n)
            } else {
                None
            }
        })?;
        read_zip_str(&mut zip, &alt_name)
    });
    if let Some(theme) = theme_xml {
        out.palette = parse_palette(&theme);
    }
    if out.palette.is_empty() {
        out.palette = DEFAULT_THEME_PALETTE.to_vec();
    }
    if let Some(styles) = read_zip_str(&mut zip, "xl/styles.xml") {
        let (xfs, resolved) = parse_styles_with_palette(&styles, &out.palette);
        out.xfs = xfs;
        out.resolved = resolved;
    }
    for part in sheet_parts {
        if let Some(xml) = read_zip_str(&mut zip, part) {
            out.cells.insert(part.clone(), parse_cell_styles(&xml));
        }
    }
    out
}

/// clrScheme 的 12 个颜色，顺序即 Excel 的 theme 槽位：dk1 lt1 dk2 lt2 accent1-6 hlink folHlink
pub fn parse_palette(xml: &str) -> Vec<[u8; 3]> {
    const SLOTS: [&str; 12] = [
        "dk1", "lt1", "dk2", "lt2", "accent1", "accent2", "accent3", "accent4", "accent5",
        "accent6", "hlink", "folHlink",
    ];
    let mut found: [Option<[u8; 3]>; 12] = [None; 12];
    let mut r = Reader::from_str(xml);
    let mut in_scheme = false;
    let mut cur_slot: Option<usize> = None;

    loop {
        match r.read_event() {
            ev @ (Ok(Event::Start(_)) | Ok(Event::Empty(_))) => {
                let is_empty = matches!(ev, Ok(Event::Empty(_)));
                let e = match &ev {
                    Ok(Event::Start(e)) | Ok(Event::Empty(e)) => e,
                    _ => unreachable!(),
                };
                let nm = e.name();
                let lname = local_name(nm.as_ref());
                if lname == "clrScheme" {
                    if !is_empty {
                        in_scheme = true;
                    }
                    continue;
                }
                if !in_scheme {
                    continue;
                }
                if let Some(pos) = SLOTS.iter().position(|&k| k == lname) {
                    if !is_empty {
                        cur_slot = Some(pos);
                    }
                    continue;
                }
                if let Some(slot) = cur_slot {
                    if found[slot].is_some() {
                        continue;
                    }
                    let mut last_clr: Option<String> = None;
                    let mut val_attr: Option<String> = None;
                    for a in e.attributes().flatten() {
                        let k = local_name(a.key.as_ref());
                        let v = a.value.to_string();
                        if k == "lastClr" {
                            last_clr = Some(v);
                        } else if k == "val" {
                            val_attr = Some(v);
                        }
                    }
                    let resolved = last_clr
                        .as_deref()
                        .and_then(hex6)
                        .or_else(|| val_attr.as_deref().and_then(hex6))
                        .or_else(|| match val_attr.as_deref() {
                            Some("window") | Some("background") | Some("lt1") => {
                                Some([255, 255, 255])
                            }
                            Some("windowText") | Some("dk1") => Some([0, 0, 0]),
                            _ => None,
                        });
                    if let Some(rgb) = resolved {
                        found[slot] = Some(rgb);
                    }
                }
            }
            Ok(Event::End(e)) => {
                let nm = e.name();
                let lname = local_name(nm.as_ref());
                if lname == "clrScheme" {
                    break;
                }
                if cur_slot.and_then(|s| SLOTS.get(s)).copied() == Some(lname) {
                    cur_slot = None;
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }

    if found.iter().all(|x| x.is_none()) {
        return Vec::new();
    }
    found
        .into_iter()
        .enumerate()
        .map(|(i, c)| c.unwrap_or(DEFAULT_THEME_PALETTE[i]))
        .collect()
}

fn hex6(s: &str) -> Option<[u8; 3]> {
    let t = s.trim();
    let t = t.strip_prefix('#').unwrap_or(t);
    if t.len() == 8 {
        return hex6(&t[2..]);
    }
    if t.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(t, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

#[derive(Clone, Copy, Default)]
struct ParsedColor {
    theme: Option<ThemeRef>,
    rgb: Option<[u8; 3]>,
}

fn parse_color_tag(
    e: &quick_xml::events::BytesStart<'_>,
    palette: &[[u8; 3]],
    indexed_palette: &[[u8; 3]; 64],
) -> ParsedColor {
    let mut rgb_attr: Option<String> = None;
    let mut theme_attr: Option<u8> = None;
    let mut indexed_attr: Option<u32> = None;
    let mut tint: f32 = 0.0;
    for a in e.attributes().flatten() {
        let k = local_name(a.key.as_ref());
        let v = a.value.to_string();
        match k {
            "rgb" => rgb_attr = Some(v),
            "theme" => {
                if let Ok(n) = v.trim().parse::<u32>() {
                    theme_attr = Some(n.min(255) as u8);
                }
            }
            "indexed" => {
                if let Ok(n) = v.trim().parse::<u32>() {
                    indexed_attr = Some(n);
                }
            }
            "tint" => {
                if let Ok(f) = v.trim().parse::<f32>() {
                    tint = f;
                }
            }
            _ => {}
        }
    }
    let theme = theme_attr.map(|idx| ThemeRef { idx, tint });
    let rgb = if let Some(t) = theme {
        palette
            .get(scheme_index(t.idx))
            .map(|base| tint_rgb(*base, t.tint))
    } else if let Some(hex) = rgb_attr.as_deref().and_then(hex6) {
        Some(tint_rgb(hex, tint))
    } else if let Some(idx) = indexed_attr {
        if (idx as usize) < indexed_palette.len() {
            Some(tint_rgb(indexed_palette[idx as usize], tint))
        } else {
            None
        }
    } else {
        None
    };
    ParsedColor { theme, rgb }
}

pub(crate) fn resolve_color_attrs(
    e: &quick_xml::events::BytesStart<'_>,
    palette: &[[u8; 3]],
    indexed_palette: &[[u8; 3]; 64],
) -> Option<[u8; 3]> {
    let pal = if palette.is_empty() {
        &DEFAULT_THEME_PALETTE[..]
    } else {
        palette
    };
    parse_color_tag(e, pal, indexed_palette).rgb
}

pub(crate) fn parse_indexed_palette(styles_xml: &str) -> [[u8; 3]; 64] {
    let mut indexed_palette = DEFAULT_INDEXED_PALETTE;
    let mut r = Reader::from_str(styles_xml);
    let mut in_indexed = false;
    let mut slot = 0usize;
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) => {
                let lname = local_name(e.name().as_ref()).to_string();
                if lname == "indexedColors" {
                    in_indexed = true;
                    slot = 0;
                } else if in_indexed && lname == "rgbColor" {
                    for a in e.attributes().flatten() {
                        if local_name(a.key.as_ref()) == "rgb" {
                            if let Some(c) = hex6(&a.value.to_string()) {
                                if slot < indexed_palette.len() {
                                    indexed_palette[slot] = c;
                                }
                            }
                        }
                    }
                    slot += 1;
                }
            }
            Ok(Event::Empty(e)) => {
                let lname = local_name(e.name().as_ref()).to_string();
                if in_indexed && lname == "rgbColor" {
                    for a in e.attributes().flatten() {
                        if local_name(a.key.as_ref()) == "rgb" {
                            if let Some(c) = hex6(&a.value.to_string()) {
                                if slot < indexed_palette.len() {
                                    indexed_palette[slot] = c;
                                }
                            }
                        }
                    }
                    slot += 1;
                }
            }
            Ok(Event::End(e)) => {
                if local_name(e.name().as_ref()) == "indexedColors" {
                    break;
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    indexed_palette
}

/// fonts/fills/cellXfs：取每个 xf 的主题字体色与主题填充色
pub fn parse_xfs(styles_xml: &str) -> Vec<XfTheme> {
    parse_styles_with_palette(styles_xml, &DEFAULT_THEME_PALETTE).0
}

pub fn parse_styles_with_palette(
    styles_xml: &str,
    palette: &[[u8; 3]],
) -> (Vec<XfTheme>, Vec<XfResolved>) {
    let indexed_palette = parse_indexed_palette(styles_xml);

    let mut font_cols: Vec<ParsedColor> = Vec::new();
    let mut fill_cols: Vec<ParsedColor> = Vec::new();
    let mut xfs_theme: Vec<XfTheme> = Vec::new();
    let mut xfs_res: Vec<XfResolved> = Vec::new();

    let mut r = Reader::from_str(styles_xml);
    let mut in_fonts = false;
    let mut in_font = false;
    let mut cur_font = ParsedColor::default();

    let mut in_fills = false;
    let mut in_fill = false;
    let mut fill_pattern = String::new();
    let mut cur_fg = ParsedColor::default();
    let mut cur_bg = ParsedColor::default();

    let mut in_cell_xfs = false;

    loop {
        match r.read_event() {
            ev @ (Ok(Event::Start(_)) | Ok(Event::Empty(_))) => {
                let is_empty = matches!(ev, Ok(Event::Empty(_)));
                let e = match &ev {
                    Ok(Event::Start(e)) | Ok(Event::Empty(e)) => e,
                    _ => unreachable!(),
                };
                let nm = e.name();
                let lname = local_name(nm.as_ref());
                match lname {
                    "fonts" => {
                        if !is_empty {
                            in_fonts = true;
                        }
                    }
                    "font" if in_fonts => {
                        cur_font = ParsedColor::default();
                        if is_empty {
                            font_cols.push(cur_font);
                        } else {
                            in_font = true;
                        }
                    }
                    "color" if in_font => {
                        cur_font = parse_color_tag(e, palette, &indexed_palette);
                    }
                    "fills" => {
                        if !is_empty {
                            in_fills = true;
                        }
                    }
                    "fill" if in_fills => {
                        fill_pattern.clear();
                        cur_fg = ParsedColor::default();
                        cur_bg = ParsedColor::default();
                        if is_empty {
                            fill_cols.push(ParsedColor::default());
                        } else {
                            in_fill = true;
                        }
                    }
                    "patternFill" if in_fill => {
                        for a in e.attributes().flatten() {
                            if local_name(a.key.as_ref()) == "patternType" {
                                fill_pattern = a.value.to_string();
                            }
                        }
                    }
                    "fgColor" if in_fill => {
                        cur_fg = parse_color_tag(e, palette, &indexed_palette);
                    }
                    "bgColor" if in_fill => {
                        cur_bg = parse_color_tag(e, palette, &indexed_palette);
                    }
                    "cellXfs" => {
                        if !is_empty {
                            in_cell_xfs = true;
                        }
                    }
                    "xf" if in_cell_xfs => {
                        let mut font_id: Option<usize> = None;
                        let mut fill_id: Option<usize> = None;
                        for a in e.attributes().flatten() {
                            let k = local_name(a.key.as_ref());
                            let v = a.value.to_string();
                            if k == "fontId" {
                                font_id = v.trim().parse().ok();
                            } else if k == "fillId" {
                                fill_id = v.trim().parse().ok();
                            }
                        }
                        let fc = font_id.and_then(|i| font_cols.get(i)).copied().unwrap_or_default();
                        let fl = fill_id.and_then(|i| fill_cols.get(i)).copied().unwrap_or_default();
                        xfs_theme.push(XfTheme {
                            font: fc.theme,
                            fill: fl.theme,
                        });
                        xfs_res.push(XfResolved {
                            font_rgb: fc.rgb,
                            fill_rgb: fl.rgb,
                        });
                    }
                    _ => {}
                }
            }
            Ok(Event::End(e)) => {
                let nm = e.name();
                let lname = local_name(nm.as_ref());
                match lname {
                    "font" if in_font => {
                        font_cols.push(cur_font);
                        in_font = false;
                    }
                    "fonts" => in_fonts = false,
                    "fill" if in_fill => {
                        let chosen = if fill_pattern.is_empty() || fill_pattern == "none" {
                            ParsedColor::default()
                        } else if cur_fg.rgb.is_some() || cur_fg.theme.is_some() {
                            cur_fg
                        } else {
                            cur_bg
                        };
                        fill_cols.push(chosen);
                        in_fill = false;
                    }
                    "fills" => in_fills = false,
                    "cellXfs" => in_cell_xfs = false,
                    _ => {}
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }

    (xfs_theme, xfs_res)
}

/// 每张工作表里 `<c r="A1" s="12">`（或省略 `r=` / 带命名空间前缀 `<x:c>`）的 xf 序号
pub fn parse_cell_styles(xml: &str) -> HashMap<(u32, u32), u16> {
    let mut out = HashMap::new();
    let _ = visit_cell_styles(xml, |r, c, id| { out.insert((r, c), id); });
    out
}

pub(crate) fn visit_cell_styles(xml: &str, mut cell: impl FnMut(u32, u32, u16)) -> anyhow::Result<(String, bool)> {
    let mut r = Reader::from_str(xml);
    let mut cur_row = 0u32;
    let mut next_col = 0u32;
    let mut in_data = false;
    let mut closed = false;
    let mut root = None;
    let mut implicit_coordinates = false;

    let mut shell = String::new();
    let mut copied = 0;
    loop {
        let start = r.buffer_position() as usize;
        match r.read_event() {
            ev @ (Ok(Event::Start(_)) | Ok(Event::Empty(_))) => {
                let e = match &ev {
                    Ok(Event::Start(e)) | Ok(Event::Empty(e)) => e,
                    _ => unreachable!(),
                };
                let nm = e.name();
                let lname = local_name(nm.as_ref());
                if root.is_none() {
                    root = Some(lname.to_owned());
                    closed = matches!(&ev, Ok(Event::Empty(_)));
                }
                if lname == "sheetData" {
                    in_data = matches!(&ev, Ok(Event::Start(_)));
                } else if in_data && lname == "row" {
                    next_col = 0;
                    let mut explicit = false;
                    for a in e.attributes().flatten() {
                        if local_name(a.key.as_ref()) == "r" {
                            if let Ok(rn) = a.value.trim().parse::<u32>() {
                                explicit = true;
                                cur_row = rn.saturating_sub(1);
                            }
                        }
                    }
                    implicit_coordinates |= !explicit;
                } else if in_data && lname == "c" {
                    let mut coord: Option<(u32, u32)> = None;
                    let mut s_idx: Option<u16> = None;
                    for a in e.attributes().flatten() {
                        let k = local_name(a.key.as_ref());
                        if k == "r" {
                            coord = a1(&a.value);
                        } else if k == "s" {
                            if let Ok(v) = a.value.trim().parse::<u32>() {
                                s_idx = Some(v.min(u16::MAX as u32) as u16);
                            }
                        }
                    }
                    implicit_coordinates |= coord.is_none();
                    let (row, col) = coord.unwrap_or((cur_row, next_col));
                    cur_row = row;
                    next_col = col + 1;
                    if let Some(s) = s_idx {
                        cell(row, col, s);
                    }
                    shell.push_str(&xml[copied..start]);
                    if matches!(&ev, Ok(Event::Start(_))) { r.read_to_end(e.name())?; }
                    copied = r.buffer_position() as usize;
                }
            }
            Ok(Event::End(e)) => {
                let tag = e.name();
                let name = local_name(tag.as_ref());
                if root.as_deref() == Some(name) { closed = true; }
                if name == "sheetData" { in_data = false; }
                if in_data && name == "row" {
                    cur_row += 1;
                    next_col = 0;
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(e.into()),
            _ => {}
        }
    }
    anyhow::ensure!(closed, "工作表 XML 未正常结束");
    shell.push_str(&xml[copied..]);
    Ok((shell, implicit_coordinates))
}

fn a1(s: &str) -> Option<(u32, u32)> {
    let mut col = 0u32;
    let mut i = 0;
    for b in s.bytes() {
        if b.is_ascii_alphabetic() {
            col = col * 26 + (b.to_ascii_uppercase() - b'A' + 1) as u32;
            i += 1;
        } else if b == b'$' && i == 0 {
            i += 1;
        } else {
            break;
        }
    }
    if col == 0 {
        return None;
    }
    let rest = s[i..].trim_start_matches('$');
    let row: u32 = rest.parse().ok()?;
    Some((row.saturating_sub(1), col - 1))
}

/// Excel 的 tint：在 HSL 亮度上按比例加白/加黑
pub fn tint_rgb(base: [u8; 3], tint: f32) -> [u8; 3] {
    if tint.abs() < 1e-6 {
        return base;
    }
    let (h, s, l) = rgb_to_hsl(base);
    let l2 = if tint < 0.0 {
        l * (1.0 + tint as f64)
    } else {
        l * (1.0 - tint as f64) + tint as f64
    };
    hsl_to_rgb(h, s, l2.clamp(0.0, 1.0))
}

fn rgb_to_hsl(c: [u8; 3]) -> (f64, f64, f64) {
    let r = c[0] as f64 / 255.0;
    let g = c[1] as f64 / 255.0;
    let b = c[2] as f64 / 255.0;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    if (max - min).abs() < 1e-9 {
        return (0.0, 0.0, l);
    }
    let d = max - min;
    let s = if l > 0.5 { d / (2.0 - max - min) } else { d / (max + min) };
    let h = if max == r {
        ((g - b) / d + if g < b { 6.0 } else { 0.0 }) / 6.0
    } else if max == g {
        ((b - r) / d + 2.0) / 6.0
    } else {
        ((r - g) / d + 4.0) / 6.0
    };
    (h, s, l)
}

fn hsl_to_rgb(h: f64, s: f64, l: f64) -> [u8; 3] {
    if s <= 0.0 {
        let v = (l * 255.0).round() as u8;
        return [v, v, v];
    }
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let f = |t: f64| -> f64 {
        let mut t = t;
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    [
        (f(h + 1.0 / 3.0) * 255.0).round() as u8,
        (f(h) * 255.0).round() as u8,
        (f(h - 1.0 / 3.0) * 255.0).round() as u8,
    ]
}

/// `theme=` 用的是 clrMap 槽位，不是 clrScheme 的 XML 顺序：
/// 0=bg1→lt1、1=tx1→dk1、2=bg2→lt2、3=tx2→dk2，4 起才是 accent1…
pub(crate) fn scheme_index(i: u8) -> usize {
    match i {
        0 => 1,
        1 => 0,
        2 => 3,
        3 => 2,
        n => n as usize,
    }
}

/// 把主题解析结果套到一格样式上（颜色按本模块的 tint 公式重算）
pub fn apply(cs: &mut CellStyle, xf: Option<&XfTheme>, palette: &[[u8; 3]]) {
    let Some(xf) = xf else { return };
    if let Some(t) = xf.font {
        if let Some(base) = palette.get(scheme_index(t.idx)) {
            cs.color = tint_rgb(*base, t.tint);
        }
    }
    if let Some(t) = xf.fill {
        if let Some(base) = palette.get(scheme_index(t.idx)) {
            cs.fill = Some(tint_rgb(*base, t.tint));
        }
    }
}

/// 同时应用主题色、索引色与 RGB+tint 的完整颜色解析结果
pub fn apply_resolved(
    cs: &mut CellStyle,
    xf: Option<&XfTheme>,
    res: Option<&XfResolved>,
    palette: &[[u8; 3]],
) {
    apply(cs, xf, palette);
    if let Some(r) = res {
        if xf.and_then(|x| x.font).is_none() {
            if let Some(c) = r.font_rgb {
                cs.color = c;
            }
        }
        if xf.and_then(|x| x.fill).is_none() {
            if let Some(c) = r.fill_rgb {
                cs.fill = Some(c);
            }
        }
    }
}

/// 保存时按样式号取回主题引用
pub fn theme_of_style(styles: &StyleTable, id: u16) -> XfTheme {
    styles.theme_of(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_order_and_tint() {
        let xml = r#"<a:theme><a:themeElements><a:clrScheme name="Office"><a:dk1><a:sysClr val="windowText" lastClr="000000"/></a:dk1><a:lt1><a:sysClr val="window" lastClr="FFFFFF"/></a:lt1><a:dk2><a:srgbClr val="44546A"/></a:dk2><a:lt2><a:srgbClr val="E7E6E6"/></a:lt2><a:accent1><a:srgbClr val="4472C4"/></a:accent1><a:accent2><a:srgbClr val="ED7D31"/></a:accent2><a:accent3><a:srgbClr val="A5A5A5"/></a:accent3><a:accent4><a:srgbClr val="FFC000"/></a:accent4><a:accent5><a:srgbClr val="5B9BD5"/></a:accent5><a:accent6><a:srgbClr val="70AD47"/></a:accent6><a:hlink><a:srgbClr val="0563C1"/></a:hlink><a:folHlink><a:srgbClr val="954F72"/></a:folHlink></a:clrScheme></a:themeElements></a:theme>"#;
        let p = parse_palette(xml);
        assert_eq!(p.len(), 12);
        assert_eq!(p[0], [0, 0, 0]);
        assert_eq!(p[4], [0x44, 0x72, 0xC4]);
        assert_eq!(p[5], [0xED, 0x7D, 0x31]);
        let light = tint_rgb([0x44, 0x72, 0xC4], 0.4);
        assert!(light[0] > 0x44 && light[2] > 0xC4, "{light:?}");
        let dark = tint_rgb([0x44, 0x72, 0xC4], -0.25);
        assert!(dark[0] < 0x44 && dark[2] < 0xC4, "{dark:?}");
    }

    #[test]
    fn xf_theme_refs_and_cell_styles() {
        let styles = r#"<styleSheet><fonts count="2"><font><sz val="11"/><color theme="4" tint="0.3999755851924192"/></font><font><b/><color rgb="FF9C0006"/></font></fonts><fills count="2"><fill><patternFill patternType="solid"><fgColor theme="5" tint="-0.249977111117893"/><bgColor indexed="64"/></patternFill></fill><fill><patternFill patternType="none"/></fill></fills><cellXfs count="2"><xf fontId="0" fillId="0" numFmtId="0"/><xf fontId="1" fillId="1" numFmtId="0"/></cellXfs></styleSheet>"#;
        let xfs = parse_xfs(styles);
        assert_eq!(xfs.len(), 2);
        assert_eq!(xfs[0].font, Some(ThemeRef { idx: 4, tint: 0.3999756 }));
        assert!(xfs[0].fill.is_some());
        assert_eq!(xfs[1].font, None);
        let cells = parse_cell_styles(r#"<sheetData><row r="1"><c r="A1" s="1" t="s"><v>0</v></c><c r="B2"/></row></sheetData>"#);
        assert_eq!(cells.get(&(0, 0)), Some(&1u16));
        assert_eq!(cells.get(&(1, 1)), None);
    }

    #[test]
    fn solid_rgb_fill_not_clobbered_by_bgcolor_theme_and_indexed_colors_resolve() {
        let styles = r#"<x:styleSheet>
            <x:fonts count="2">
                <x:font><x:color indexed="10"/></x:font>
                <x:font><x:color rgb="FF006100"/></x:font>
            </x:fonts>
            <x:fills count="3">
                <x:fill><x:patternFill patternType="none"><x:bgColor theme="0"/></x:patternFill></x:fill>
                <x:fill><x:patternFill patternType="solid"><x:fgColor rgb="FFFFFF00"/><x:bgColor theme="0"/></x:patternFill></x:fill>
                <x:fill><x:patternFill patternType="solid"><x:fgColor indexed="43"/><x:bgColor indexed="64"/></x:patternFill></x:fill>
            </x:fills>
            <x:cellXfs count="3">
                <x:xf fontId="0" fillId="0"/>
                <x:xf fontId="0" fillId="1"/>
                <x:xf fontId="1" fillId="2"/>
            </x:cellXfs>
        </x:styleSheet>"#;
        let (xfs, res) = parse_styles_with_palette(styles, &DEFAULT_THEME_PALETTE);
        assert_eq!(xfs[1].fill, None, "fgColor 为 RGB 时不得把 bgColor 的 theme=0 当主题填充");
        assert_eq!(res[0].fill_rgb, None, "patternType=none 不得产生背景填充");
        assert_eq!(res[1].fill_rgb, Some([255, 255, 0]), "solid 黄色不得被 bgColor theme=0 冲成白色");
        assert_eq!(res[1].font_rgb, Some([255, 0, 0]), "indexed=10 应解析为红色");
        assert_eq!(res[2].fill_rgb, Some([255, 255, 153]), "indexed=43 应解析为浅黄");
        assert_eq!(res[2].font_rgb, Some([0, 0x61, 0]));
    }

    #[test]
    fn theme_slots_follow_clrmap() {
        let palette = parse_palette(r#"<a:theme><a:clrScheme><a:dk1><a:sysClr lastClr="000000"/></a:dk1><a:lt1><a:sysClr lastClr="FFFFFF"/></a:lt1><a:dk2><a:srgbClr val="44546A"/></a:dk2><a:lt2><a:srgbClr val="E7E6E6"/></a:lt2><a:accent1><a:srgbClr val="4472C4"/></a:accent1></a:clrScheme></a:theme>"#);
        let mut cs = CellStyle::default();
        cs.color = [255, 255, 255];
        apply(&mut cs, Some(&XfTheme { font: Some(ThemeRef { idx: 1, tint: 0.0 }), fill: None }), &palette);
        assert_eq!(cs.color, [0, 0, 0], "theme=1 是 tx1/dk1");
        let mut fill = CellStyle::default();
        apply(&mut fill, Some(&XfTheme { font: None, fill: Some(ThemeRef { idx: 0, tint: 0.0 }) }), &palette);
        assert_eq!(fill.fill, Some([255, 255, 255]), "theme=0 是 bg1/lt1");
        let mut accent = CellStyle::default();
        apply(&mut accent, Some(&XfTheme { font: Some(ThemeRef { idx: 4, tint: 0.0 }), fill: None }), &palette);
        assert_eq!(accent.color, [0x44, 0x72, 0xC4]);
    }
}
