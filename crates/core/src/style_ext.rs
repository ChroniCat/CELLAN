//! C18.2：图案填充与对角线边框的「保真」模型。
//!
//! 这些属性不进 `CellStyle`（那是热路径上的紧凑结构），而是按样式号挂在
//! `StyleTable` 的平行表里：读盘时记下来，保存时按原 patternType / diagonal 写回，
//! 改别的属性不会把它们抹掉。渲染暂未建模，状态如实记为实施中。

use crate::style::StyleTable;
use std::path::Path;

#[derive(Clone, Debug, PartialEq)]
pub struct FillPattern {
    /// `patternType` 原值：gray125、lightGrid、darkTrellis…
    pub kind: String,
    pub fg: Option<[u8; 3]>,
    pub bg: Option<[u8; 3]>,
    pub fg_theme: Option<(u8, f32)>,
    pub bg_theme: Option<(u8, f32)>,
    pub fg_indexed: Option<u8>,
    pub bg_indexed: Option<u8>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DiagBorder {
    /// `diagonal` 的 style 原值
    pub style: String,
    pub color: [u8; 3],
    pub color_theme: Option<(u8, f32)>,
    pub up: bool,
    pub down: bool,
}

#[derive(Clone, Debug, Default)]
pub struct StyleExt {
    pub patterns: Vec<Option<FillPattern>>,
    pub diags: Vec<Option<DiagBorder>>,
    /// 每个 cellXfs 的 fillId / borderId
    pub xf_fill: Vec<u16>,
    pub xf_border: Vec<u16>,
    /// 每个 cellXfs 是否带 quotePrefix
    pub xf_quote: Vec<bool>,
}

/// 解析 styles.xml 的 fills 与 borders（只取图案与对角线这两类未建模属性）
pub fn load(path: &Path) -> StyleExt {
    use std::io::Read;
    let Ok(file) = std::fs::File::open(path) else { return StyleExt::default() };
    let Ok(mut zip) = zip::ZipArchive::new(file) else { return StyleExt::default() };
    let Ok(mut f) = zip.by_name("xl/styles.xml") else { return StyleExt::default() };
    let mut xml = String::new();
    if f.read_to_string(&mut xml).is_err() {
        return StyleExt::default();
    }
    parse(&xml)
}

pub fn parse(xml: &str) -> StyleExt {
    let mut out = StyleExt::default();
    let fills = section(xml, "fills");
    for item in split_items(&fills, "fill") {
        out.patterns.push(pattern_of(&item));
    }
    let borders = section(xml, "borders");
    for item in split_items(&borders, "border") {
        out.diags.push(diag_of(&item));
    }
    let xfs = section(xml, "cellXfs");
    for xf in split_items(&xfs, "xf") {
        out.xf_fill.push(num(&xf, "fillId").unwrap_or(0) as u16);
        out.xf_border.push(num(&xf, "borderId").unwrap_or(0) as u16);
        let q = attr(&xf, "quotePrefix").map(|v| v == "1" || v == "true").unwrap_or(false);
        out.xf_quote.push(q);
    }
    out
}

fn pattern_of(item: &str) -> Option<FillPattern> {
    let pf = tag_start(item, "patternFill")?;
    let kind = attr(&pf, "patternType")?;
    if kind.is_empty() || kind == "none" {
        return None;
    }
    let (fg, fg_theme, fg_indexed) = color_of(item, "fgColor");
    let (bg, bg_theme, bg_indexed) = color_of(item, "bgColor");
    Some(FillPattern { kind, fg, bg, fg_theme, bg_theme, fg_indexed, bg_indexed })
}

fn diag_of(item: &str) -> Option<DiagBorder> {
    let bhead = tag_start(item, "border").unwrap_or_else(|| item.to_string());
    let up = attr(&bhead, "diagonalUp").map(|v| v == "1" || v == "true").unwrap_or(false);
    let down = attr(&bhead, "diagonalDown").map(|v| v == "1" || v == "true").unwrap_or(false);
    let diag = tag_block(item, "diagonal")?;
    let head = tag_start(item, "diagonal")?;
    let style = attr(&head, "style").unwrap_or_default();
    if style.is_empty() || style == "none" {
        return None;
    }
    let (rgb, theme, _) = color_of(&diag, "color");
    Some(DiagBorder { style, color: rgb.unwrap_or([0, 0, 0]), color_theme: theme, up, down })
}

/// 取某色标签的 RGB 与主题引用；`<diagonal>` 里的 `<color>` 用 diagonalColor 这个假名查
fn color_of(item: &str, tag: &str) -> (Option<[u8; 3]>, Option<(u8, f32)>, Option<u8>) {
    let seg = match tag_start(item, tag) {
        Some(s) => s,
        None => return (None, None, None),
    };
    let theme = num(&seg, "theme").map(|i| (i as u8, float(&seg, "tint").unwrap_or(0.0)));
    let rgb = attr(&seg, "rgb").and_then(|s| hex6(&s));
    let indexed = num(&seg, "indexed").map(|i| i.min(255) as u8);
    (rgb, theme, indexed)
}

fn hex6(s: &str) -> Option<[u8; 3]> {
    let t = s.trim();
    let t = if t.len() == 8 { &t[2..] } else { t };
    if t.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(t, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

/// `<tag ...>…</tag>` 整段（含子元素）
fn tag_block(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}");
    let i = xml.find(&open)?;
    let rest = &xml[i..];
    let close = format!("</{tag}>");
    let end = rest.find(&close)? + close.len();
    Some(rest[..end].to_string())
}

fn tag_start(xml: &str, tag: &str) -> Option<String> {
    let pat = format!("<{tag}");
    let i = xml.find(&pat)?;
    let rest = &xml[i..];
    let end = rest.find('>')?;
    Some(rest[..=end].to_string())
}

fn attr(head: &str, key: &str) -> Option<String> {
    let pat = format!("{key}=\"");
    let i = head.find(&pat)? + pat.len();
    let rest = &head[i..];
    let e = rest.find('"')?;
    Some(rest[..e].to_string())
}

fn num(head: &str, key: &str) -> Option<u32> {
    attr(head, key)?.trim().parse().ok()
}

fn float(head: &str, key: &str) -> Option<f32> {
    attr(head, key)?.trim().parse().ok()
}

fn section(xml: &str, tag: &str) -> String {
    let open = format!("<{tag}");
    let Some(i) = xml.find(&open) else { return String::new() };
    let rest = &xml[i..];
    let close = format!("</{tag}>");
    let end = rest.find(&close).map(|e| e + close.len()).unwrap_or(rest.len());
    rest[..end].to_string()
}

fn split_items(block: &str, tag: &str) -> Vec<String> {
    let mut out = Vec::new();
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut i = 0;
    while let Some(p) = block[i..].find(&open) {
        let start = i + p;
        let after = &block[start..];
        // `<fill` 也匹配 `<fills`，`<border` 也匹配 `<borders`：标签名后必须是空白/>//
        let next = after.as_bytes().get(open.len()).copied().unwrap_or(b'>');
        if !(next == b' ' || next == b'>' || next == b'/') {
            i = start + open.len();
            continue;
        }
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

/// 把解析结果挂到样式表上
pub fn attach(styles: &mut StyleTable, ext: &StyleExt, id: u16, xf_index: u16) {
    if let Some(fill_id) = ext.xf_fill.get(xf_index as usize) {
        if let Some(p) = ext.patterns.get(*fill_id as usize).cloned().flatten() {
            styles.set_pattern(id, p);
        }
    }
    if ext.xf_quote.get(xf_index as usize).copied().unwrap_or(false) {
        styles.set_quote(id, true);
    }
    if let Some(border_id) = ext.xf_border.get(xf_index as usize) {
        if let Some(d) = ext.diags.get(*border_id as usize).cloned().flatten() {
            styles.set_diag(id, d);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pattern_and_diagonal_parse() {
        let xml = r#"<styleSheet><fills count="2"><fill><patternFill patternType="none"/></fill><fill><patternFill patternType="gray125"><fgColor rgb="FF808080"/><bgColor rgb="FFFFFFFF"/></patternFill></fill></fills><borders count="1"><border diagonalUp="1" diagonalDown="0"><left/><diagonal style="thin"><color rgb="FF9C0006"/></diagonal></border></borders><cellXfs count="2"><xf fillId="1" borderId="0"/><xf fillId="0" borderId="0"/></cellXfs></styleSheet>"#;
        let e = parse(xml);
        assert_eq!(e.patterns.len(), 2);
        assert!(e.patterns[0].is_none());
        let p = e.patterns[1].as_ref().unwrap();
        assert_eq!(p.kind, "gray125");
        assert_eq!(p.fg, Some([0x80, 0x80, 0x80]));
        assert_eq!(p.bg, Some([0xFF, 0xFF, 0xFF]));
        let d = e.diags[0].as_ref().unwrap();
        assert_eq!(d.style, "thin");
        assert_eq!(d.color, [0x9C, 0x00, 0x06]);
        assert!(d.up && !d.down);
        assert_eq!(e.xf_fill, vec![1, 0]);
    }
}
