//! 从 Excel 粘贴：读剪贴板里的 HTML 表格，连格式和合并一起带过来。
//!
//! Windows 剪贴板上，Excel 会把一块区域同时放成多种格式：纯文本(TSV)、
//! HTML、以及几种二进制格式。TSV 只有值；HTML 里有 `colspan/rowspan` 和
//! 内联样式（粗体、底色、字色、对齐、边框），够我们把"看起来一样"还原出来。
//! 公式不在任何可直接读取的格式里，所以保持"值"的语义。
//!
//! 直接调 Win32：项目里没有 windows crate，也不该为了几十行 API 引一整套。

use std::collections::HashMap;
use std::ffi::c_void;

#[link(name = "user32")]
unsafe extern "system" {
    fn OpenClipboard(hwnd: *mut c_void) -> i32;
    fn CloseClipboard() -> i32;
    fn GetClipboardData(format: u32) -> *mut c_void;
    fn IsClipboardFormatAvailable(format: u32) -> i32;
    fn RegisterClipboardFormatW(name: *const u16) -> u32;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GlobalLock(mem: *mut c_void) -> *mut c_void;
    fn GlobalUnlock(mem: *mut c_void) -> i32;
    fn GlobalSize(mem: *mut c_void) -> usize;
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 读某个已注册格式的原始字节
fn read_format(fmt: u32) -> Option<Vec<u8>> {
    if fmt == 0 {
        return None;
    }
    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return None;
        }
        let handle = GetClipboardData(fmt);
        if handle.is_null() {
            CloseClipboard();
            return None;
        }
        let ptr = GlobalLock(handle) as *const u8;
        if ptr.is_null() {
            CloseClipboard();
            return None;
        }
        // GlobalSize 给的是分配大小，HTML Format 结尾有 \0，按字符串截断
        let size = GlobalSize(handle);
        let mut buf = Vec::with_capacity(size);
        for i in 0..size {
            let b = *ptr.add(i);
            if b == 0 {
                break;
            }
            buf.push(b);
        }
        GlobalUnlock(handle);
        CloseClipboard();
        Some(buf)
    }
}

#[derive(Clone, Debug, Default)]
pub struct OfficeCell {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub fill: Option<[u8; 3]>,
    pub color: Option<[u8; 3]>,
    /// 0 常规 1 左 2 中 3 右
    pub halign: u8,
    pub colspan: u32,
    pub rowspan: u32,
    /// 有边框（简化为细实线，颜色取解析到的或黑）
    pub border: Option<[u8; 3]>,
    /// 公式（A1 形式、带 `=`）；来自 XML Spreadsheet 那条路径
    pub formula: Option<String>,
    /// 缓存值类型：0 数字 1 文本 2 布尔 3 日期
    pub kind: u8,
    pub font: String,
    pub size: f32,
    pub num_format: String,
    /// 原始值（HTML 的 x:num/x:str），text 保留显示文本用于比对
    pub raw: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct OfficeBlock {
    pub rows: Vec<Vec<OfficeCell>>,
}

impl OfficeBlock {
    pub fn cols(&self) -> usize {
        self.rows.iter().map(|r| r.len()).max().unwrap_or(0)
    }
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty() || self.cols() == 0
    }
}

/// 从系统剪贴板读 Excel 区域。
///
/// 优先 **XML Spreadsheet**：它带公式（R1C1）、值类型、字体名/字号/数字格式；
/// 没有这个格式（非 Excel 来源）才退回 HTML（带合并和底色，但没有公式）。
pub fn read_table() -> Option<OfficeBlock> {
    #[cfg(test)]
    {
        // 单测不读机器上的真实剪贴板；需要富格式时直接测 parse_html/convert_block。
        return None;
    }
    if let Some(b) = read_xmlss() {
        if !b.is_empty() {
            return Some(b);
        }
    }
    unsafe {
        let fmt = {
            let name = wide("HTML Format");
            RegisterClipboardFormatW(name.as_ptr())
        };
        if fmt == 0 || IsClipboardFormatAvailable(fmt) == 0 {
            return None;
        }
        let raw = read_format(fmt)?;
        let text = String::from_utf8_lossy(&raw).to_string();
        let html = match (text.find("<!--StartFragment"), text.rfind("<!--EndFragment")) {
            (Some(a), Some(b)) if b > a => &text[a..b],
            _ => text.as_str(),
        };
        let block = parse_html(html);
        if block.is_empty() {
            None
        } else {
            Some(block)
        }
    }
}

