//! 从 Excel 粘贴：读剪贴板里的 HTML 表格，连格式和合并一起带过来。
//!
//! Windows 剪贴板上，Excel 会把一块区域同时放成多种格式：纯文本(TSV)、
//! HTML、以及几种二进制格式。TSV 只有值；HTML 里有 `colspan/rowspan` 和
//! 内联样式（粗体、底色、字色、对齐、边框），够我们把"看起来一样"还原出来。
//! 公式不在任何可直接读取的格式里，所以保持"值"的语义。
//!
//! 直接调 Win32：项目里没有 windows crate，也不该为了几十行 API 引一整套。

pub mod delayed;
pub mod shortcut;

use std::collections::HashMap;
use std::ffi::c_void;

#[link(name = "user32")]
unsafe extern "system" {
    fn OpenClipboard(hwnd: *mut c_void) -> i32;
    fn CloseClipboard() -> i32;
    fn GetClipboardData(format: u32) -> *mut c_void;
    fn IsClipboardFormatAvailable(format: u32) -> i32;
    fn RegisterClipboardFormatW(name: *const u16) -> u32;
    #[cfg(not(test))]
    fn GetClipboardSequenceNumber() -> u32;
}

static LAST_INTERNAL_CLIP_SEQ: std::sync::atomic::AtomicU32 =
    std::sync::atomic::AtomicU32::new(0);

/// O(1) Win32 clipboard sequence counter (`GetClipboardSequenceNumber`), zero `OpenClipboard` lock.
#[inline]
pub fn clipboard_sequence_number() -> u32 {
    #[cfg(test)]
    {
        crate::TEST_CLIP_GEN.with(|seq| seq.get() as u32)
    }
    #[cfg(not(test))]
    unsafe {
        GetClipboardSequenceNumber()
    }
}

/// Record the current Win32 clipboard sequence number as owned by CELLAN's internal copy.
#[inline]
pub fn record_internal_clipboard_seq() {
    LAST_INTERNAL_CLIP_SEQ.store(
        clipboard_sequence_number(),
        std::sync::atomic::Ordering::Release,
    );
}

/// Returns `true` in O(1) when another process modified the system clipboard after CELLAN's last copy.
#[inline]
pub fn is_external_clipboard() -> bool {
    let cur = clipboard_sequence_number();
    let last = LAST_INTERNAL_CLIP_SEQ.load(std::sync::atomic::Ordering::Acquire);
    cur != 0 && cur != last
}

#[link(name = "user32")]
unsafe extern "system" {
    fn EmptyClipboard() -> i32;
    fn SetClipboardData(format: u32, mem: *mut c_void) -> *mut c_void;
}

#[link(name = "kernel32")]
unsafe extern "system" {
    fn GlobalLock(mem: *mut c_void) -> *mut c_void;
    fn GlobalUnlock(mem: *mut c_void) -> i32;
    fn GlobalSize(mem: *mut c_void) -> usize;
    fn GlobalAlloc(flags: u32, bytes: usize) -> *mut c_void;
    fn GlobalFree(mem: *mut c_void) -> *mut c_void;
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
        let slice = std::slice::from_raw_parts(ptr, size);
        let len = slice.iter().position(|&b| b == 0).unwrap_or(size);
        let buf = slice[..len].to_vec();
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

/// 剪贴板里有没有东西可粘贴（Excel 的「粘贴」按钮灰化条件）。
///
/// 只看格式是否可用，不读数据；每 500ms 最多查一次，避免每帧开剪贴板。
pub fn has_pasteable() -> bool {
    #[cfg(test)]
    {
        return false;
    }
    #[cfg(not(test))]
    {
        use std::sync::Mutex;
        use std::time::{Duration, Instant};
        static CACHE: Mutex<Option<(Instant, bool)>> = Mutex::new(None);
        let mut g = match CACHE.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        if let Some((t, v)) = *g {
            if t.elapsed() < Duration::from_millis(500) {
                return v;
            }
        }
        const CF_UNICODETEXT: u32 = 13;
        let mut ok = unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT) != 0 };
        if !ok {
            unsafe {
                let name = wide("XML Spreadsheet");
                let fmt = RegisterClipboardFormatW(name.as_ptr());
                ok = fmt != 0 && IsClipboardFormatAvailable(fmt) != 0;
            }
        }
        if !ok {
            unsafe {
                let name = wide("HTML Format");
                let fmt = RegisterClipboardFormatW(name.as_ptr());
                ok = fmt != 0 && IsClipboardFormatAvailable(fmt) != 0;
            }
        }
        *g = Some((Instant::now(), ok));
        ok
    }
}

