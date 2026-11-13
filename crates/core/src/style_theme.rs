//! 主题色与 tint：Excel 的 `<color theme="4" tint="0.4"/>` 不能只当 RGB。
//!
//! 读盘时把主题索引 + tint 解析成 RGB（与 Excel 的 HSL 调整一致），同时把
//! 「这是主题色第几号、tint 多少」记在样式表里；保存时按主题写回，
//! 改别的属性不会把主题色降级成固定 RGB。

use crate::style::{CellStyle, StyleTable};
use std::collections::HashMap;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ThemeRef {
    pub idx: u8,
    pub tint: f32,
}

/// 一份工作簿的主题信息：调色板 + 每个 cellXfs 的主题色 + 每张表每格的 xf 序号
#[derive(Clone, Debug, Default)]
pub struct ThemeBook {
    pub palette: Vec<[u8; 3]>,
    pub xfs: Vec<XfTheme>,
    /// 工作表部件名 → (行,列) → cellXfs 序号
    pub cells: HashMap<String, HashMap<(u32, u32), u16>>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct XfTheme {
    pub font: Option<ThemeRef>,
    pub fill: Option<ThemeRef>,
}

/// 解析 xl/styles.xml + xl/theme/theme1.xml，并读取每张表的 `s=` 序号
pub fn load(path: &Path, sheet_parts: &[String]) -> ThemeBook {
    let mut out = ThemeBook::default();
    let Ok(file) = std::fs::File::open(path) else { return out };
    let Ok(mut zip) = zip::ZipArchive::new(file) else { return out };
    let mut read = |name: &str| -> Option<String> {
        use std::io::Read;
        let mut f = zip.by_name(name).ok()?;
        let mut s = String::new();
        f.read_to_string(&mut s).ok()?;
        Some(s)
    };
    if let Some(theme) = read("xl/theme/theme1.xml") {
        out.palette = parse_palette(&theme);
    }
    if let Some(styles) = read("xl/styles.xml") {
        out.xfs = parse_xfs(&styles);
    }
    for part in sheet_parts {
        if let Some(xml) = read(part) {
            out.cells.insert(part.clone(), parse_cell_styles(&xml));
        }
    }
    out
}

/// clrScheme 的 12 个颜色，顺序即 Excel 的 theme 索引：dk1 lt1 dk2 lt2 accent1-6 hlink folHlink
pub fn parse_palette(xml: &str) -> Vec<[u8; 3]> {
    let Some(i) = xml.find("<a:clrScheme") else { return Vec::new() };
    let rest = &xml[i..];
    let end = rest.find("</a:clrScheme>").unwrap_or(rest.len());
    let block = &rest[..end];
    let mut out = Vec::new();
    for key in [
        "dk1", "lt1", "dk2", "lt2", "accent1", "accent2", "accent3", "accent4", "accent5",
        "accent6", "hlink", "folHlink",
    ] {
        let pat = format!("<a:{key}>");
        let Some(p) = block.find(&pat) else {
            out.push([0, 0, 0]);
            continue;
        };
        let seg = &block[p + pat.len()..];
        let seg_end = seg.find("</a:").unwrap_or(seg.len());
        let seg = &seg[..seg_end];
        let color = seg
            .find("lastClr=\"")
            .or_else(|| seg.find("val=\""))
            .and_then(|q| {
                let rest = &seg[q..];
                let s = rest.find('"')? + 1;
                let e = rest[s..].find('"')? + s;
                hex6(&rest[s..e])
            })
            .unwrap_or([0, 0, 0]);
        out.push(color);
    }
    out
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

/// fonts/fills/cellXfs：取每个 xf 的主题字体色与主题填充色
pub fn parse_xfs(styles_xml: &str) -> Vec<XfTheme> {
    let fonts = section(styles_xml, "fonts");
    let fills = section(styles_xml, "fills");
    let font_colors: Vec<Option<ThemeRef>> = split_items(&fonts, "font")
        .iter()
        .map(|f| theme_of(f, "color"))
        .collect();
    let fill_colors: Vec<Option<ThemeRef>> = split_items(&fills, "fill")
        .iter()
        .map(|f| {
            let fg = theme_of(f, "fgColor");
            let bg = theme_of(f, "bgColor");
            fg.or(bg)
        })
        .collect();
    let xfs = section(styles_xml, "cellXfs");
    split_items(&xfs, "xf")
        .iter()
        .map(|xf| XfTheme {
            font: attr_num(xf, "fontId").and_then(|i| font_colors.get(i as usize).copied().flatten()),
            fill: attr_num(xf, "fillId").and_then(|i| fill_colors.get(i as usize).copied().flatten()),
        })
        .collect()
}

fn section(xml: &str, tag: &str) -> String {
    let open = format!("<{tag}");
    let Some(i) = xml.find(&open) else { return String::new() };
    let rest = &xml[i..];
    let close = format!("</{tag}>");
    let end = rest.find(&close).map(|e| e + close.len()).unwrap_or(rest.len());
    rest[..end].to_string()
}

/// 按 `<tag ...>` … `</tag>` 或自闭合 `<tag/>` 切条目
fn split_items(block: &str, tag: &str) -> Vec<String> {
    let mut out = Vec::new();
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut i = 0;
    while let Some(p) = block[i..].find(&open) {
        let start = i + p;
        let after = &block[start..];
        // 自闭合
        if let Some(gt) = after.find('>') {
            if after[..gt].ends_with('/') {
                out.push(after[..=gt].to_string());
                i = start + gt + 1;
                continue;
            }
        }
        let Some(c) = after.find(&close) else { break };
        out.push(after[..c + close.len()].to_string());
        i = start + c + close.len();
    }
    out
}

fn theme_of(frag: &str, tag: &str) -> Option<ThemeRef> {
    let pat = format!("<{tag}");
    let p = frag.find(&pat)?;
    let seg = &frag[p..];
    let end = seg.find('>')?;
    let head = &seg[..end];
    let idx = attr_num(head, "theme")?;
    let tint = attr_float(head, "tint").unwrap_or(0.0);
    Some(ThemeRef { idx: idx.min(255) as u8, tint })
}

fn attr_num(s: &str, key: &str) -> Option<u32> {
    attr_str(s, key)?.trim().parse().ok()
}

fn attr_float(s: &str, key: &str) -> Option<f32> {
    attr_str(s, key)?.trim().parse().ok()
}

fn attr_str(s: &str, key: &str) -> Option<String> {
    let pat = format!("{key}=\"");
    let i = s.find(&pat)? + pat.len();
    let rest = &s[i..];
    let e = rest.find('"')?;
    Some(rest[..e].to_string())
}

/// 每张工作表里 `<c r="A1" s="12">` 的 xf 序号
pub fn parse_cell_styles(xml: &str) -> HashMap<(u32, u32), u16> {
    let mut out = HashMap::new();
    for (pos, _) in xml.match_indices("<c ") {
        let rest = &xml[pos..];
        let end = rest.find('>').unwrap_or(rest.len());
        let head = &rest[..end];
        let Some(r) = attr_str(head, "r") else { continue };
        let Some(s) = attr_num(head, "s") else { continue };
        if let Some((row, col)) = a1(&r) {
            out.insert((row, col), s.min(u16::MAX as u32) as u16);
        }
    }
    out
}

fn a1(s: &str) -> Option<(u32, u32)> {
    let mut col = 0u32;
    let mut i = 0;
    for b in s.bytes() {
        if b.is_ascii_alphabetic() {
            col = col * 26 + (b.to_ascii_uppercase() - b'A' + 1) as u32;
            i += 1;
        } else {
            break;
        }
    }
    if i == 0 {
        return None;
    }
    let row: u32 = s[i..].parse().ok()?;
    Some((row.saturating_sub(1), col - 1))
}

/// Excel 的 tint：在 HSL 亮度上按比例加白/加黑
pub fn tint_rgb(base: [u8; 3], tint: f32) -> [u8; 3] {
    if tint.abs() < 1e-6 {
        return base;
    }
    let (h, s, l) = rgb_to_hsl(base);
    let l2 = if tint < 0.0 { l * (1.0 + tint as f64) } else { l * (1.0 - tint as f64) + tint as f64 };
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
/// Excel 默认字体 `<color theme="1"/>` 必须是黑色，不能直接拿 dk1/lt1 原始顺序。
fn scheme_index(i: u8) -> usize {
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
        // tint>0 提亮，tint<0 压暗
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