/// 解析 `<table>`：只认 tr/td，属性看 style 和 colspan/rowspan
fn parse_style_classes(lower: &str, html: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Some(a) = lower.find("<style") else {
        return map;
    };
    let Some(rel_b) = lower[a..].find("</style>") else {
        return map;
    };
    let css = &html[a..a + rel_b];
    let mut i = 0usize;
    while let Some(rel) = css[i..].find('.') {
        let at = i + rel;
        let name_start = at + 1;
        let name_end = css[name_start..]
            .find(|c: char| c.is_whitespace() || c == '{' || c == ',')
            .map(|p| name_start + p)
            .unwrap_or(css.len());
        let name = css[name_start..name_end].trim().to_lowercase();
        if name.is_empty() {
            i = at + 1;
            continue;
        }
        let Some(brace_rel) = css[name_end..].find('{') else {
            break;
        };
        let brace = name_end + brace_rel;
        let Some(close_rel) = css[brace..].find('}') else {
            break;
        };
        let close = brace + close_rel;
        map.insert(name, css[brace + 1..close].trim().to_string());
        i = close + 1;
    }
    map
}

/// CSS 声明里的原始值：`mso-number-format:"yyyy\\-mm\\-dd"` → `yyyy-mm-dd`
fn style_value(style: &str, prop: &str) -> Option<String> {
    let at = style.find(prop)?;
    let rest = style[at + prop.len()..].trim_start_matches([':', ' ']);
    let value: String = rest.chars().take_while(|c| *c != ';' && *c != '}').collect();
    let value = value.trim().trim_matches(['"', '\'']).replace('\\', "");
    if value.is_empty() {
        None
    } else {
        Some(value)
    }
}

/// 解码 HTML 数字实体（WPS 会把中文写成 `&#31532;`）。
fn decode_entities(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '&' {
            out.push(c);
            continue;
        }
        let mut ent = String::new();
        while let Some(&n) = chars.peek() {
            if n == ';' || ent.len() > 12 {
                break;
            }
            ent.push(n);
            chars.next();
        }
        if chars.peek() == Some(&';') {
            chars.next();
            let decoded = if let Some(hex) = ent.strip_prefix("#x").or_else(|| ent.strip_prefix("#X")) {
                u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
            } else if let Some(dec) = ent.strip_prefix('#') {
                dec.parse::<u32>().ok().and_then(char::from_u32)
            } else {
                match ent.as_str() {
                    "nbsp" => Some(' '),
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    "quot" => Some('"'),
                    "apos" => Some('\''),
                    "amp" => Some('&'),
                    _ => None,
                }
            };
            if let Some(ch) = decoded {
                out.push(ch);
            } else {
                out.push('&');
                out.push_str(&ent);
                out.push(';');
            }
        } else {
            out.push('&');
            out.push_str(&ent);
        }
    }
    out
}

pub fn parse_html(html: &str) -> OfficeBlock {
    let mut block = OfficeBlock::default();
    let lower = html.to_lowercase();
    let classes = parse_style_classes(&lower, html);
    let mut row: Vec<OfficeCell> = Vec::new();
    let mut i = 0usize;
    let bytes = lower.as_bytes();
    while i < bytes.len() {
        let at = |pat: &str| lower.get(i..).map(|s| s.starts_with(pat)).unwrap_or(false);
        if at("<tr") {
            if !row.is_empty() {
                block.rows.push(std::mem::take(&mut row));
            }
            i += 3;
        } else if at("<td") || at("<th") {
            let tag_end = lower[i..].find('>').map(|p| i + p).unwrap_or(bytes.len() - 1);
            let attrs = &html[i..tag_end];
            let close = lower[tag_end..]
                .find("</td")
                .or_else(|| lower[tag_end..].find("</th"))
                .map(|p| tag_end + p)
                .unwrap_or_else(|| html.len().min(tag_end + 1));
            let inner = if close > tag_end {
                &html[tag_end + 1..close]
            } else {
                ""
            };
            row.push(parse_cell(attrs, inner, &classes));
            i = close + 5;
        } else if at("</table") {
            break;
        } else {
            i += 1;
        }
    }
    if !row.is_empty() {
        block.rows.push(row);
    }
    // 丢掉全空的行（Excel 的 HTML 有时带一行空表头）
    block.rows.retain(|r| !r.iter().all(|c| c.text.is_empty()));
    block
}