pub fn has_xml_spreadsheet() -> bool {
    #[cfg(test)]
    { false }
    #[cfg(not(test))]
    unsafe {
        let format = RegisterClipboardFormatW(wide("XML Spreadsheet").as_ptr());
        format != 0 && IsClipboardFormatAvailable(format) != 0
    }
}

pub fn has_rich_table_format() -> bool {
    #[cfg(test)]
    {
        false
    }
    #[cfg(not(test))]
    unsafe {
        let xml = RegisterClipboardFormatW(wide("XML Spreadsheet").as_ptr());
        if xml != 0 && IsClipboardFormatAvailable(xml) != 0 {
            return true;
        }
        let html = RegisterClipboardFormatW(wide("HTML Format").as_ptr());
        html != 0 && IsClipboardFormatAvailable(html) != 0
    }
}

pub enum RichClipboard {
    Block(OfficeBlock),
    Sparse(sheetcore::clipboard_xmlss::SparseBlock),
}

/// 单次读取系统剪贴板富格式：优先一次性读取 `"XML Spreadsheet"` 并直接走流式稀疏解析
///（不再二次读剪贴板、不再构建双份稠密矩阵、超阈值时不跌落慢速 `parse_html`）；
/// 仅当剪贴板不存在 XMLSS 时才读取 `"HTML Format"`。
pub fn read_rich_table() -> Option<RichClipboard> {
    read_rich_table_ext(false)
}

pub fn read_rich_table_ext(values_only: bool) -> Option<RichClipboard> {
    #[cfg(test)]
    {
        let _ = values_only;
        return None;
    }
    #[cfg(not(test))]
    unsafe {
        let xml_fmt = RegisterClipboardFormatW(wide("XML Spreadsheet").as_ptr());
        if xml_fmt != 0 && IsClipboardFormatAvailable(xml_fmt) != 0 {
            if let Some(raw) = read_format_raw(xml_fmt) {
                let text = match std::str::from_utf8(&raw) {
                    Ok(s) => std::borrow::Cow::Borrowed(s),
                    Err(_) => String::from_utf8_lossy(&raw),
                };
                let xml = text.trim_end_matches('\0');
                if let Some(sparse) =
                    sheetcore::clipboard_xmlss::parse_sparse_ext(xml, values_only)
                {
                    if sparse.rows > 0 && sparse.cols > 0 {
                        return Some(RichClipboard::Sparse(sparse));
                    }
                }
                let parsed = sheetcore::clipboard_xmlss::parse(xml);
                if !parsed.is_empty() {
                    return Some(RichClipboard::Block(convert_block(parsed)));
                }
                // XMLSS existed on clipboard (e.g. giant malformed/empty export) — do not fall back
                // to slow HTML parsing when XMLSS already carried an explicit spreadsheet table.
                if contains_ascii_ci(xml, b"expandedrowcount=") {
                    return None;
                }
            }
        }
        read_html_table().map(RichClipboard::Block)
    }
}

/// Read raw `"XML Spreadsheet"` bytes once from the system clipboard so callers can invoke
/// zero-copy `sheetcore::clipboard_xmlss::parse_sparse_borrowed` / `apply_sparse_xmlss_to_sheet`.
pub fn read_raw_xmlss() -> Option<Vec<u8>> {
    #[cfg(test)]
    {
        None
    }
    #[cfg(not(test))]
    unsafe {
        let fmt = RegisterClipboardFormatW(wide("XML Spreadsheet").as_ptr());
        if fmt == 0 || IsClipboardFormatAvailable(fmt) == 0 {
            return None;
        }
        read_format_raw(fmt)
    }
}

#[cfg(not(test))]
unsafe fn read_html_table() -> Option<OfficeBlock> {
    unsafe {
        let fmt = RegisterClipboardFormatW(wide("HTML Format").as_ptr());
        if fmt == 0 || IsClipboardFormatAvailable(fmt) == 0 {
            return None;
        }
        let raw = read_format(fmt)?;
        let text = String::from_utf8_lossy(&raw);
        let html = match (text.find("<!--StartFragment"), text.rfind("<!--EndFragment")) {
            (Some(a), Some(b)) if b > a => &text[a..b],
            _ => text.as_ref(),
        };
        let block = parse_html(html);
        (!block.is_empty()).then_some(block)
    }
}

/// 读取超 50 万格或外部 Excel 整列（`ExpandedRowCount="1048576"`）的稀疏 XML Spreadsheet。
pub fn read_sparse_xmlss() -> Option<sheetcore::clipboard_xmlss::SparseBlock> {
    #[cfg(test)]
    {
        return None;
    }
    #[cfg(not(test))]
    unsafe {
        let name = wide("XML Spreadsheet");
        let fmt = RegisterClipboardFormatW(name.as_ptr());
        if fmt == 0 || IsClipboardFormatAvailable(fmt) == 0 {
            return None;
        }
        let raw = read_format_raw(fmt)?;
        let text = String::from_utf8_lossy(&raw);
        let xml = text.trim_end_matches('\0');
        sheetcore::clipboard_xmlss::parse_sparse(xml)
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
    #[cfg(not(test))]
    {
        if let Some(b) = read_xmlss() {
            if !b.is_empty() {
                return Some(b);
            }
        }
        unsafe { read_html_table() }
    }
}

#[inline]
fn starts_with_ascii_ci(bytes: &[u8], needle_lower: &[u8]) -> bool {
    bytes.len() >= needle_lower.len()
        && bytes[..needle_lower.len()]
            .iter()
            .zip(needle_lower)
            .all(|(&a, &b)| a.to_ascii_lowercase() == b)
}

#[inline]
fn find_ascii_ci(haystack: &str, needle_lower: &[u8]) -> Option<usize> {
    let hb = haystack.as_bytes();
    let n = needle_lower.len();
    if n == 0 {
        return Some(0);
    }
    if hb.len() < n {
        return None;
    }
    let first = needle_lower[0];
    let first_upper = first.to_ascii_uppercase();
    let mut i = 0usize;
    let limit = hb.len() - n;
    while i <= limit {
        let b = hb[i];
        if (b == first || b == first_upper) && starts_with_ascii_ci(&hb[i..], needle_lower) {
            return Some(i);
        }
        i += 1;
    }
    None
}

#[inline]
fn contains_ascii_ci(haystack: &str, needle_lower: &[u8]) -> bool {
    find_ascii_ci(haystack, needle_lower).is_some()
}

/// 解析 `<table>`：只认 tr/td，属性看 style 和 colspan/rowspan（字节级 ASCII 不区分大小写扫描，零全量拷贝）
fn parse_style_classes(html: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Some(a) = find_ascii_ci(html, b"<style") else {
        return map;
    };
    let Some(rel_b) = find_ascii_ci(&html[a..], b"</style>") else {
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
        let name = css[name_start..name_end].trim().to_ascii_lowercase();
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
    let classes = parse_style_classes(html);
    let mut row: Vec<OfficeCell> = Vec::new();
    let mut i = 0usize;
    let bytes = html.as_bytes();
    while i < bytes.len() {
        let Some(rel) = bytes[i..].iter().position(|&b| b == b'<') else {
            break;
        };
        i += rel;
        let rest = &bytes[i..];
        if starts_with_ascii_ci(rest, b"<tr") {
            if !row.is_empty() {
                block.rows.push(std::mem::take(&mut row));
            }
            i += 3;
        } else if starts_with_ascii_ci(rest, b"<td") || starts_with_ascii_ci(rest, b"<th") {
            let tag_end = bytes[i..]
                .iter()
                .position(|&b| b == b'>')
                .map(|p| i + p)
                .unwrap_or(bytes.len().saturating_sub(1));
            let attrs = &html[i..tag_end];
            let close = find_ascii_ci(&html[tag_end..], b"</td")
                .or_else(|| find_ascii_ci(&html[tag_end..], b"</th"))
                .map(|p| tag_end + p)
                .unwrap_or_else(|| html.len().min(tag_end + 1));
            let inner = if close > tag_end {
                &html[tag_end + 1..close]
            } else {
                ""
            };
            row.push(parse_cell(attrs, inner, &classes));
            i = (close + 5).min(bytes.len());
        } else if starts_with_ascii_ci(rest, b"</table") {
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
    if let Some(v) = attr_value(attrs, b"colspan").and_then(|s| s.parse::<u32>().ok()) {
        cell.colspan = v.max(1);
    }
    if let Some(v) = attr_value(attrs, b"rowspan").and_then(|s| s.parse::<u32>().ok()) {
        cell.rowspan = v.max(1);
    }
    let mut style = attr_value(attrs, b"style").unwrap_or_default();
    if let Some(class) = attr_value(attrs, b"class") {
        for name in class.split_whitespace() {
            if let Some(css) = classes.get(&name.to_ascii_lowercase()) {
                style.push(';');
                style.push_str(css);
            }
        }
    }
    let style = style.to_ascii_lowercase();
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
    if let Some(v) = attr_value(attrs, b"x:num") {
        cell.raw = Some(clean_text(&v));
    } else if contains_ascii_ci(attrs, b"x:str") {
        cell.raw = Some(clean_text(inner));
    }
    cell.text = clean_text(inner);
    cell
}

fn attr_value(attrs: &str, name_lower: &[u8]) -> Option<String> {
    let hb = attrs.as_bytes();
    let n = name_lower.len();
    if hb.len() <= n {
        return None;
    }
    let mut at = None;
    for i in 0..=(hb.len() - n - 1) {
        if hb[i + n] == b'=' && starts_with_ascii_ci(&hb[i..], name_lower) {
            at = Some(i);
            break;
        }
    }
    let at = at?;
    let rest = &attrs[at + n + 1..];
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
    fn xmlss_formula_uses_r1c1_for_excel_paste() {
        let mut wb = sheetcore::Workbook::new_blank();
        wb.sheets[0].set(
            0,
            0,
            sheetcore::Cell { value: sheetcore::Value::Number(5.0), formula: None },
        );
        wb.sheets[0].set(
            0,
            1,
            sheetcore::Cell { value: sheetcore::Value::Number(10.0), formula: Some("=A1*2".into()) },
        );
        let xml = build_xmlss(&wb, 0, (0, 0, 0, 1), &|_| "k".to_string());
        assert!(
            xml.contains(r#"ss:Formula="=RC[-1]*2""#),
            "XML Spreadsheet 必须写 R1C1 公式，Excel 才会按公式粘贴: {xml}"
        );
        assert!(!xml.contains(r#"ss:Formula="=A1*2""#), "不能写 A1 公式");
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
        let xml = text.trim_end_matches('\0');
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
        let buf = std::slice::from_raw_parts(ptr, size).to_vec();
        GlobalUnlock(handle);
        CloseClipboard();
        Some(buf)
    }
}

// ---------------------------------------------------------------------------
// 写出：让 Excel/WPS 粘贴时也能拿到公式、数字格式和日期类型
// ---------------------------------------------------------------------------

#[inline]
fn write_u64_010(dst: &mut [u8; 10], mut v: usize) {
    for i in (0..10).rev() {
        dst[i] = b'0' + (v % 10) as u8;
        v /= 10;
    }
}

/// `CF_HTML` 头：Excel/WPS 都按这四个偏移切片段，偏移是**字节**数（UTF-8）。
/// 单缓冲一次性预分配构造，消除中间 `String` 与 `format!` 二次整块拷贝。
fn cf_html(fragment: &str) -> Vec<u8> {
    const HEAD_TPL: &[u8] = b"Version:1.0\r\nStartHTML:0000000000\r\nEndHTML:0000000000\r\nStartFragment:0000000000\r\nEndFragment:0000000000\r\n";
    const DOC_HEAD: &[u8] = b"<html><head><meta http-equiv=\"Content-Type\" content=\"text/html; charset=utf-8\"></head><body>";
    const MARKER_S: &[u8] = b"<!--StartFragment-->";
    const MARKER_E: &[u8] = b"<!--EndFragment-->";
    const DOC_TAIL: &[u8] = b"</body></html>";

    let head_len = HEAD_TPL.len();
    let sf = head_len + DOC_HEAD.len() + MARKER_S.len();
    let ef = sf + fragment.len();
    let end = ef + MARKER_E.len() + DOC_TAIL.len();

    let mut out = Vec::with_capacity(end);
    out.extend_from_slice(HEAD_TPL);
    write_u64_010((&mut out[23..33]).try_into().unwrap(), head_len);
    write_u64_010((&mut out[43..53]).try_into().unwrap(), end);
    write_u64_010((&mut out[69..79]).try_into().unwrap(), sf);
    write_u64_010((&mut out[93..103]).try_into().unwrap(), ef);
    out.extend_from_slice(DOC_HEAD);
    out.extend_from_slice(MARKER_S);
    out.extend_from_slice(fragment.as_bytes());
    out.extend_from_slice(MARKER_E);
    out.extend_from_slice(DOC_TAIL);
    debug_assert_eq!(out.len(), end, "CF_HTML 偏移算错");
    out
}
fn alloc_global(bytes: &[u8]) -> *mut c_void {
    const GMEM_MOVEABLE: u32 = 0x0002;
    unsafe {
        let h = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1));
        if h.is_null() {
            return h;
        }
        let p = GlobalLock(h) as *mut u8;
        if p.is_null() {
            GlobalFree(h);
            return std::ptr::null_mut();
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
        GlobalUnlock(h);
        h
    }
}

/// 一次写入三种格式：XML Spreadsheet（公式/类型/格式）、HTML（底色/合并）、纯文本。
///
/// arboard 只能写 HTML/文本，没有 XMLSS flavor；Excel 粘 HTML 时公式会退化成值，
/// 所以这里直接走 Win32，三种格式同一时刻落到剪贴板上。
pub fn write_office_clipboard(xmlss: &str, html: &str, text: &str) -> bool {
    if cfg!(test) { return false; }
    write_office_clipboard_native(xmlss, html, text)
}

fn write_office_clipboard_native(xmlss: &str, html: &str, text: &str) -> bool {
    use windows_sys::Win32::UI::{Input::KeyboardAndMouse::GetActiveWindow, WindowsAndMessaging::*};
    // EmptyClipboard with a null opener leaves no owner for SetClipboardData.
    // Background copies (including private-desktop tests) use a message-only
    // window for the duration of the write.
    struct TemporaryWindow(*mut c_void);
    impl Drop for TemporaryWindow {
        fn drop(&mut self) {
            if !self.0.is_null() { unsafe { DestroyWindow(self.0); } }
        }
    }
    let mut temporary = TemporaryWindow(std::ptr::null_mut());
    let mut window = unsafe { GetActiveWindow() };
    if window.is_null() {
        window = unsafe { CreateWindowExW(0, wide("STATIC").as_ptr(), std::ptr::null(), 0,
            0, 0, 0, 0, HWND_MESSAGE, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null()) };
        if window.is_null() { return false; }
        temporary.0 = window;
    }
    let mut xmlss_bytes = xmlss.as_bytes().to_vec();
    xmlss_bytes.push(0);
    let mut text_utf16: Vec<u8> = Vec::with_capacity(text.len() * 2 + 2);
    for u in text.encode_utf16() {
        text_utf16.extend_from_slice(&u.to_le_bytes());
    }
    text_utf16.extend_from_slice(&[0, 0]);
    let html_bytes = cf_html(html);
    // WPS 看到 HTML 会优先走它并丢弃 x:fmla 公式，因此带公式选区省略 CF_HTML 让 Excel/WPS
    // 走首位的 XML Spreadsheet 通路；但始终保留 CF_UNICODETEXT（排在 XMLSS 之后），
    // 确保粘贴到微信/记事本/Word 等纯文本应用时仍能得到单元格显示值而非空白。
    let has_formula = xmlss.contains("ss:Formula=");
    let include_html = !has_formula;
    let include_text = true;

    const CF_UNICODETEXT: u32 = 13;
    unsafe {
        // 别的进程/本进程的 arboard 实例可能正开着剪贴板：重试几次再放弃。
        let mut opened = false;
        for _ in 0..8 {
            if OpenClipboard(window) != 0 {
                opened = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        if !opened {
            if std::env::var("SC_CLIP_LOG").is_ok() {
                eprintln!("[clip] OpenClipboard 失败，错误 {}", std::io::Error::last_os_error());
            }
            return false;
        }
        if EmptyClipboard() == 0 { CloseClipboard(); return false; }
        let mut ok = true;
        let mut put = |fmt: u32, bytes: &[u8]| {
            let h = alloc_global(bytes);
            if h.is_null() || SetClipboardData(fmt, h).is_null() {
                if !h.is_null() {
                    GlobalFree(h);
                }
                ok = false;
            }
        };
        let xml_fmt = RegisterClipboardFormatW(wide("XML Spreadsheet").as_ptr());
        if xml_fmt != 0 {
            put(xml_fmt, &xmlss_bytes);
        }
        let html_fmt = RegisterClipboardFormatW(wide("HTML Format").as_ptr());
        if include_html && html_fmt != 0 {
            put(html_fmt, &html_bytes);
        }
        if include_text {
            put(CF_UNICODETEXT, &text_utf16);
        }
        if ok {
            record_internal_clipboard_seq();
        }
        CloseClipboard();
        ok
    }
}

/// 把选区写成 XML Spreadsheet 文本（公式转 R1C1，日期写 DateTime，文本保前导零）。
pub fn selection_xmlss(wb: &sheetcore::Workbook, si: usize, rect: (u32, u32, u32, u32)) -> String {
    build_xmlss_rows(wb, si, rect, None, &|st| format!("{}|{}|{}|{}|{:?}|{:?}|{}",
        st.font, st.size, st.bold, st.italic, st.color, st.fill, st.format))
}

/// 把筛选态可见行压紧写成稀疏 XML Spreadsheet 文本（保留公式、样式、数字格式与前导零）。
pub fn selection_xmlss_filtered(
    wb: &sheetcore::Workbook,
    si: usize,
    rect: (u32, u32, u32, u32),
    visible_rows: &[u32],
) -> String {
    build_xmlss_rows(wb, si, rect, Some(visible_rows), &|st| format!("{}|{}|{}|{}|{:?}|{:?}|{}",
        st.font, st.size, st.bold, st.italic, st.color, st.fill, st.format))
}

pub(crate) use sheetcore::clipboard_xmlss::sparse_column_xmlss;

pub fn build_xmlss(
    wb: &sheetcore::Workbook,
    si: usize,
    rect: (u32, u32, u32, u32),
    style_key: &dyn Fn(&sheetcore::style::CellStyle) -> String,
) -> String {
    build_xmlss_rows(wb, si, rect, None, style_key)
}

pub fn build_xmlss_rows(
    wb: &sheetcore::Workbook,
    si: usize,
    rect: (u32, u32, u32, u32),
    visible_rows: Option<&[u32]>,
    style_key: &dyn Fn(&sheetcore::style::CellStyle) -> String,
) -> String {
    use std::collections::{BTreeMap, BTreeSet};
    use std::fmt::Write as _;
    let (r1, c1, r2, c2) = rect;
    let sh = wb.sheet(si);
    let mut ids: BTreeMap<String, usize> = BTreeMap::new();
    let mut style_xml = String::new();
    let mut style_ids: HashMap<u16, usize> = HashMap::new();
    let merges: Vec<_> = if visible_rows.is_some() {
        Vec::new()
    } else {
        sh.merges
            .iter()
            .copied()
            .filter(|&(a, b, c, d)| a >= r1 && b >= c1 && c <= r2 && d <= c2)
            .collect()
    };
    let mut merge_index = sheetcore::merges::MergeIndex::default();
    merge_index.refresh(&merges);

    let mut style_idx_of = |style: u16, ids: &mut BTreeMap<String, usize>| -> usize {
        if let Some(&idx) = style_ids.get(&style) {
            return idx;
        }
        let st = wb.styles.get(style);
        let key = style_key(st);
        let idx = if let Some(&i) = ids.get(&key) {
            i
        } else {
            let i = ids.len();
            ids.insert(key, i);
            let font = if st.font.is_empty() { "等线" } else { st.font.as_str() };
            let size = if st.size > 0.0 { st.size } else { 11.0 };
            let bold = if st.bold { " ss:Bold=\"1\"" } else { "" };
            let italic = if st.italic { " ss:Italic=\"1\"" } else { "" };
            let c = st.color;
            let _ = write!(style_xml, "<Style ss:ID=\"sc{i}\"><Alignment ss:Vertical=\"Center\"/><Font ss:FontName=\"");
            xml_escape_into(&mut style_xml, font);
            let _ = write!(
                style_xml,
                "\" ss:Size=\"{size}\"{bold}{italic} ss:Color=\"#{:02X}{:02X}{:02X}\"/>",
                c[0], c[1], c[2]
            );
            if let Some(f) = st.fill {
                let _ = write!(
                    style_xml,
                    "<Interior ss:Color=\"#{:02X}{:02X}{:02X}\" ss:Pattern=\"Solid\"/>",
                    f[0], f[1], f[2]
                );
            }
            if !st.format.trim().is_empty() {
                style_xml.push_str("<NumberFormat ss:Format=\"");
                xml_escape_into(&mut style_xml, &st.format);
                style_xml.push_str("\"/>");
            }
            style_xml.push_str("</Style>");
            i
        };
        style_ids.insert(style, idx);
        idx
    };

    // Pre-cache whether a style format looks like a date so we don't allocate `.to_lowercase()` per numeric cell.
    let mut date_style_cache: HashMap<u16, bool> = HashMap::new();
    let mut is_date_style = |style: u16| -> bool {
        *date_style_cache.entry(style).or_insert_with(|| {
            let fmt = &wb.styles.get(style).format;
            fmt.bytes().any(|b| matches!(b, b'y' | b'Y')) && (fmt.contains('-') || fmt.contains('/'))
        })
    };

    let has_col_style = (c1..=c2).any(|c| sh.col_styles.get(&c).copied().unwrap_or(0) != 0);
    let area = u64::from(r2.saturating_sub(r1) + 1) * u64::from(c2.saturating_sub(c1) + 1);
    let sparse_active_rows: Option<BTreeSet<u32>> = if !has_col_style && area > 8_192 {
        let mut set = BTreeSet::new();
        for (key, _) in sh.iter_rect(rect) {
            set.insert(key.row);
        }
        for (key, sid) in sh.styles.iter_rect(rect) {
            if sid != 0 {
                set.insert(key.row);
            }
        }
        for (&r, &sid) in &sh.row_styles {
            if sid != 0 && r >= r1 && r <= r2 {
                set.insert(r);
            }
        }
        for &(a, _, _, _) in &merges {
            set.insert(a);
        }
        Some(set)
    } else {
        None
    };

    let est_rows = match (&visible_rows, &sparse_active_rows) {
        (Some(v), _) => v.len(),
        (None, Some(s)) => s.len(),
        (None, None) => (r2 - r1 + 1) as usize,
    };
    let mut body = String::with_capacity(est_rows.saturating_mul(64).clamp(1024, 8 * 1024 * 1024));
    let mut expected_out_row: usize = 0;

    let mut emit_one_row = |out_row: usize, r: u32| {
        if let Some(ref active) = sparse_active_rows {
            if !active.contains(&r) {
                return;
            }
        }
        let mut row_started = false;
        let mut expected_col = c1;
        for c in c1..=c2 {
            let merge = merge_index.at(r, c);
            if merge.is_some_and(|(a, b, _, _)| (r, c) != (a, b)) {
                continue;
            }
            let cell_opt = sh.get(r, c);
            let style = sh.style_of(r, c);
            let has_formula = cell_opt.as_deref().and_then(|cell| cell.formula.as_deref()).is_some();
            let has_value = cell_opt.as_deref().is_some_and(|cell| !matches!(cell.value, sheetcore::Value::Empty));
            if !has_formula && !has_value && style == 0 && merge.is_none() {
                continue;
            }
            if !row_started {
                if out_row == expected_out_row {
                    body.push_str("<Row>");
                } else {
                    let _ = write!(body, "<Row ss:Index=\"{}\">", out_row + 1);
                }
                row_started = true;
            }
            let sid_idx = style_idx_of(style, &mut ids);
            body.push_str("<Cell");
            if c != expected_col {
                let _ = write!(body, " ss:Index=\"{}\"", c - c1 + 1);
            }
            let _ = write!(body, " ss:StyleID=\"sc{sid_idx}\"");
            let mut span_cols = 1u32;
            if let Some((a, b, cc, dd)) = merge {
                if dd > b {
                    let across = dd - b;
                    span_cols = across + 1;
                    let _ = write!(body, " ss:MergeAcross=\"{across}\"");
                }
                if cc > a {
                    let _ = write!(body, " ss:MergeDown=\"{}\"", cc - a);
                }
            }
            expected_col = c + span_cols;

            if let Some(f) = cell_opt.as_deref().and_then(|cell| cell.formula.as_deref()) {
                let r1c1 = sheetcore::clipboard_xmlss::a1_to_r1c1(f, r, c);
                body.push_str(" ss:Formula=\"");
                xml_escape_into(&mut body, &r1c1);
                body.push('"');
            }

            match cell_opt.as_deref().map(|cell| &cell.value) {
                None | Some(sheetcore::Value::Empty) => {
                    body.push_str("/>");
                }
                Some(sheetcore::Value::Text(t)) if t.is_empty() => {
                    body.push_str("/>");
                }
                Some(sheetcore::Value::Text(t)) => {
                    body.push_str("><Data ss:Type=\"String\">");
                    xml_escape_into(&mut body, t);
                    body.push_str("</Data></Cell>");
                }
                Some(sheetcore::Value::Number(n)) => {
                    let push_num = |b: &mut String, val: f64| {
                        b.push_str("><Data ss:Type=\"Number\">");
                        let val = if val == 0.0 { 0.0 } else { val };
                        let iv = val as i64;
                        if (iv as f64) == val && iv.unsigned_abs() < 10_000_000_000_000_000 {
                            let _ = std::fmt::Write::write_fmt(b, format_args!("{iv}"));
                        } else {
                            // SAFETY: `serde_json::to_writer` appends valid ASCII decimal bytes.
                            let _ = serde_json::to_writer(unsafe { b.as_mut_vec() }, &val);
                        }
                        b.push_str("</Data></Cell>");
                    };
                    if is_date_style(style) {
                        if let Some((y, m, d)) = wb.serial_to_date(*n) {
                            let _ = write!(
                                body,
                                "><Data ss:Type=\"DateTime\">{y:04}-{m:02}-{d:02}T00:00:00.000</Data></Cell>"
                            );
                        } else {
                            push_num(&mut body, *n);
                        }
                    } else {
                        push_num(&mut body, *n);
                    }
                }
                Some(sheetcore::Value::Bool(b)) => {
                    body.push_str(if *b {
                        "><Data ss:Type=\"Boolean\">1</Data></Cell>"
                    } else {
                        "><Data ss:Type=\"Boolean\">0</Data></Cell>"
                    });
                }
                Some(sheetcore::Value::Error(e)) => {
                    body.push_str("><Data ss:Type=\"Error\">");
                    xml_escape_into(&mut body, e.text());
                    body.push_str("</Data></Cell>");
                }
            }
        }
        if row_started {
            body.push_str("</Row>");
            expected_out_row = out_row + 1;
        }
    };

    let out_row_count = if let Some(rows) = visible_rows {
        for (out_row, &r) in rows.iter().enumerate() {
            emit_one_row(out_row, r);
        }
        rows.len()
    } else if let Some(ref active) = sparse_active_rows {
        for &r in active {
            emit_one_row((r - r1) as usize, r);
        }
        (r2 - r1 + 1) as usize
    } else {
        for r in r1..=r2 {
            emit_one_row((r - r1) as usize, r);
        }
        (r2 - r1 + 1) as usize
    };

    let cols = c2 - c1 + 1;
    let rows = out_row_count;
    let mut out = String::with_capacity(512 + style_xml.len() + body.len());
    out.push_str(
        "<?xml version=\"1.0\"?>\n\
<?mso-application progid=\"Excel.Sheet\"?>\n\
<Workbook xmlns=\"urn:schemas-microsoft-com:office:spreadsheet\"\n\
 xmlns:o=\"urn:schemas-microsoft-com:office:office\"\n\
 xmlns:x=\"urn:schemas-microsoft-com:office:excel\"\n\
 xmlns:ss=\"urn:schemas-microsoft-com:office:spreadsheet\"\n\
 xmlns:html=\"http://www.w3.org/TR/REC-html40\">\n\
<Styles><Style ss:ID=\"Default\" ss:Name=\"Normal\"><Alignment ss:Vertical=\"Center\"/><Borders/><Font ss:FontName=\"等线\" ss:Size=\"11\"/><Interior/><NumberFormat/><Protection/></Style>",
    );
    out.push_str(&style_xml);
    out.push_str("</Styles>\n<Worksheet ss:Name=\"");
    xml_escape_into(&mut out, &sh.name);
    let _ = write!(
        out,
        "\"><Table ss:ExpandedColumnCount=\"{cols}\" ss:ExpandedRowCount=\"{rows}\">"
    );
    out.push_str(&body);
    out.push_str("</Table></Worksheet>\n</Workbook>");
    out
}

fn xml_escape_into(out: &mut String, s: &str) {
    let bytes = s.as_bytes();
    let mut last = 0usize;
    for (i, &b) in bytes.iter().enumerate() {
        let rep = match b {
            b'&' => "&amp;",
            b'<' => "&lt;",
            b'>' => "&gt;",
            b'"' => "&quot;",
            _ => continue,
        };
        if i > last {
            out.push_str(&s[last..i]);
        }
        out.push_str(rep);
        last = i + 1;
    }
    if last < s.len() {
        out.push_str(&s[last..]);
    }
}

fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    xml_escape_into(&mut out, s);
    out
}