fn parse_cell(attrs: &str, inner: &str, classes: &HashMap<String, String>) -> OfficeCell {
    let mut cell = OfficeCell { colspan: 1, rowspan: 1, ..Default::default() };
    if let Some(v) = attr_value(attrs, "colspan").and_then(|s| s.parse::<u32>().ok()) {
        cell.colspan = v.max(1);
    }
    if let Some(v) = attr_value(attrs, "rowspan").and_then(|s| s.parse::<u32>().ok()) {
        cell.rowspan = v.max(1);
    }
    let mut style = attr_value(attrs, "style").unwrap_or_default();
    if let Some(class) = attr_value(attrs, "class") {
        for name in class.split_whitespace() {
            if let Some(css) = classes.get(&name.to_lowercase()) {
                style.push(';');
                style.push_str(css);
            }
        }
    }
    let style = style.to_lowercase();
    cell.bold = style.contains("font-weight:700")
        || style.contains("font-weight:bold")
        || style.contains("font-weight: 700")
        || style.contains("font-weight: bold");
    cell.italic = style.contains("font-style:italic") || style.contains("font-style: italic");
    cell.fill = style_color(&style, "background-color").or_else(|| style_color(&style, "background"));
    cell.color = style_color(&style, "color");
    cell.halign = if style.contains("text-align:center") || style.contains("text-align: center") {
        2
    } else if style.contains("text-align:right") || style.contains("text-align: right") {
        3
    } else if style.contains("text-align:left") || style.contains("text-align: left") {
        1
    } else {
        0
    };
    if style.contains("border") && !style.contains("border:none") && !style.contains("border: none") {
        cell.border = Some(style_color(&style, "border-color").unwrap_or([0, 0, 0]));
    }
    if let Some(nf) = style_value(&style, "mso-number-format") {
        cell.num_format = nf;
    }
    if let Some(v) = attr_value(attrs, "x:num") {
        cell.raw = Some(clean_text(&v));
    } else if attrs.to_lowercase().contains("x:str") {
        cell.raw = Some(clean_text(inner));
    }
    cell.text = clean_text(inner);
    cell
}

fn attr_value(attrs: &str, name: &str) -> Option<String> {
    let lower = attrs.to_lowercase();
    let pat = format!("{name}=");
    let at = lower.find(&pat)?;
    let rest = &attrs[at + pat.len()..];
    let rest = rest.trim_start();
    if let Some(stripped) = rest.strip_prefix('"') {
        Some(stripped[..stripped.find('"')?].to_string())
    } else if let Some(stripped) = rest.strip_prefix('\'') {
        Some(stripped[..stripped.find('\'')?].to_string())
    } else {
        Some(rest.split_whitespace().next()?.to_string())
    }
}

/// style 串里取颜色：支持 #rrggbb / rgb(r,g,b) / 命名色少数几种
fn style_color(style: &str, prop: &str) -> Option<[u8; 3]> {
    let at = style.find(prop)?;
    let rest = &style[at + prop.len()..];
    let rest = rest.trim_start_matches([':', ' ']);
    let value: String = rest.chars().take_while(|c| *c != ';').collect();
    let v = value.trim();
    if let Some(hex) = v.strip_prefix('#') {
        let hex = hex.trim();
        if hex.len() >= 6 {
            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
            let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
            return Some([r, g, b]);
        }
    }
    if let Some(inner) = v.strip_prefix("rgb(") {
        let nums: Vec<u8> = inner
            .trim_end_matches(')')
            .split(',')
            .filter_map(|x| x.trim().parse().ok())
            .collect();
        if nums.len() == 3 {
            return Some([nums[0], nums[1], nums[2]]);
        }
    }
    match v {
        "red" => Some([255, 0, 0]),
        "black" => Some([0, 0, 0]),
        "white" => Some([255, 255, 255]),
        "yellow" => Some([255, 255, 0]),
        "green" => Some([0, 128, 0]),
        "blue" => Some([0, 0, 255]),
        _ => None,
    }
}

/// 去掉标签、还原实体、合并空白
fn clean_text(inner: &str) -> String {
    let mut out = String::new();
    let mut depth = 0i32;
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '<' {
            depth += 1;
            // <br> 当换行
            let mut tag = String::new();
            while let Some(&n) = chars.peek() {
                tag.push(n);
                chars.next();
                if n == '>' {
                    break;
                }
            }
            if tag.to_lowercase().starts_with("br") {
                out.push('\n');
            }
            depth -= 1;
            continue;
        }
        if depth == 0 {
            out.push(c);
        }
    }
    let unescaped = decode_entities(&out);
    unescaped.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_excel_html_table() {
        let html = r#"<table>
            <tr><td style="font-weight:700;background-color:#FFFF00" colspan="2">合计</td><td>3</td></tr>
            <tr><td>甲</td><td>1</td><td style="text-align:right;color:#FF0000">2</td></tr>
        </table>"#;
        let b = parse_html(html);
        assert_eq!(b.rows.len(), 2);
        assert_eq!(b.rows[0][0].text, "合计");
        assert!(b.rows[0][0].bold, "粗体要认出来");
        assert_eq!(b.rows[0][0].fill, Some([255, 255, 0]), "#FFFF00 → 黄底");
        assert_eq!(b.rows[0][0].colspan, 2, "合并列");
        assert_eq!(b.rows[1][2].halign, 3, "右对齐");
        assert_eq!(b.rows[1][2].color, Some([255, 0, 0]));
    }

    #[test]
    fn parses_wps_class_styles_and_numeric_entities() {
        let html = r#"<html><head><style>
.et2 {font-weight:700; mso-number-format:'yyyy\-mm\-dd'; background:#FFFF00;}
</style></head><body><table>
<tr><td class=et2 x:num='46024'>2026-01-02</td></tr>
<tr><td x:str>&#31532;&#19968;&#34892;<br>&#31532;&#20108;&#34892;</td></tr>
</table></body></html>"#;
        let b = parse_html(html);
        assert!(b.rows[0][0].bold, "class 里的粗体");
        assert_eq!(b.rows[0][0].fill, Some([255, 255, 0]), "class 里的底色");
        assert_eq!(b.rows[0][0].num_format, "yyyy-mm-dd");
        assert_eq!(b.rows[0][0].raw.as_deref(), Some("46024"));
        assert_eq!(b.rows[1][0].text, "第一行\n第二行", "数字实体 + <br>");
        assert_eq!(b.rows[1][0].raw.as_deref(), Some("第一行\n第二行"));
    }

    #[test]
    fn entities_and_breaks() {
        let b = parse_html("<table><tr><td>a&amp;b<br>c</td></tr></table>");
        assert_eq!(b.rows[0][0].text, "a&b\nc");
    }

    #[test]
    fn ignores_empty_rows() {
        let b = parse_html("<table><tr><td></td></tr><tr><td>x</td></tr></table>");
        assert_eq!(b.rows.len(), 1, "全空行应被丢弃");
    }

    #[test]
    fn rgb_notation() {
        assert_eq!(style_color("background-color:rgb(1,2,3)", "background-color"), Some([1, 2, 3]));
    }
}

/// 读 "XML Spreadsheet"（Excel 复制时总会放一份），解析成我们的块
fn read_xmlss() -> Option<OfficeBlock> {
    unsafe {
        let name = wide("XML Spreadsheet");
        let fmt = RegisterClipboardFormatW(name.as_ptr());
        if fmt == 0 || IsClipboardFormatAvailable(fmt) == 0 {
            return None;
        }
        let raw = read_format_raw(fmt)?;
        let text = String::from_utf8_lossy(&raw);
        let xml = text.trim_end_matches(' ');
        let parsed = sheetcore::clipboard_xmlss::parse(xml);
        if parsed.is_empty() {
            return None;
        }
        Some(convert_block(parsed))
    }
}

pub(crate) fn convert_block(b: sheetcore::clipboard_xmlss::Block) -> OfficeBlock {
    use sheetcore::clipboard_xmlss::Kind;
    let mut rows = Vec::with_capacity(b.rows.len());
    for row in b.rows {
        let mut out = Vec::with_capacity(row.len());
        for c in row {
            let kind = match c.kind {
                Kind::Number => 0,
                Kind::Text => 1,
                Kind::Bool => 2,
                Kind::DateTime => 3,
                // 4 = 错误值（不能被当成数字 0，否则粘进来变成 0）
                Kind::Error => 4,
            };
            out.push(OfficeCell {
                text: c.text,
                bold: c.style.bold,
                italic: c.style.italic,
                fill: c.style.fill,
                color: c.style.color,
                halign: c.style.halign,
                colspan: c.colspan.max(1),
                rowspan: c.rowspan.max(1),
                border: c.style.border,
                formula: c.formula,
                kind,
                font: c.style.font,
                size: c.style.size,
                num_format: c.style.num_format,
                raw: None,
            });
        }
        rows.push(out);
    }
    OfficeBlock { rows }
}

/// 读整块内存（二进制格式不能按 NUL 截断）
fn read_format_raw(fmt: u32) -> Option<Vec<u8>> {
    if fmt == 0 {
        return None;
    }
    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return None;
        }
        let handle = GetClipboardData(fmt);
        if handle.is_null() {
            CloseClipboard();
            return None;
        }
        let ptr = GlobalLock(handle) as *const u8;
        if ptr.is_null() {
            CloseClipboard();
            return None;
        }
        let size = GlobalSize(handle);
        let mut buf = Vec::with_capacity(size);
        for i in 0..size {
            buf.push(*ptr.add(i));
        }
        GlobalUnlock(handle);
        CloseClipboard();
        Some(buf)
    }
}
