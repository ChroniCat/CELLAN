//! 解析剪贴板上的 **XML Spreadsheet**（SpreadsheetML 2003）。
//!
//! Excel 复制区域时除了 HTML，还会放一份这个格式：里面有公式（R1C1）、缓存值、
//! 数据类型，以及字体名/字号/边框/数字格式——比 HTML 那条路拿到的还全。
//!
//! 实测样本（复制 A1:B2，B 列两个公式）：
//!
//! ```xml
//! <Cell ss:StyleID="s63" ss:Formula="=RC[-1]+R[1]C[-1]"><Data ss:Type="Number">3</Data></Cell>
//! ```
//!
//! 所以"带公式的粘贴"不需要解 OLE/BIFF：把 R1C1 折成 A1 就够了。

mod columns;
pub use columns::sparse_column_xmlss;

use quick_xml::events::Event;
use quick_xml::Reader;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Kind {
    #[default]
    Number,
    Text,
    Bool,
    DateTime,
    Error,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub font: String,
    pub size: f32,
    pub color: Option<[u8; 3]>,
    pub fill: Option<[u8; 3]>,
    pub border: Option<[u8; 3]>,
    pub num_format: String,
    /// 0 常规 1 左 2 中 3 右
    pub halign: u8,
}

/// XMLSS 的 `<Data ss:Type="DateTime">1904-01-02T00:00:00</Data>` → 目标簿的序列号。
///
/// 跨工作簿复制日期**不能**照搬数字：1900 簿的 1 是 1900-01-01，1904 簿的 1 是 1904-01-02，
/// 差 1462 天。所以按 ISO 日期 + **目标簿的日期体系**重新算序列号；普通数字原样保留。
pub fn iso_to_serial(sys: crate::model::DateSystem, iso: &str) -> Option<(f64, (i32, u32, u32))> {
    let s = iso.trim();
    let (date_part, time_part) = match s.find(['T', ' ']) {
        Some(i) => (&s[..i], s[i + 1..].trim()),
        None => (s, ""),
    };
    let mut it = date_part.split('-');
    let y: i32 = it.next()?.parse().ok()?;
    let m: u32 = it.next()?.parse().ok()?;
    let d: u32 = it.next()?.parse().ok()?;
    let serial = crate::formula::date_to_serial_in(sys, y, m, d)?;
    // 时间部分必须保留：XMLSS 的 DateTime 是完整时间戳，
    // "2024-03-01T12:00:00" 是中午，丢掉就成了午夜（差 0.5 天）。
    Some((serial + time_fraction(time_part)?, (y, m, d)))
}

/// "12:34:56.789" → 0.5242683…（一天的分数）；空串 → 0；格式不对 → None
fn time_fraction(t: &str) -> Option<f64> {
    let t = t.trim();
    if t.is_empty() {
        return Some(0.0);
    }
    let mut parts = t.split(':');
    let h: f64 = parts.next()?.parse().ok()?;
    let mi: f64 = parts.next()?.parse().ok()?;
    let sec: f64 = parts.next().unwrap_or("0").parse().ok()?;
    if !(0.0..24.0).contains(&h) || !(0.0..60.0).contains(&mi) || !(0.0..61.0).contains(&sec) {
        return None;
    }
    Some((h * 3600.0 + mi * 60.0 + sec) / 86_400.0)
}

#[derive(Clone, Debug, Default)]
pub struct Cell {
    /// 显示值（数值会保留原始文本，便于回退）
    pub text: String,
    pub kind: Kind,
    /// A1 形式、带前导 `=`；没有公式就是 None
    pub formula: Option<String>,
    pub style: Style,
    pub colspan: u32,
    pub rowspan: u32,
}

#[derive(Clone, Debug, Default)]
pub struct Block {
    pub rows: Vec<Vec<Cell>>,
}

impl Block {
    pub fn cols(&self) -> usize {
        self.rows.iter().map(|r| r.len()).max().unwrap_or(0)
    }
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty() || self.cols() == 0
    }
}

/// 稀疏 XML Spreadsheet 单元格（保留原始 R1C1 公式以便按目标粘贴锚点 `(r0 + row, c0 + col)` 精算 A1 引用）。
#[derive(Clone, Debug, Default)]
pub struct SparseCell {
    pub row: u32,
    pub col: u32,
    pub text: String,
    pub kind: Kind,
    pub r1c1_formula: Option<String>,
    pub style_idx: u32,
    pub has_explicit_style: bool,
    pub colspan: u32,
    pub rowspan: u32,
}

/// 支持百万行整列（`ExpandedRowCount="1048576"`）与超 50 万格大选区的稀疏 XML Spreadsheet 块。
#[derive(Clone, Debug, Default)]
pub struct SparseBlock {
    pub rows: u32,
    pub cols: u32,
    pub full_columns: bool,
    pub date1904: bool,
    pub styles: Vec<Style>,
    pub cells: Vec<SparseCell>,
}

/// Bound allocations by the actual addressed rectangle. Rejecting an oversized
/// block must never change an ss:Index into a different address.
#[derive(Default)]
struct DenseExtent {
    rows: usize,
    cols: usize,
}

impl DenseExtent {
    fn include(&mut self, rows: usize, cols: usize) -> Option<()> {
        self.rows = self.rows.max(rows);
        self.cols = self.cols.max(cols);
        (self.rows <= 1_048_576 && self.cols <= 16_384
            && self.rows.max(1).checked_mul(self.cols.max(1))? <= crate::undo::MAX_DENSE_RECT_KEYS).then_some(())
    }
}

#[inline]
fn local_bytes(q: &[u8]) -> &[u8] {
    match q.iter().rposition(|&b| b == b':') {
        Some(pos) => &q[pos + 1..],
        None => q,
    }
}

#[inline]
fn unescape_cow<'a>(raw: std::borrow::Cow<'a, str>) -> std::borrow::Cow<'a, str> {
    match raw {
        std::borrow::Cow::Borrowed(s) => {
            if s.as_bytes().contains(&b'&') {
                match quick_xml::escape::unescape(s) {
                    Ok(c) => std::borrow::Cow::Owned(c.into_owned()),
                    Err(_) => std::borrow::Cow::Borrowed(s),
                }
            } else {
                std::borrow::Cow::Borrowed(s)
            }
        }
        std::borrow::Cow::Owned(s) => {
            if s.as_bytes().contains(&b'&') {
                match quick_xml::escape::unescape(&s) {
                    Ok(c) => std::borrow::Cow::Owned(c.into_owned()),
                    Err(_) => std::borrow::Cow::Owned(s),
                }
            } else {
                std::borrow::Cow::Owned(s)
            }
        }
    }
}

#[inline]
fn parse_ascii_usize(s: &[u8]) -> Option<usize> {
    if s.is_empty() {
        return None;
    }
    let mut acc = 0usize;
    for &b in s {
        if !b.is_ascii_digit() {
            return None;
        }
        acc = acc.checked_mul(10)?.checked_add((b - b'0') as usize)?;
    }
    Some(acc)
}

#[inline]
fn decode_kind_bytes(v: &[u8]) -> Kind {
    match v {
        b"String" => Kind::Text,
        b"Boolean" => Kind::Bool,
        b"DateTime" => Kind::DateTime,
        b"Error" => Kind::Error,
        _ => Kind::Number,
    }
}

enum FastTableErr {
    Reject,
    Fallback,
}

#[inline]
fn skip_ws(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && matches!(b[i], b' ' | b'\t' | b'\r' | b'\n') {
        i += 1;
    }
    i
}

#[inline]
fn is_name_end(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\r' | b'\n' | b'/' | b'>' | b'=')
}

/// 扫描 `<Tag ...>` 的单个属性 `(local_key, raw_value_slice)`，返回下一起始偏移或遇到 `>`/`/>`。
/// 遇到语法异常返回 `Err(FastTableErr::Fallback)` 交由通用 quick-xml 处理。
#[inline]
fn next_attr<'a>(b: &'a [u8], pos: &mut usize) -> Result<Option<(&'a [u8], &'a [u8])>, FastTableErr> {
    let mut i = skip_ws(b, *pos);
    if i >= b.len() {
        return Err(FastTableErr::Fallback);
    }
    if b[i] == b'>' || b[i] == b'/' {
        *pos = i;
        return Ok(None);
    }
    let kstart = i;
    while i < b.len() && !is_name_end(b[i]) {
        i += 1;
    }
    if i == kstart {
        return Err(FastTableErr::Fallback);
    }
    let key = local_bytes(&b[kstart..i]);
    i = skip_ws(b, i);
    if i >= b.len() || b[i] != b'=' {
        return Err(FastTableErr::Fallback);
    }
    i = skip_ws(b, i + 1);
    if i >= b.len() || (b[i] != b'"' && b[i] != b'\'') {
        return Err(FastTableErr::Fallback);
    }
    let quote = b[i];
    i += 1;
    let vstart = i;
    let Some(rel) = crate::xlsx::sheets::stream::simd_memchr(quote, &b[vstart..]) else {
        return Err(FastTableErr::Fallback);
    };
    i = vstart + rel;
    let val = &b[vstart..i];
    *pos = i + 1;
    Ok(Some((key, val)))
}

#[inline]
fn finish_tag_head(b: &[u8], pos: &mut usize) -> Result<bool, FastTableErr> {
    let i = skip_ws(b, *pos);
    if i >= b.len() {
        return Err(FastTableErr::Fallback);
    }
    if b[i] == b'>' {
        *pos = i + 1;
        Ok(false)
    } else if b[i] == b'/' && b.get(i + 1) == Some(&b'>') {
        *pos = i + 2;
        Ok(true)
    } else {
        Err(FastTableErr::Fallback)
    }
}

/// 在 `<Table ...>` 与 `</Table>` 之间走单遍零拷贝字节快速路径，
/// 避免 100k 行 / 200k 格场景下 quick-xml 的百万次事件与重复属性扫描开销。
#[allow(clippy::too_many_arguments)]
fn parse_sparse_table_fast(
    xml: &str,
    start_pos: usize,
    style_lookup: &std::collections::HashMap<String, u32>,
    col_styles: &mut std::collections::HashMap<u32, u32>,
    cells: &mut Vec<SparseCell>,
    max_rows: &mut usize,
    max_cols: &mut usize,
) -> Result<usize, FastTableErr> {
    const MAX_R: usize = 1_048_576;
    const MAX_C: usize = 16_384;

    let b = xml.as_bytes();
    let len = b.len();
    let mut i = start_pos;
    let mut col_cursor = 0usize;
    let mut row_cursor = 0usize;
    let mut in_row = false;
    let mut row_index = 0usize;
    let mut row_span = 1usize;
    let mut row_style_idx: Option<u32> = None;
    let mut next_col = 0usize;

    // 按 XML 字节长度预估容量，避免大选区 20 次 Vec 扩容搬移
    let est = ((len.saturating_sub(start_pos)) / 48).min(crate::undo::MAX_DENSE_RECT_KEYS);
    if est > cells.capacity() {
        cells.reserve(est - cells.len());
    }

    let include_bounds = |rows: usize, cols: usize, mr: &mut usize, mc: &mut usize| -> Result<(), FastTableErr> {
        *mr = (*mr).max(rows);
        *mc = (*mc).max(cols);
        if *mr <= MAX_R && *mc <= MAX_C {
            Ok(())
        } else {
            Err(FastTableErr::Reject)
        }
    };

    while i < len {
        // 跳到下一个 `<`
        let Some(lt_rel) = crate::xlsx::sheets::stream::simd_memchr(b'<', &b[i..]) else {
            return Err(FastTableErr::Fallback);
        };
        i += lt_rel + 1; // 跳过 `<`
        if i >= len {
            return Err(FastTableErr::Fallback);
        }
        let first = b[i];
        if first == b'!' || first == b'?' {
            // 注释 / CDATA / PI：回退给 quick-xml 完整处理
            return Err(FastTableErr::Fallback);
        }
        if first == b'/' {
            // 闭合标签
            i += 1;
            let nstart = i;
            while i < len && !matches!(b[i], b'>' | b' ' | b'\t' | b'\r' | b'\n') {
                i += 1;
            }
            let close_local = local_bytes(&b[nstart..i]);
            i = skip_ws(b, i);
            if i >= len || b[i] != b'>' {
                return Err(FastTableErr::Fallback);
            }
            i += 1;
            match close_local {
                b"Row" => {
                    if !in_row {
                        return Err(FastTableErr::Fallback);
                    }
                    in_row = false;
                    row_cursor = row_index + row_span;
                    row_style_idx = None;
                }
                b"Table" => {
                    if in_row {
                        return Err(FastTableErr::Fallback);
                    }
                    return Ok(i);
                }
                _ => return Err(FastTableErr::Fallback),
            }
            continue;
        }

        // 开始标签名
        let nstart = i;
        while i < len && !is_name_end(b[i]) {
            i += 1;
        }
        let tag_local = local_bytes(&b[nstart..i]);

        match tag_local {
            b"Row" => {
                if in_row {
                    return Err(FastTableErr::Fallback);
                }
                let mut idx_attr: Option<usize> = None;
                let mut span_attr: usize = 0;
                let mut sty_attr: Option<u32> = None;
                if i < len && b[i] == b'>' {
                    i += 1;
                    // 最热路径：无属性 `<Row>`
                    let ri = row_cursor;
                    include_bounds(ri.checked_add(1).ok_or(FastTableErr::Reject)?, 1, max_rows, max_cols)?;
                    in_row = true;
                    row_index = ri;
                    row_span = 1;
                    row_style_idx = None;
                    next_col = 0;
                    continue;
                }
                while let Some((k, v)) = next_attr(b, &mut i)? {
                    match k {
                        b"Index" => idx_attr = Some(parse_ascii_usize(v).ok_or(FastTableErr::Reject)?),
                        b"Span" => span_attr = parse_ascii_usize(v).ok_or(FastTableErr::Reject)?,
                        b"StyleID" => {
                            if let Ok(s) = std::str::from_utf8(v) {
                                let cow = unescape_cow(std::borrow::Cow::Borrowed(s));
                                sty_attr = style_lookup.get(cow.as_ref()).copied().filter(|&id| id != 0);
                            }
                        }
                        _ => {}
                    }
                }
                let self_closing = finish_tag_head(b, &mut i)?;
                let ri = idx_attr.unwrap_or(row_cursor + 1).checked_sub(1).ok_or(FastTableErr::Reject)?;
                let span = span_attr.checked_add(1).ok_or(FastTableErr::Reject)?;
                if ri < row_cursor {
                    return Err(FastTableErr::Reject);
                }
                include_bounds(ri.checked_add(span).ok_or(FastTableErr::Reject)?, 1, max_rows, max_cols)?;
                if self_closing {
                    row_cursor = ri + span;
                } else {
                    in_row = true;
                    row_index = ri;
                    row_span = span;
                    row_style_idx = sty_attr;
                    next_col = 0;
                }
            }
            b"Cell" => {
                if !in_row {
                    return Err(FastTableErr::Fallback);
                }
                let (col, across, down, style_idx, has_explicit_style, r1c1_formula, self_closing) =
                    if i < len && b[i] == b'>' {
                        // 最热路径：无属性 `<Cell>`
                        i += 1;
                        let col = next_col;
                        include_bounds(
                            row_index.checked_add(1).ok_or(FastTableErr::Reject)?,
                            col.checked_add(1).ok_or(FastTableErr::Reject)?,
                            max_rows,
                            max_cols,
                        )?;
                        next_col = col + 1;
                        let style_idx = row_style_idx
                            .or_else(|| {
                                if col_styles.is_empty() {
                                    None
                                } else {
                                    col_styles.get(&(col as u32)).copied()
                                }
                            })
                            .unwrap_or(0);
                        (col, 1usize, 1usize, style_idx, style_idx != 0, None, false)
                    } else {
                        let mut idx_attr: Option<usize> = None;
                        let mut across_attr = 0usize;
                        let mut down_attr = 0usize;
                        let mut raw_style: Option<&[u8]> = None;
                        let mut raw_formula: Option<&[u8]> = None;
                        while let Some((k, v)) = next_attr(b, &mut i)? {
                            match k {
                                b"Index" => {
                                    idx_attr = Some(parse_ascii_usize(v).ok_or(FastTableErr::Reject)?);
                                }
                                b"MergeAcross" => {
                                    across_attr = parse_ascii_usize(v).ok_or(FastTableErr::Reject)?;
                                }
                                b"MergeDown" => {
                                    down_attr = parse_ascii_usize(v).ok_or(FastTableErr::Reject)?;
                                }
                                b"StyleID" => raw_style = Some(v),
                                b"Formula" => raw_formula = Some(v),
                                _ => {}
                            }
                        }
                        let self_closing = finish_tag_head(b, &mut i)?;
                        let col = idx_attr.unwrap_or(next_col + 1).checked_sub(1).ok_or(FastTableErr::Reject)?;
                        let across = across_attr.checked_add(1).ok_or(FastTableErr::Reject)?;
                        let down = down_attr.checked_add(1).ok_or(FastTableErr::Reject)?;
                        if col < next_col {
                            return Err(FastTableErr::Reject);
                        }
                        include_bounds(
                            row_index.checked_add(down).ok_or(FastTableErr::Reject)?,
                            col.checked_add(across).ok_or(FastTableErr::Reject)?,
                            max_rows,
                            max_cols,
                        )?;
                        next_col = col + across;
                        let has_raw_style = raw_style.is_some();
                        let style_idx = raw_style
                            .and_then(|v| std::str::from_utf8(v).ok())
                            .and_then(|s| {
                                let cow = unescape_cow(std::borrow::Cow::Borrowed(s));
                                style_lookup.get(cow.as_ref()).copied()
                            })
                            .or(row_style_idx)
                            .or_else(|| {
                                if col_styles.is_empty() {
                                    None
                                } else {
                                    col_styles.get(&(col as u32)).copied()
                                }
                            })
                            .unwrap_or(0);
                        let r1c1_formula = raw_formula
                            .and_then(|v| std::str::from_utf8(v).ok())
                            .map(|s| unescape_cow(std::borrow::Cow::Borrowed(s)).into_owned());
                        (
                            col,
                            across,
                            down,
                            style_idx,
                            has_raw_style || style_idx != 0,
                            r1c1_formula,
                            self_closing,
                        )
                    };

                let mut text = String::new();
                let mut kind = Kind::Number;
                let mut had_data = false;

                if !self_closing {
                    // 超热快路径：直接匹配标准单子元素 `<Data ss:Type="...">...</Data></Cell>`
                    let d_pos = skip_ws(b, i);
                    let fast_matched = if b[d_pos..].starts_with(b"<Data ss:Type=\"") {
                        let type_start = d_pos + 15;
                        let fast_type = if b[type_start..].starts_with(b"Number\">") {
                            Some((Kind::Number, type_start + 8))
                        } else if b[type_start..].starts_with(b"String\">") {
                            Some((Kind::Text, type_start + 8))
                        } else if b[type_start..].starts_with(b"Boolean\">") {
                            Some((Kind::Bool, type_start + 9))
                        } else if b[type_start..].starts_with(b"DateTime\">") {
                            Some((Kind::DateTime, type_start + 10))
                        } else if b[type_start..].starts_with(b"Error\">") {
                            Some((Kind::Error, type_start + 7))
                        } else {
                            None
                        };
                        if let Some((dtype, tstart)) = fast_type {
                            let mut scan = tstart;
                            let mut has_amp = false;
                            let val_end = loop {
                                match crate::xlsx::sheets::stream::simd_memchr2(
                                    b'<',
                                    b'&',
                                    &b[scan..],
                                ) {
                                    Some(rel) => {
                                        let hit = scan + rel;
                                        if b[hit] == b'<' {
                                            break Some(hit);
                                        }
                                        has_amp = true;
                                        scan = hit + 1;
                                    }
                                    None => break None,
                                }
                            };
                            if let Some(vend) = val_end
                                && b[vend..].starts_with(b"</Data></Cell>")
                            {
                                let raw_str = &xml[tstart..vend];
                                text = if has_amp {
                                    match quick_xml::escape::unescape(raw_str) {
                                        Ok(c) => c.into_owned(),
                                        Err(_) => return Err(FastTableErr::Fallback),
                                    }
                                } else {
                                    raw_str.to_owned()
                                };
                                kind = dtype;
                                had_data = true;
                                i = vend + 14;
                                true
                            } else {
                                false
                            }
                        } else {
                            false
                        }
                    } else {
                        false
                    };

                    if !fast_matched {
                        // 读取 `<Cell>` 内部直至 `</Cell>`
                        loop {
                            let Some(lt_rel) =
                                crate::xlsx::sheets::stream::simd_memchr(b'<', &b[i..])
                            else {
                                return Err(FastTableErr::Fallback);
                            };
                            i += lt_rel;
                            if i + 1 >= len {
                                return Err(FastTableErr::Fallback);
                            }
                            i += 1;
                            if b[i] == b'!' || b[i] == b'?' {
                                return Err(FastTableErr::Fallback);
                            }
                            if b[i] == b'/' {
                                i += 1;
                                let cs = i;
                                while i < len && !matches!(b[i], b'>' | b' ' | b'\t' | b'\r' | b'\n') {
                                    i += 1;
                                }
                                let cl = local_bytes(&b[cs..i]);
                                i = skip_ws(b, i);
                                if i >= len || b[i] != b'>' || cl != b"Cell" {
                                    return Err(FastTableErr::Fallback);
                                }
                                i += 1;
                                break;
                            }
                            let cs = i;
                            while i < len && !is_name_end(b[i]) {
                                i += 1;
                            }
                            let child = local_bytes(&b[cs..i]);
                            if child == b"Data" {
                                let mut dtype = Kind::Number;
                                while let Some((k, v)) = next_attr(b, &mut i)? {
                                    if k == b"Type" {
                                        dtype = decode_kind_bytes(v);
                                    }
                                }
                                let data_empty = finish_tag_head(b, &mut i)?;
                                if !data_empty {
                                    // 直接扫描到 `</Data>`；若含子标签或 CDATA/注释则回退 quick-xml
                                    let tstart = i;
                                    let mut has_amp = false;
                                    let vend = loop {
                                        match crate::xlsx::sheets::stream::simd_memchr2(
                                            b'<',
                                            b'&',
                                            &b[i..],
                                        ) {
                                            Some(rel) => {
                                                let hit = i + rel;
                                                if b[hit] == b'<' {
                                                    break hit;
                                                }
                                                has_amp = true;
                                                i = hit + 1;
                                            }
                                            None => return Err(FastTableErr::Fallback),
                                        }
                                    };
                                    i = vend;
                                    if i + 2 >= len || b[i + 1] != b'/' {
                                        return Err(FastTableErr::Fallback);
                                    }
                                    let raw_str = &xml[tstart..i];
                                    i += 2; // 跳过 `</`
                                    let es = i;
                                    while i < len && !matches!(b[i], b'>' | b' ' | b'\t' | b'\r' | b'\n') {
                                        i += 1;
                                    }
                                    if local_bytes(&b[es..i]) != b"Data" {
                                        return Err(FastTableErr::Fallback);
                                    }
                                    i = skip_ws(b, i);
                                    if i >= len || b[i] != b'>' {
                                        return Err(FastTableErr::Fallback);
                                    }
                                    i += 1;
                                    text = if has_amp {
                                        match quick_xml::escape::unescape(raw_str) {
                                            Ok(c) => c.into_owned(),
                                            Err(_) => return Err(FastTableErr::Fallback),
                                        }
                                    } else {
                                        raw_str.to_owned()
                                    };
                                    kind = dtype;
                                    had_data = true;
                                }
                            } else {
                                // 如 `<Comment>` / `<NamedCell>` 等子标签：回退 quick-xml
                                return Err(FastTableErr::Fallback);
                            }
                        }
                    }
                }

                let meaningful = had_data
                    || !text.is_empty()
                    || r1c1_formula.is_some()
                    || has_explicit_style
                    || across > 1
                    || down > 1;
                if meaningful {
                    if cells.len() >= crate::undo::MAX_DENSE_RECT_KEYS {
                        return Err(FastTableErr::Reject);
                    }
                    cells.push(SparseCell {
                        row: row_index as u32,
                        col: col as u32,
                        text,
                        kind,
                        r1c1_formula,
                        style_idx,
                        has_explicit_style,
                        colspan: across as u32,
                        rowspan: down as u32,
                    });
                }
            }
            b"Column" => {
                if in_row {
                    return Err(FastTableErr::Fallback);
                }
                let mut idx_attr: Option<usize> = None;
                let mut span_attr = 0usize;
                let mut raw_style: Option<&[u8]> = None;
                while let Some((k, v)) = next_attr(b, &mut i)? {
                    match k {
                        b"Index" => idx_attr = Some(parse_ascii_usize(v).ok_or(FastTableErr::Reject)?),
                        b"Span" => span_attr = parse_ascii_usize(v).ok_or(FastTableErr::Reject)?,
                        b"StyleID" => raw_style = Some(v),
                        _ => {}
                    }
                }
                let self_closing = finish_tag_head(b, &mut i)?;
                if !self_closing {
                    // 消费 `</Column>`
                    while i < len && b[i] != b'<' {
                        i += 1;
                    }
                    if i + 2 >= len || b[i + 1] != b'/' {
                        return Err(FastTableErr::Fallback);
                    }
                    i += 2;
                    let cs = i;
                    while i < len && !matches!(b[i], b'>' | b' ' | b'\t' | b'\r' | b'\n') {
                        i += 1;
                    }
                    if local_bytes(&b[cs..i]) != b"Column" {
                        return Err(FastTableErr::Fallback);
                    }
                    i = skip_ws(b, i);
                    if i >= len || b[i] != b'>' {
                        return Err(FastTableErr::Fallback);
                    }
                    i += 1;
                }
                let c_idx = idx_attr.unwrap_or(col_cursor + 1).checked_sub(1).ok_or(FastTableErr::Reject)?;
                let c_span = span_attr.checked_add(1).ok_or(FastTableErr::Reject)?;
                if c_idx < col_cursor {
                    return Err(FastTableErr::Reject);
                }
                include_bounds(0, c_idx.checked_add(c_span).ok_or(FastTableErr::Reject)?, max_rows, max_cols)?;
                if let Some(sid) = raw_style
                    .and_then(|v| std::str::from_utf8(v).ok())
                    .and_then(|s| {
                        let cow = unescape_cow(std::borrow::Cow::Borrowed(s));
                        style_lookup.get(cow.as_ref()).copied()
                    })
                {
                    if sid != 0 {
                        for cc in c_idx..(c_idx + c_span) {
                            col_styles.insert(cc as u32, sid);
                        }
                    }
                }
                col_cursor = c_idx + c_span;
            }
            _ => return Err(FastTableErr::Fallback),
        }
    }
    Err(FastTableErr::Fallback)
}

#[inline]
fn attr_cow<'a>(
    e: &'a quick_xml::events::BytesStart<'a>,
    key: &[u8],
) -> Option<std::borrow::Cow<'a, str>> {
    for a in e.attributes().flatten() {
        if local_bytes(a.key.as_ref().as_bytes()) == key {
            return Some(unescape_cow(a.value));
        }
    }
    None
}

#[inline]
fn index_attr(e: &quick_xml::events::BytesStart<'_>, key: &[u8], default: usize) -> Option<usize> {
    match attr_cow(e, key) {
        Some(s) => s.parse().ok(),
        None => Some(default),
    }
}

fn prepare_row(
    e: &quick_xml::events::BytesStart<'_>,
    rows_len: usize,
    extent: &mut DenseExtent,
) -> Option<(usize, usize)> {
    let row = index_attr(e, b"Index", rows_len + 1)?.checked_sub(1)?;
    let span = index_attr(e, b"Span", 0)?.checked_add(1)?;
    if row < rows_len {
        return None;
    }
    extent.include(row.checked_add(span)?, 1)?;
    Some((row, span))
}

#[derive(Default)]
struct RawCell {
    text: String,
    kind: Kind,
    formula: Option<String>,
    style_idx: u32,
    colspan: u32,
    rowspan: u32,
}

fn prepare_cell(
    e: &quick_xml::events::BytesStart<'_>,
    row: &mut Vec<RawCell>,
    r: usize,
    next_col: &mut usize,
    extent: &mut DenseExtent,
    style_lookup: &std::collections::HashMap<String, u32>,
) -> Option<RawCell> {
    let col = index_attr(e, b"Index", *next_col + 1)?.checked_sub(1)?;
    let across = index_attr(e, b"MergeAcross", 0)?.checked_add(1)?;
    let down = index_attr(e, b"MergeDown", 0)?.checked_add(1)?;
    if col < *next_col {
        return None;
    }
    extent.include(r.checked_add(down)?, col.checked_add(across)?)?;
    row.resize_with(row.len() + (col - *next_col), RawCell::default);
    *next_col = col + across;
    let style_idx = attr_cow(e, b"StyleID")
        .and_then(|id| style_lookup.get(id.as_ref()).copied())
        .unwrap_or(0);
    Some(RawCell {
        style_idx,
        formula: attr_cow(e, b"Formula").map(|f| r1c1_to_a1(&f, r as u32, col as u32)),
        colspan: across as u32,
        rowspan: down as u32,
        ..Default::default()
    })
}

fn apply_style_tag(tag_local: &[u8], e: &quick_xml::events::BytesStart<'_>, cur_style: &mut Style) {
    match tag_local {
        b"Font" => {
            cur_style.bold = matches!(attr_cow(e, b"Bold").as_deref(), Some("1" | "true"));
            cur_style.italic = matches!(attr_cow(e, b"Italic").as_deref(), Some("1" | "true"));
            cur_style.font = attr_cow(e, b"FontName").map(|c| c.into_owned()).unwrap_or_default();
            if let Some(sz) = attr_cow(e, b"Size").and_then(|s| s.parse::<f32>().ok()) {
                if sz > 0.0 {
                    cur_style.size = sz;
                }
            }
            cur_style.color = attr_cow(e, b"Color").and_then(|c| hex_rgb(&c));
        }
        b"Interior" => {
            let pattern = attr_cow(e, b"Pattern");
            if pattern.as_deref().is_some_and(|p| !p.is_empty() && p != "None") {
                cur_style.fill = attr_cow(e, b"Color").and_then(|c| hex_rgb(&c));
            }
        }
        b"Border" => {
            if let Some(c) = attr_cow(e, b"Color").and_then(|c| hex_rgb(&c)) {
                cur_style.border.get_or_insert(c);
            }
        }
        b"NumberFormat" => {
            if let Some(f) = attr_cow(e, b"Format") {
                cur_style.num_format = normalize_num_format(&f);
            }
        }
        b"Alignment" => {
            cur_style.halign = match attr_cow(e, b"Horizontal").as_deref() {
                Some("Left") => 1,
                Some("Center") => 2,
                Some("Right") => 3,
                _ => 0,
            };
        }
        _ => {}
    }
}

/// 解析整份 XML Spreadsheet；不是这个格式（没有 Workbook/Worksheet）就返回空
pub fn parse(xml: &str) -> Block {
    let mut style_table: Vec<Style> = vec![Style::default()];
    let mut style_lookup: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    let mut raw_rows: Vec<Vec<RawCell>> = Vec::new();
    let mut r = Reader::from_str(xml);
    r.config_mut().trim_text(false);

    let mut style_id = String::new();
    let mut cur_style = Style::default();
    let mut in_style = false;

    let mut row: Vec<RawCell> = Vec::new();
    let mut cur: Option<RawCell> = None;
    let mut data_type = Kind::Number;
    let mut in_data = false;
    let mut text = String::new();
    let mut extent = DenseExtent::default();
    let (mut row_index, mut row_span, mut next_col) = (0usize, 1usize, 0usize);

    loop {
        match r.read_event() {
            Ok(Event::Start(e)) => {
                let qname = e.name();
                let n = local_bytes(qname.as_ref().as_bytes());
                match n {
                    b"Style" => {
                        in_style = true;
                        style_id = attr_cow(&e, b"ID").map(|c| c.into_owned()).unwrap_or_default();
                        cur_style = Style::default();
                    }
                    b"Font" | b"Interior" | b"Border" | b"NumberFormat" | b"Alignment" if in_style => {
                        apply_style_tag(n, &e, &mut cur_style);
                    }
                    b"Table" => {
                        let dims = index_attr(&e, b"ExpandedRowCount", 0)
                            .zip(index_attr(&e, b"ExpandedColumnCount", 0));
                        if dims.and_then(|(rows, cols)| extent.include(rows, cols)).is_none() {
                            return Block::default();
                        }
                    }
                    b"Row" => {
                        let Some((ri, span)) = prepare_row(&e, raw_rows.len(), &mut extent) else {
                            return Block::default();
                        };
                        raw_rows.resize_with(ri, Vec::new);
                        (row_index, row_span, next_col) = (ri, span, 0);
                        row.clear();
                    }
                    b"Cell" => {
                        cur = prepare_cell(&e, &mut row, row_index, &mut next_col, &mut extent, &style_lookup);
                        if cur.is_none() {
                            return Block::default();
                        }
                    }
                    b"Data" => {
                        data_type = match attr_cow(&e, b"Type").as_deref() {
                            Some("String") => Kind::Text,
                            Some("Boolean") => Kind::Bool,
                            Some("DateTime") => Kind::DateTime,
                            Some("Error") => Kind::Error,
                            _ => Kind::Number,
                        };
                        in_data = true;
                        text.clear();
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(e)) => {
                let qname = e.name();
                let n = local_bytes(qname.as_ref().as_bytes());
                if in_style {
                    apply_style_tag(n, &e, &mut cur_style);
                } else if n == b"Cell" {
                    let Some(cell) =
                        prepare_cell(&e, &mut row, row_index, &mut next_col, &mut extent, &style_lookup)
                    else {
                        return Block::default();
                    };
                    row.push(cell);
                } else if n == b"Row" {
                    let Some((ri, span)) = prepare_row(&e, raw_rows.len(), &mut extent) else {
                        return Block::default();
                    };
                    raw_rows.resize_with(ri + span, Vec::new);
                }
            }
            Ok(Event::GeneralRef(r_ref)) => {
                if in_data {
                    if let Ok(Some(ch)) = r_ref.resolve_char_ref() {
                        text.push(ch);
                    } else {
                        match r_ref.as_ref() {
                            "amp" => text.push('&'),
                            "lt" => text.push('<'),
                            "gt" => text.push('>'),
                            "quot" => text.push('"'),
                            "apos" => text.push('\''),
                            _ => {}
                        }
                    }
                }
            }
            Ok(Event::Text(t)) => {
                if in_data {
                    let raw = t.as_ref();
                    if raw.contains('&') {
                        match quick_xml::escape::unescape(raw) {
                            Ok(c) => text.push_str(&c),
                            Err(_) => text.push_str(raw),
                        }
                    } else {
                        text.push_str(raw);
                    }
                }
            }
            Ok(Event::End(e)) => {
                let qname = e.name();
                let n = local_bytes(qname.as_ref().as_bytes());
                match n {
                    b"Style" => {
                        in_style = false;
                        if cur_style == Style::default() {
                            style_lookup.insert(std::mem::take(&mut style_id), 0);
                        } else {
                            let idx = style_table.len() as u32;
                            style_table.push(std::mem::take(&mut cur_style));
                            style_lookup.insert(std::mem::take(&mut style_id), idx);
                        }
                    }
                    b"Data" => {
                        in_data = false;
                        if let Some(c) = cur.as_mut() {
                            c.text = std::mem::take(&mut text);
                            c.kind = data_type;
                        }
                    }
                    b"Cell" => {
                        if let Some(c) = cur.take() {
                            row.push(c);
                        }
                    }
                    b"Row" => {
                        raw_rows.push(std::mem::take(&mut row));
                        raw_rows.resize_with(row_index + row_span, Vec::new);
                    }
                    b"Worksheet" => break,
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => return Block::default(),
            _ => {}
        }
    }

    let rows = raw_rows
        .into_iter()
        .map(|r_vec| {
            r_vec
                .into_iter()
                .map(|rc| Cell {
                    text: rc.text,
                    kind: rc.kind,
                    formula: rc.formula,
                    style: if rc.style_idx == 0 {
                        Style::default()
                    } else {
                        style_table[rc.style_idx as usize].clone()
                    },
                    colspan: rc.colspan,
                    rowspan: rc.rowspan,
                })
                .collect()
        })
        .collect();
    Block { rows }
}

/// 稀疏解析 XML Spreadsheet：允许 `rows <= 1_048_576`、`cols <= 16_384`（含外部 Excel / 跨实例整列与超 50 万格稀疏区域），
/// 严格校验 `ss:Index` 单调不回退且不越界，并限制实际显式非空/带样式/合并单元格总数 `<= MAX_DENSE_RECT_KEYS`。
pub fn parse_sparse(xml: &str) -> Option<SparseBlock> {
    const MAX_R: usize = 1_048_576;
    const MAX_C: usize = 16_384;

    let mut styles: Vec<Style> = vec![Style::default()];
    let mut style_lookup: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    let mut col_styles: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    let mut cells: Vec<SparseCell> = Vec::new();

    let mut r = Reader::from_str(xml);
    r.config_mut().trim_text(false);

    let mut style_id = String::new();
    let mut cur_style = Style::default();
    let mut in_style = false;

    let mut saw_table = false;
    let mut full_columns = false;
    let mut date1904 = false;
    let mut max_rows = 0usize;
    let mut max_cols = 0usize;
    let mut col_cursor = 0usize;
    let mut row_cursor = 0usize;
    let mut row_index = 0usize;
    let mut row_span = 1usize;
    let mut row_style_idx: Option<u32> = None;
    let mut next_col = 0usize;

    let mut cur: Option<(SparseCell, bool)> = None;
    let mut data_type = Kind::Number;
    let mut in_data = false;
    let mut text = String::new();

    let include_bounds = |rows: usize, cols: usize, mr: &mut usize, mc: &mut usize| -> Option<()> {
        *mr = (*mr).max(rows);
        *mc = (*mc).max(cols);
        (*mr <= MAX_R && *mc <= MAX_C).then_some(())
    };

    let parse_sparse_cell = |e: &quick_xml::events::BytesStart<'_>,
                             r_idx: usize,
                             next_col: &mut usize,
                             mr: &mut usize,
                             mc: &mut usize,
                             row_sty: Option<u32>,
                             col_styles: &std::collections::HashMap<u32, u32>,
                             style_lookup: &std::collections::HashMap<String, u32>|
     -> Option<SparseCell> {
        let col = index_attr(e, b"Index", *next_col + 1)?.checked_sub(1)?;
        let across = index_attr(e, b"MergeAcross", 0)?.checked_add(1)?;
        let down = index_attr(e, b"MergeDown", 0)?.checked_add(1)?;
        if col < *next_col {
            return None;
        }
        include_bounds(r_idx.checked_add(down)?, col.checked_add(across)?, mr, mc)?;
        *next_col = col + across;
        let raw_style = attr_cow(e, b"StyleID");
        let has_explicit_style = raw_style.is_some();
        let style_idx = raw_style
            .and_then(|id| style_lookup.get(id.as_ref()).copied())
            .or(row_sty)
            .or_else(|| col_styles.get(&(col as u32)).copied())
            .unwrap_or(0);
        Some(SparseCell {
            row: r_idx as u32,
            col: col as u32,
            r1c1_formula: attr_cow(e, b"Formula").map(|f| f.into_owned()),
            style_idx,
            has_explicit_style: has_explicit_style || style_idx != 0,
            colspan: across as u32,
            rowspan: down as u32,
            ..Default::default()
        })
    };

    let push_if_meaningful = |cells: &mut Vec<SparseCell>, cell: SparseCell, had_data: bool| -> Option<()> {
        let meaningful = had_data
            || !cell.text.is_empty()
            || cell.r1c1_formula.is_some()
            || cell.has_explicit_style
            || cell.colspan > 1
            || cell.rowspan > 1;
        if meaningful {
            if cells.len() >= crate::undo::MAX_DENSE_RECT_KEYS {
                return None;
            }
            cells.push(cell);
        }
        Some(())
    };

    loop {
        match r.read_event() {
            Ok(Event::Start(e)) => {
                let qname = e.name();
                let n = local_bytes(qname.as_ref().as_bytes());
                match n {
                    b"Date1904" => date1904 = true,
                    b"Style" => {
                        in_style = true;
                        style_id = attr_cow(&e, b"ID").map(|c| c.into_owned()).unwrap_or_default();
                        cur_style = Style::default();
                    }
                    b"Font" | b"Interior" | b"Border" | b"NumberFormat" | b"Alignment" if in_style => {
                        apply_style_tag(n, &e, &mut cur_style);
                    }
                    b"Table" => {
                        saw_table = true;
                        let dims = index_attr(&e, b"ExpandedRowCount", 0)
                            .zip(index_attr(&e, b"ExpandedColumnCount", 0));
                        let (er, ec) = dims?;
                        include_bounds(er, ec, &mut max_rows, &mut max_cols)?;
                        if attr_cow(&e, b"FullColumns").as_deref() == Some("1") || er == MAX_R {
                            full_columns = true;
                        }
                        let table_body_start = r.buffer_position() as usize;
                        let saved_mr = max_rows;
                        let saved_mc = max_cols;
                        match parse_sparse_table_fast(
                            xml,
                            table_body_start,
                            &style_lookup,
                            &mut col_styles,
                            &mut cells,
                            &mut max_rows,
                            &mut max_cols,
                        ) {
                            Ok(_end_pos) => break,
                            Err(FastTableErr::Reject) => return None,
                            Err(FastTableErr::Fallback) => {
                                cells.clear();
                                col_styles.clear();
                                max_rows = saved_mr;
                                max_cols = saved_mc;
                            }
                        }
                    }
                    b"Column" => {
                        let c_idx = index_attr(&e, b"Index", col_cursor + 1)?.checked_sub(1)?;
                        let c_span = index_attr(&e, b"Span", 0)?.checked_add(1)?;
                        if c_idx < col_cursor {
                            return None;
                        }
                        include_bounds(0, c_idx.checked_add(c_span)?, &mut max_rows, &mut max_cols)?;
                        if let Some(sid) = attr_cow(&e, b"StyleID").and_then(|id| style_lookup.get(id.as_ref()).copied()) {
                            if sid != 0 {
                                for cc in c_idx..(c_idx + c_span) {
                                    col_styles.insert(cc as u32, sid);
                                }
                            }
                        }
                        col_cursor = c_idx + c_span;
                    }
                    b"Row" => {
                        let ri = index_attr(&e, b"Index", row_cursor + 1)?.checked_sub(1)?;
                        let span = index_attr(&e, b"Span", 0)?.checked_add(1)?;
                        if ri < row_cursor {
                            return None;
                        }
                        include_bounds(ri.checked_add(span)?, 1, &mut max_rows, &mut max_cols)?;
                        row_index = ri;
                        row_span = span;
                        row_style_idx = attr_cow(&e, b"StyleID")
                            .and_then(|id| style_lookup.get(id.as_ref()).copied())
                            .filter(|&id| id != 0);
                        next_col = 0;
                    }
                    b"Cell" => {
                        let cell = parse_sparse_cell(
                            &e,
                            row_index,
                            &mut next_col,
                            &mut max_rows,
                            &mut max_cols,
                            row_style_idx,
                            &col_styles,
                            &style_lookup,
                        )?;
                        cur = Some((cell, false));
                    }
                    b"Data" => {
                        data_type = match attr_cow(&e, b"Type").as_deref() {
                            Some("String") => Kind::Text,
                            Some("Boolean") => Kind::Bool,
                            Some("DateTime") => Kind::DateTime,
                            Some("Error") => Kind::Error,
                            _ => Kind::Number,
                        };
                        in_data = true;
                        text.clear();
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(e)) => {
                let qname = e.name();
                let n = local_bytes(qname.as_ref().as_bytes());
                if in_style {
                    apply_style_tag(n, &e, &mut cur_style);
                } else {
                    match n {
                        b"Date1904" => date1904 = true,
                        b"Table" => {
                            saw_table = true;
                            let (er, ec) = index_attr(&e, b"ExpandedRowCount", 0)
                                .zip(index_attr(&e, b"ExpandedColumnCount", 0))?;
                            include_bounds(er, ec, &mut max_rows, &mut max_cols)?;
                            if attr_cow(&e, b"FullColumns").as_deref() == Some("1") || er == MAX_R {
                                full_columns = true;
                            }
                        }
                        b"Column" => {
                            let c_idx = index_attr(&e, b"Index", col_cursor + 1)?.checked_sub(1)?;
                            let c_span = index_attr(&e, b"Span", 0)?.checked_add(1)?;
                            if c_idx < col_cursor {
                                return None;
                            }
                            include_bounds(0, c_idx.checked_add(c_span)?, &mut max_rows, &mut max_cols)?;
                            if let Some(sid) = attr_cow(&e, b"StyleID").and_then(|id| style_lookup.get(id.as_ref()).copied()) {
                                if sid != 0 {
                                    for cc in c_idx..(c_idx + c_span) {
                                        col_styles.insert(cc as u32, sid);
                                    }
                                }
                            }
                            col_cursor = c_idx + c_span;
                        }
                        b"Row" => {
                            let ri = index_attr(&e, b"Index", row_cursor + 1)?.checked_sub(1)?;
                            let span = index_attr(&e, b"Span", 0)?.checked_add(1)?;
                            if ri < row_cursor {
                                return None;
                            }
                            include_bounds(ri.checked_add(span)?, 1, &mut max_rows, &mut max_cols)?;
                            row_cursor = ri + span;
                        }
                        b"Cell" => {
                            let cell = parse_sparse_cell(
                                &e,
                                row_index,
                                &mut next_col,
                                &mut max_rows,
                                &mut max_cols,
                                row_style_idx,
                                &col_styles,
                                &style_lookup,
                            )?;
                            push_if_meaningful(&mut cells, cell, false)?;
                        }
                        _ => {}
                    }
                }
            }
            Ok(Event::GeneralRef(r_ref)) => {
                if in_data {
                    if let Ok(Some(ch)) = r_ref.resolve_char_ref() {
                        text.push(ch);
                    } else {
                        match r_ref.as_ref() {
                            "amp" => text.push('&'),
                            "lt" => text.push('<'),
                            "gt" => text.push('>'),
                            "quot" => text.push('"'),
                            "apos" => text.push('\''),
                            _ => {}
                        }
                    }
                }
            }
            Ok(Event::Text(t)) => {
                if in_data {
                    let raw = t.as_ref();
                    if raw.contains('&') {
                        match quick_xml::escape::unescape(raw) {
                            Ok(c) => text.push_str(&c),
                            Err(_) => text.push_str(raw),
                        }
                    } else {
                        text.push_str(raw);
                    }
                }
            }
            Ok(Event::End(e)) => {
                let qname = e.name();
                let n = local_bytes(qname.as_ref().as_bytes());
                match n {
                    b"Style" => {
                        in_style = false;
                        if cur_style == Style::default() {
                            style_lookup.insert(std::mem::take(&mut style_id), 0);
                        } else {
                            let idx = styles.len() as u32;
                            styles.push(std::mem::take(&mut cur_style));
                            style_lookup.insert(std::mem::take(&mut style_id), idx);
                        }
                    }
                    b"Data" => {
                        in_data = false;
                        if let Some((c, had_data)) = cur.as_mut() {
                            c.text = std::mem::take(&mut text);
                            c.kind = data_type;
                            *had_data = true;
                        }
                    }
                    b"Cell" => {
                        if let Some((c, had_data)) = cur.take() {
                            push_if_meaningful(&mut cells, c, had_data)?;
                        }
                    }
                    b"Row" => {
                        row_cursor = row_index + row_span;
                        row_style_idx = None;
                    }
                    b"Worksheet" => break,
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => return None,
            _ => {}
        }
    }

    if !saw_table || max_rows == 0 || max_cols == 0 {
        return None;
    }
    Some(SparseBlock {
        rows: max_rows as u32,
        cols: max_cols as u32,
        full_columns,
        date1904,
        styles,
        cells,
    })
}

/// Zero-copy borrowed sparse cell (`Cow<'a, str>` avoids per-cell `String` heap allocations
/// when parsing 100k–500k unescaped XMLSS cells).
#[derive(Clone, Debug, Default)]
pub struct BorrowedSparseCell<'a> {
    pub row: u32,
    pub col: u32,
    pub text: std::borrow::Cow<'a, str>,
    pub kind: Kind,
    pub r1c1_formula: Option<std::borrow::Cow<'a, str>>,
    pub style_idx: u32,
    pub has_explicit_style: bool,
    pub colspan: u32,
    pub rowspan: u32,
}

#[derive(Clone, Debug, Default)]
pub struct BorrowedSparseBlock<'a> {
    pub rows: u32,
    pub cols: u32,
    pub full_columns: bool,
    pub date1904: bool,
    pub styles: Vec<Style>,
    pub cells: Vec<BorrowedSparseCell<'a>>,
}

/// Zero-copy fast parser for `<Table>` body into `BorrowedSparseCell<'a>`, skipping `<Styles>`
/// entirely when `values_only == true`.
pub fn parse_sparse_borrowed<'a>(xml: &'a str, values_only: bool) -> Option<BorrowedSparseBlock<'a>> {
    const MAX_R: usize = 1_048_576;
    const MAX_C: usize = 16_384;

    let mut styles: Vec<Style> = vec![Style::default()];
    let mut style_lookup: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    let mut col_styles: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();

    let mut r = Reader::from_str(xml);
    r.config_mut().trim_text(false);
    let mut style_id = String::new();
    let mut cur_style = Style::default();
    let mut in_style = false;
    let mut date1904 = false;
    let mut max_rows = 0usize;
    let mut max_cols = 0usize;
    let mut full_columns = false;
    let mut table_body_start: Option<usize> = None;
    let mut empty_table = false;

    loop {
        match r.read_event() {
            Ok(Event::Start(e)) => {
                let qname = e.name();
                let n = local_bytes(qname.as_ref().as_bytes());
                match n {
                    b"Date1904" => date1904 = true,
                    b"Style" if !values_only => {
                        in_style = true;
                        style_id = attr_cow(&e, b"ID").map(|c| c.into_owned()).unwrap_or_default();
                        cur_style = Style::default();
                    }
                    b"Font" | b"Interior" | b"Border" | b"NumberFormat" | b"Alignment"
                        if in_style && !values_only =>
                    {
                        apply_style_tag(n, &e, &mut cur_style);
                    }
                    b"Table" => {
                        let (er, ec) = index_attr(&e, b"ExpandedRowCount", 0)
                            .zip(index_attr(&e, b"ExpandedColumnCount", 0))?;
                        max_rows = max_rows.max(er);
                        max_cols = max_cols.max(ec);
                        if max_rows > MAX_R || max_cols > MAX_C {
                            return None;
                        }
                        if attr_cow(&e, b"FullColumns").as_deref() == Some("1") || er == MAX_R {
                            full_columns = true;
                        }
                        table_body_start = Some(r.buffer_position() as usize);
                        break;
                    }
                    _ => {}
                }
            }
            Ok(Event::Empty(e)) => {
                let qname = e.name();
                let n = local_bytes(qname.as_ref().as_bytes());
                if in_style && !values_only {
                    apply_style_tag(n, &e, &mut cur_style);
                } else if n == b"Date1904" {
                    date1904 = true;
                } else if n == b"Table" {
                    let (er, ec) = index_attr(&e, b"ExpandedRowCount", 0)
                        .zip(index_attr(&e, b"ExpandedColumnCount", 0))?;
                    max_rows = max_rows.max(er);
                    max_cols = max_cols.max(ec);
                    if max_rows > MAX_R || max_cols > MAX_C || max_rows == 0 || max_cols == 0 {
                        return None;
                    }
                    if attr_cow(&e, b"FullColumns").as_deref() == Some("1") || er == MAX_R {
                        full_columns = true;
                    }
                    empty_table = true;
                    break;
                }
            }
            Ok(Event::End(e)) => {
                let qname = e.name();
                let n = local_bytes(qname.as_ref().as_bytes());
                if n == b"Style" && !values_only {
                    in_style = false;
                    if cur_style == Style::default() {
                        style_lookup.insert(std::mem::take(&mut style_id), 0);
                    } else {
                        let idx = styles.len() as u32;
                        styles.push(std::mem::take(&mut cur_style));
                        style_lookup.insert(std::mem::take(&mut style_id), idx);
                    }
                }
            }
            Ok(Event::Eof) => return None,
            Err(_) => return None,
            _ => {}
        }
    }

    if empty_table {
        return Some(BorrowedSparseBlock {
            rows: max_rows as u32,
            cols: max_cols as u32,
            full_columns,
            date1904,
            styles,
            cells: Vec::new(),
        });
    }

    let start_pos = table_body_start?;
    let b = xml.as_bytes();
    let len = b.len();
    let mut i = start_pos;
    let mut col_cursor = 0usize;
    let mut row_cursor = 0usize;
    let mut in_row = false;
    let mut row_index = 0usize;
    let mut row_span = 1usize;
    let mut row_style_idx: Option<u32> = None;
    let mut next_col = 0usize;
    let est = ((len.saturating_sub(start_pos)) / 48).min(crate::undo::MAX_DENSE_RECT_KEYS);
    let mut cells: Vec<BorrowedSparseCell<'a>> = Vec::with_capacity(est);

    while i < len {
        let Some(lt_rel) = crate::xlsx::sheets::stream::simd_memchr(b'<', &b[i..]) else {
            break;
        };
        i += lt_rel + 1;
        if i >= len {
            break;
        }
        let first = b[i];
        if first == b'!' || first == b'?' {
            return parse_sparse(xml).map(|owned| BorrowedSparseBlock {
                rows: owned.rows,
                cols: owned.cols,
                full_columns: owned.full_columns,
                date1904: owned.date1904,
                styles: owned.styles,
                cells: owned
                    .cells
                    .into_iter()
                    .map(|c| BorrowedSparseCell {
                        row: c.row,
                        col: c.col,
                        text: std::borrow::Cow::Owned(c.text),
                        kind: c.kind,
                        r1c1_formula: c.r1c1_formula.map(std::borrow::Cow::Owned),
                        style_idx: c.style_idx,
                        has_explicit_style: c.has_explicit_style,
                        colspan: c.colspan,
                        rowspan: c.rowspan,
                    })
                    .collect(),
            });
        }
        if first == b'/' {
            i += 1;
            let nstart = i;
            while i < len && !matches!(b[i], b'>' | b' ' | b'\t' | b'\r' | b'\n') {
                i += 1;
            }
            let close_local = local_bytes(&b[nstart..i]);
            i = skip_ws(b, i);
            if i >= len || b[i] != b'>' {
                return None;
            }
            i += 1;
            match close_local {
                b"Row" => {
                    if !in_row {
                        return None;
                    }
                    in_row = false;
                    row_cursor = row_index + row_span;
                    row_style_idx = None;
                }
                b"Table" => {
                    if in_row || max_rows == 0 || max_cols == 0 {
                        return None;
                    }
                    return Some(BorrowedSparseBlock {
                        rows: max_rows as u32,
                        cols: max_cols as u32,
                        full_columns,
                        date1904,
                        styles,
                        cells,
                    });
                }
                _ => return None,
            }
            continue;
        }
        let nstart = i;
        while i < len && !is_name_end(b[i]) {
            i += 1;
        }
        match local_bytes(&b[nstart..i]) {
            b"Row" => {
                if in_row {
                    return None;
                }
                if i < len && b[i] == b'>' {
                    i += 1;
                    let ri = row_cursor;
                    max_rows = max_rows.max(ri.checked_add(1)?);
                    max_cols = max_cols.max(1);
                    if max_rows > MAX_R || max_cols > MAX_C {
                        return None;
                    }
                    in_row = true;
                    row_index = ri;
                    row_span = 1;
                    row_style_idx = None;
                    next_col = 0;
                    continue;
                }
                let mut idx_attr = None;
                let mut span_attr = 0usize;
                let mut sty_attr = None;
                while let Ok(Some((k, v))) = next_attr(b, &mut i) {
                    match k {
                        b"Index" => idx_attr = Some(parse_ascii_usize(v)?),
                        b"Span" => span_attr = parse_ascii_usize(v)?,
                        b"StyleID" if !values_only => {
                            if let Ok(s) = std::str::from_utf8(v) {
                                let cow = unescape_cow(std::borrow::Cow::Borrowed(s));
                                sty_attr = style_lookup.get(cow.as_ref()).copied().filter(|&id| id != 0);
                            }
                        }
                        _ => {}
                    }
                }
                let Ok(self_closing) = finish_tag_head(b, &mut i) else {
                    return None;
                };
                let ri = idx_attr.unwrap_or(row_cursor + 1).checked_sub(1)?;
                let span = span_attr.checked_add(1)?;
                if ri < row_cursor {
                    return None;
                }
                max_rows = max_rows.max(ri.checked_add(span)?);
                max_cols = max_cols.max(1);
                if max_rows > MAX_R || max_cols > MAX_C {
                    return None;
                }
                if self_closing {
                    row_cursor = ri + span;
                } else {
                    in_row = true;
                    row_index = ri;
                    row_span = span;
                    row_style_idx = sty_attr;
                    next_col = 0;
                }
            }
            b"Cell" => {
                if !in_row {
                    return None;
                }
                let mut idx_attr = None;
                let mut across_attr = 0usize;
                let mut down_attr = 0usize;
                let mut raw_style: Option<&'a [u8]> = None;
                let mut raw_formula: Option<&'a [u8]> = None;
                let self_closing = if i < len && b[i] == b'>' {
                    i += 1;
                    false
                } else {
                    while let Ok(Some((k, v))) = next_attr(b, &mut i) {
                        match k {
                            b"Index" => idx_attr = Some(parse_ascii_usize(v)?),
                            b"MergeAcross" => across_attr = parse_ascii_usize(v)?,
                            b"MergeDown" => down_attr = parse_ascii_usize(v)?,
                            b"StyleID" if !values_only => raw_style = Some(v),
                            b"Formula" if !values_only => raw_formula = Some(v),
                            _ => {}
                        }
                    }
                    let Ok(sc) = finish_tag_head(b, &mut i) else {
                        return None;
                    };
                    sc
                };
                let col = idx_attr.unwrap_or(next_col + 1).checked_sub(1)?;
                let across = across_attr.checked_add(1)?;
                let down = down_attr.checked_add(1)?;
                if col < next_col {
                    return None;
                }
                max_rows = max_rows.max(row_index.checked_add(down)?);
                max_cols = max_cols.max(col.checked_add(across)?);
                if max_rows > MAX_R || max_cols > MAX_C {
                    return None;
                }
                next_col = col + across;
                let style_idx = if values_only {
                    0
                } else {
                    raw_style
                        .and_then(|v| std::str::from_utf8(v).ok())
                        .and_then(|s| {
                            let cow = unescape_cow(std::borrow::Cow::Borrowed(s));
                            style_lookup.get(cow.as_ref()).copied()
                        })
                        .or(row_style_idx)
                        .or_else(|| col_styles.get(&(col as u32)).copied())
                        .unwrap_or(0)
                };
                let has_explicit_style = !values_only && (raw_style.is_some() || style_idx != 0);
                let r1c1_formula = raw_formula
                    .and_then(|v| std::str::from_utf8(v).ok())
                    .map(|s| unescape_cow(std::borrow::Cow::Borrowed(s)));

                let mut text: std::borrow::Cow<'a, str> = std::borrow::Cow::Borrowed("");
                let mut kind = Kind::Number;
                let mut had_data = false;

                if !self_closing {
                    let d_pos = skip_ws(b, i);
                    let fast_type = if b[d_pos..].starts_with(b"<Data ss:Type=\"") {
                        let ts = d_pos + 15;
                        if b[ts..].starts_with(b"Number\">") {
                            Some((Kind::Number, ts + 8))
                        } else if b[ts..].starts_with(b"String\">") {
                            Some((Kind::Text, ts + 8))
                        } else if b[ts..].starts_with(b"Boolean\">") {
                            Some((Kind::Bool, ts + 9))
                        } else if b[ts..].starts_with(b"DateTime\">") {
                            Some((Kind::DateTime, ts + 10))
                        } else if b[ts..].starts_with(b"Error\">") {
                            Some((Kind::Error, ts + 7))
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    if let Some((dtype, tstart)) = fast_type {
                        let mut scan = tstart;
                        let mut has_amp = false;
                        let val_end = loop {
                            match crate::xlsx::sheets::stream::simd_memchr2(b'<', b'&', &b[scan..]) {
                                Some(rel) => {
                                    let hit = scan + rel;
                                    if b[hit] == b'<' {
                                        break Some(hit);
                                    }
                                    has_amp = true;
                                    scan = hit + 1;
                                }
                                None => break None,
                            }
                        };
                        let vend = val_end?;
                        if !b[vend..].starts_with(b"</Data></Cell>") {
                            return parse_sparse(xml).map(|owned| BorrowedSparseBlock {
                                rows: owned.rows,
                                cols: owned.cols,
                                full_columns: owned.full_columns,
                                date1904: owned.date1904,
                                styles: owned.styles,
                                cells: owned
                                    .cells
                                    .into_iter()
                                    .map(|c| BorrowedSparseCell {
                                        row: c.row,
                                        col: c.col,
                                        text: std::borrow::Cow::Owned(c.text),
                                        kind: c.kind,
                                        r1c1_formula: c.r1c1_formula.map(std::borrow::Cow::Owned),
                                        style_idx: c.style_idx,
                                        has_explicit_style: c.has_explicit_style,
                                        colspan: c.colspan,
                                        rowspan: c.rowspan,
                                    })
                                    .collect(),
                            });
                        }
                        let raw_str = &xml[tstart..vend];
                        text = if has_amp {
                            std::borrow::Cow::Owned(quick_xml::escape::unescape(raw_str).ok()?.into_owned())
                        } else {
                            std::borrow::Cow::Borrowed(raw_str)
                        };
                        kind = dtype;
                        had_data = true;
                        i = vend + 14;
                    } else {
                        return parse_sparse(xml).map(|owned| BorrowedSparseBlock {
                            rows: owned.rows,
                            cols: owned.cols,
                            full_columns: owned.full_columns,
                            date1904: owned.date1904,
                            styles: owned.styles,
                            cells: owned
                                .cells
                                .into_iter()
                                .map(|c| BorrowedSparseCell {
                                    row: c.row,
                                    col: c.col,
                                    text: std::borrow::Cow::Owned(c.text),
                                    kind: c.kind,
                                    r1c1_formula: c.r1c1_formula.map(std::borrow::Cow::Owned),
                                    style_idx: c.style_idx,
                                    has_explicit_style: c.has_explicit_style,
                                    colspan: c.colspan,
                                    rowspan: c.rowspan,
                                })
                                .collect(),
                        });
                    }
                }

                let meaningful = had_data
                    || !text.is_empty()
                    || r1c1_formula.is_some()
                    || has_explicit_style
                    || (!values_only && (across > 1 || down > 1));
                if meaningful {
                    if cells.len() >= crate::undo::MAX_DENSE_RECT_KEYS {
                        return None;
                    }
                    cells.push(BorrowedSparseCell {
                        row: row_index as u32,
                        col: col as u32,
                        text,
                        kind,
                        r1c1_formula,
                        style_idx,
                        has_explicit_style,
                        colspan: if values_only { 1 } else { across as u32 },
                        rowspan: if values_only { 1 } else { down as u32 },
                    });
                }
            }
            b"Column" => {
                if in_row {
                    return None;
                }
                let mut idx_attr = None;
                let mut span_attr = 0usize;
                let mut raw_style: Option<&'a [u8]> = None;
                while let Ok(Some((k, v))) = next_attr(b, &mut i) {
                    match k {
                        b"Index" => idx_attr = Some(parse_ascii_usize(v)?),
                        b"Span" => span_attr = parse_ascii_usize(v)?,
                        b"StyleID" if !values_only => raw_style = Some(v),
                        _ => {}
                    }
                }
                let Ok(self_closing) = finish_tag_head(b, &mut i) else {
                    return None;
                };
                if !self_closing {
                    let lt_rel = crate::xlsx::sheets::stream::simd_memchr(b'<', &b[i..])?;
                    i += lt_rel;
                    if !b[i..].starts_with(b"</") {
                        return None;
                    }
                    i += 2;
                    let cs = i;
                    while i < len && !matches!(b[i], b'>' | b' ' | b'\t' | b'\r' | b'\n') {
                        i += 1;
                    }
                    if local_bytes(&b[cs..i]) != b"Column" {
                        return None;
                    }
                    i = skip_ws(b, i);
                    if i >= len || b[i] != b'>' {
                        return None;
                    }
                    i += 1;
                }
                let c_idx = idx_attr.unwrap_or(col_cursor + 1).checked_sub(1)?;
                let c_span = span_attr.checked_add(1)?;
                if c_idx < col_cursor {
                    return None;
                }
                max_cols = max_cols.max(c_idx.checked_add(c_span)?);
                if max_cols > MAX_C {
                    return None;
                }
                if !values_only
                    && let Some(sid) = raw_style
                        .and_then(|v| std::str::from_utf8(v).ok())
                        .and_then(|s| {
                            let cow = unescape_cow(std::borrow::Cow::Borrowed(s));
                            style_lookup.get(cow.as_ref()).copied()
                        })
                    && sid != 0
                {
                    for cc in c_idx..(c_idx + c_span) {
                        col_styles.insert(cc as u32, sid);
                    }
                }
                col_cursor = c_idx + c_span;
            }
            _ => return None,
        }
    }
    None
}

/// Parse XMLSS into an owned `SparseBlock`, skipping `<Styles>`, `StyleID`, `Formula`,
/// and merge attributes when `values_only == true` (e.g. `Ctrl+Shift+V` paste-values).
pub fn parse_sparse_ext(xml: &str, values_only: bool) -> Option<SparseBlock> {
    if !values_only {
        return parse_sparse(xml);
    }
    let borrowed = parse_sparse_borrowed(xml, true)?;
    Some(SparseBlock {
        rows: borrowed.rows,
        cols: borrowed.cols,
        full_columns: borrowed.full_columns,
        date1904: borrowed.date1904,
        styles: borrowed.styles,
        cells: borrowed
            .cells
            .into_iter()
            .map(|c| SparseCell {
                row: c.row,
                col: c.col,
                text: c.text.into_owned(),
                kind: c.kind,
                r1c1_formula: None,
                style_idx: 0,
                has_explicit_style: false,
                colspan: 1,
                rowspan: 1,
            })
            .collect(),
    })
}

/// Apply a pre-parsed `BorrowedSparseBlock` onto `(wb, sheet_idx)` anchored at `(r0, c0)`
/// using `fast_parse_f64_bytes`, `StringPool` interning, `(base_id, style_idx)` style cache,
/// and column-ordered `(col, row)` `Sheet::set` writes so `ColSlab` appends stay cache-local.
pub fn apply_borrowed_sparse_to_sheet<'a>(
    block: &BorrowedSparseBlock<'a>,
    wb: &mut crate::model::Workbook,
    sheet_idx: usize,
    anchor: (u32, u32),
    values_only: bool,
) -> Option<(u32, u32)> {
    let (r0, c0) = anchor;
    if r0.checked_add(block.rows)? > crate::sync::MAX_ROWS
        || c0.checked_add(block.cols)? > crate::sync::MAX_COLS
    {
        return None;
    }
    let date_sys = wb.date_system;
    let mut default_date_sid: Option<u16> = None;
    let mut style_cache: std::collections::HashMap<(u16, u32), Option<u16>> =
        std::collections::HashMap::new();

    // Sort indices by `(col, row)` so writes hit contiguous 256-row column slabs in cache order.
    let mut order: Vec<usize> = (0..block.cells.len()).collect();
    order.sort_unstable_by_key(|&idx| {
        let c = &block.cells[idx];
        (c.col, c.row)
    });

    let mut str_pool: std::collections::HashMap<&'a str, std::sync::Arc<str>> =
        std::collections::HashMap::new();
    let mut owned_pool: std::collections::HashMap<String, std::sync::Arc<str>> =
        std::collections::HashMap::new();
    let mut intern_cow = |cow: &std::borrow::Cow<'a, str>| -> std::sync::Arc<str> {
        match cow {
            std::borrow::Cow::Borrowed(s) => str_pool
                .entry(s)
                .or_insert_with(|| std::sync::Arc::<str>::from(*s))
                .clone(),
            std::borrow::Cow::Owned(s) => {
                if let Some(found) = owned_pool.get(s.as_str()) {
                    found.clone()
                } else {
                    let arc = std::sync::Arc::<str>::from(s.as_str());
                    owned_pool.insert(s.clone(), arc.clone());
                    arc
                }
            }
        }
    };

    for idx in order {
        let c = &block.cells[idx];
        let tr = r0 + c.row;
        let tc = c0 + c.col;
        let formula = if values_only {
            None
        } else {
            c.r1c1_formula
                .as_deref()
                .map(|f| std::sync::Arc::<str>::from(r1c1_to_a1(f, tr, tc)))
        };
        let mut needs_iso_date_fmt = false;
        let val = match c.kind {
            Kind::Number => {
                let raw_bytes = c.text.as_bytes();
                let parsed = crate::xlsx::sheets::stream::fast_parse_f64_bytes(raw_bytes).or_else(
                    || {
                        raw_bytes
                            .contains(&b',')
                            .then(|| c.text.replace(',', "").trim().parse::<f64>().ok())
                            .flatten()
                    },
                );
                match parsed {
                    Some(n) => crate::model::Value::Number(n),
                    None if c.text.trim().is_empty() => crate::model::Value::Empty,
                    None if formula.is_some() => crate::model::Value::Empty,
                    None => crate::model::Value::Text(intern_cow(&c.text)),
                }
            }
            Kind::Text => {
                if c.text.is_empty() {
                    crate::model::Value::Empty
                } else {
                    crate::model::Value::Text(intern_cow(&c.text))
                }
            }
            Kind::Bool => {
                let t = c.text.trim();
                if t.is_empty() && formula.is_none() {
                    crate::model::Value::Empty
                } else {
                    crate::model::Value::Bool(t == "1" || t.eq_ignore_ascii_case("true"))
                }
            }
            Kind::DateTime => {
                let t = c.text.trim();
                if t.is_empty() && formula.is_none() {
                    crate::model::Value::Empty
                } else {
                    match iso_to_serial(date_sys, t) {
                        Some((serial, _)) => {
                            if !values_only && formula.is_none() {
                                let has_fmt = block
                                    .styles
                                    .get(c.style_idx as usize)
                                    .is_some_and(|st| !st.num_format.trim().is_empty());
                                if !has_fmt {
                                    needs_iso_date_fmt = true;
                                }
                            }
                            crate::model::Value::Number(serial)
                        }
                        None => crate::model::Value::Text(intern_cow(&c.text)),
                    }
                }
            }
            Kind::Error => {
                let t = c.text.trim();
                if t.is_empty() && formula.is_none() {
                    crate::model::Value::Empty
                } else {
                    match crate::formula::ErrKind::from_text(t) {
                        Some(ek) => crate::model::Value::Error(ek),
                        None => crate::model::Value::Text(intern_cow(&c.text)),
                    }
                }
            }
        };
        if !matches!(val, crate::model::Value::Empty) || formula.is_some() {
            wb.sheet_mut(sheet_idx)
                .set(tr, tc, crate::model::Cell { value: val, formula });
        }
        if !values_only {
            if needs_iso_date_fmt {
                let sid = *default_date_sid.get_or_insert_with(|| {
                    wb.styles.intern(crate::style::CellStyle {
                        format: "yyyy-mm-dd".into(),
                        ..Default::default()
                    })
                });
                wb.sheet_mut(sheet_idx).set_style(tr, tc, sid);
            }
            if c.style_idx != 0 {
                let base_id = wb.sheet(sheet_idx).style_of(tr, tc);
                let resolved = *style_cache
                    .entry((base_id, c.style_idx))
                    .or_insert_with(|| {
                        let st = block.styles.get(c.style_idx as usize)?;
                        let mut cs = wb.styles.get(base_id).clone();
                        let mut changed = false;
                        if st.bold != cs.bold {
                            cs.bold = st.bold;
                            changed = true;
                        }
                        if st.italic != cs.italic {
                            cs.italic = st.italic;
                            changed = true;
                        }
                        if let Some(fill) = st.fill {
                            cs.fill = Some(fill);
                            changed = true;
                        }
                        if let Some(color) = st.color {
                            cs.color = color;
                            changed = true;
                        }
                        if st.halign != 0 && st.halign != cs.halign {
                            cs.halign = st.halign;
                            changed = true;
                        }
                        if let Some(bc) = st.border {
                            for side in cs.border.iter_mut() {
                                if side.style == 0 {
                                    side.style = 1;
                                    side.color = bc;
                                }
                            }
                            changed = true;
                        }
                        if !st.font.is_empty() {
                            let f = crate::style::normalize_font(&st.font);
                            if f != cs.font {
                                cs.font = f;
                                changed = true;
                            }
                        }
                        if st.size > 0.0 && (st.size - cs.size).abs() > 0.01 {
                            cs.size = st.size;
                            changed = true;
                        }
                        if !st.num_format.is_empty() && st.num_format != cs.format {
                            cs.format = st.num_format.clone();
                            changed = true;
                        }
                        changed.then(|| wb.styles.intern(cs))
                    });
                if let Some(sid) = resolved {
                    wb.sheet_mut(sheet_idx).set_style(tr, tc, sid);
                }
            }
            if c.colspan > 1 || c.rowspan > 1 {
                wb.sheet_mut(sheet_idx)
                    .merges
                    .push((tr, tc, tr + c.rowspan - 1, tc + c.colspan - 1));
            }
        }
    }
    Some((block.rows, block.cols))
}

/// Apply an XMLSS payload directly onto `(wb, sheet_idx)` anchored at `(r0, c0)` using
/// zero-copy `parse_sparse_borrowed`, `fast_parse_f64_bytes`, `StringPool` interning,
/// and column-ordered `Sheet::set` writes so `ColSlab` appends stay cache-local.
pub fn apply_sparse_xmlss_to_sheet(
    xml: &str,
    wb: &mut crate::model::Workbook,
    sheet_idx: usize,
    anchor: (u32, u32),
    values_only: bool,
) -> Option<(u32, u32)> {
    let block = parse_sparse_borrowed(xml, values_only)?;
    apply_borrowed_sparse_to_sheet(&block, wb, sheet_idx, anchor, values_only)
}

fn hex_rgb(s: &str) -> Option<[u8; 3]> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some([r, g, b])
}

/// XML Spreadsheet 里既有内置格式名（Fixed/Standard/Percent…）也有自定义格式串，
/// 前者要翻成 Excel 的格式代码，后者原样保留
fn normalize_num_format(f: &str) -> String {
    let t = f.trim();
    match t.to_ascii_lowercase().as_str() {
        "" | "general" => String::new(),
        "fixed" => "0.00".into(),
        "standard" => "#,##0.00".into(),
        "percent" => "0.00%".into(),
        "scientific" => "0.00E+00".into(),
        "currency" => "¥#,##0.00".into(),
        "short date" | "medium date" => "yyyy-mm-dd".into(),
        "long date" => "yyyy年m月d日".into(),
        "time" => "h:mm:ss".into(),
        "short time" => "h:mm".into(),
        _ => t.to_string(),
    }
}

// ---------------------------------------------------------------- R1C1 → A1

/// `=RC[-1]+R[1]C[-1]` 以 (row0,col0) 为基准折成 `=A1+A2`
/// A1 公式 → R1C1（相对当前格）。给“复制到 Excel”的 XML Spreadsheet 用。
///
/// 只改单元格引用，字符串/函数名/数字原样保留；`$` 决定行列是否绝对。
/// 支持 `Sheet1!A1`、`'参数 表'!A1:B2` 这类带表名的引用。
pub fn a1_to_r1c1(f: &str, row0: u32, col0: u32) -> String {
    use std::fmt::Write as _;
    let bytes = f.as_bytes();
    let mut out = String::with_capacity(f.len() + 8);
    let mut i = 0usize;
    let mut in_double = false;
    let mut in_single = false;
    let mut prev_ident = false;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'"' {
            in_double = !in_double;
            out.push('"');
            prev_ident = false;
            i += 1;
            continue;
        }
        if b == b'\'' && !in_double {
            in_single = !in_single;
            out.push('\'');
            prev_ident = false;
            i += 1;
            continue;
        }
        if in_double || in_single {
            if b < 0x80 {
                out.push(b as char);
                i += 1;
            } else {
                let ch = f[i..].chars().next().unwrap();
                out.push(ch);
                i += ch.len_utf8();
            }
            prev_ident = false;
            continue;
        }
        if !prev_ident && (b == b'$' || b.is_ascii_alphabetic()) {
            let start = i;
            let mut j = i;
            let col_abs = bytes[j] == b'$';
            if col_abs {
                j += 1;
            }
            let letters_start = j;
            while j < bytes.len() && bytes[j].is_ascii_alphabetic() && j - letters_start < 3 {
                j += 1;
            }
            let letters_end = j;
            if letters_end > letters_start {
                let row_abs = j < bytes.len() && bytes[j] == b'$';
                let row_start = if row_abs { j + 1 } else { j };
                let mut k = row_start;
                while k < bytes.len() && bytes[k].is_ascii_digit() && k - row_start < 7 {
                    k += 1;
                }
                if k > row_start {
                    let next_ok = if k >= bytes.len() {
                        true
                    } else if bytes[k] < 0x80 {
                        !(bytes[k].is_ascii_alphanumeric() || bytes[k] == b'_' || bytes[k] == b'(')
                    } else {
                        !f[k..].chars().next().is_some_and(char::is_alphanumeric)
                    };
                    let mut col: u32 = 0;
                    let mut valid = true;
                    for &cb in &bytes[letters_start..letters_end] {
                        let d = u32::from(cb.to_ascii_uppercase() - b'A') + 1;
                        col = col * 26 + d;
                        if col > 16_384 {
                            valid = false;
                            break;
                        }
                    }
                    let mut row: u32 = 0;
                    for &rb in &bytes[row_start..k] {
                        row = row.saturating_mul(10).saturating_add(u32::from(rb - b'0'));
                    }
                    if valid && (1..=1_048_576).contains(&row) && next_ok {
                        if row_abs {
                            let _ = write!(out, "R{row}");
                        } else if row - 1 == row0 {
                            out.push('R');
                        } else {
                            let _ = write!(out, "R[{}]", row as i64 - 1 - row0 as i64);
                        }
                        if col_abs {
                            let _ = write!(out, "C{col}");
                        } else if col - 1 == col0 {
                            out.push('C');
                        } else {
                            let _ = write!(out, "C[{}]", col as i64 - 1 - col0 as i64);
                        }
                        prev_ident = true;
                        i = k;
                        continue;
                    }
                }
            }
            let ch = f[start..].chars().next().unwrap();
            out.push(ch);
            prev_ident = ch.is_alphanumeric() || ch == '_' || ch == '.';
            i = start + ch.len_utf8();
            continue;
        }
        if b < 0x80 {
            let ch = b as char;
            out.push(ch);
            prev_ident = ch.is_ascii_alphanumeric() || ch == '_' || ch == '.';
            i += 1;
        } else {
            let ch = f[i..].chars().next().unwrap();
            out.push(ch);
            prev_ident = ch.is_alphanumeric() || ch == '_' || ch == '.';
            i += ch.len_utf8();
        }
    }
    out
}

pub fn r1c1_to_a1(f: &str, row0: u32, col0: u32) -> String {
    let bytes = f.as_bytes();
    let mut out = String::with_capacity(f.len() + 8);
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'"' {
            let start = i;
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'"' {
                    if bytes.get(i + 1) == Some(&b'"') {
                        i += 2;
                        continue;
                    }
                    i += 1;
                    break;
                }
                i += 1;
            }
            out.push_str(&f[start..i]);
            continue;
        }
        if b == b'R' || b == b'r' {
            let (rend, rpart) = parse_offset_bytes(bytes, i + 1);
            if matches!(bytes.get(rend), Some(b'C' | b'c')) {
                let (cend, cpart) = parse_offset_bytes(bytes, rend + 1);
                push_a1(&mut out, &rpart, &cpart, row0, col0);
                i = cend;
                continue;
            }
            out.push(b as char);
            i += 1;
            continue;
        }
        if b < 0x80 {
            out.push(b as char);
            i += 1;
        } else {
            let ch = f[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

/// R/C 后面的偏移：`[±n]` 相对、`n` 绝对、什么都没有就是相对 0
#[derive(Debug, Clone, Copy)]
struct Part {
    abs: bool,
    n: i64,
}

fn parse_offset_bytes(bytes: &[u8], i: usize) -> (usize, Part) {
    let mut j = i;
    if j < bytes.len() && bytes[j] == b'[' {
        let mut k = j + 1;
        let mut sign = 1i64;
        if k < bytes.len() && (bytes[k] == b'-' || bytes[k] == b'+') {
            if bytes[k] == b'-' {
                sign = -1;
            }
            k += 1;
        }
        let start = k;
        let mut acc = 0i64;
        while k < bytes.len() && bytes[k].is_ascii_digit() {
            acc = acc.saturating_mul(10).saturating_add(i64::from(bytes[k] - b'0'));
            k += 1;
        }
        if k > start && k < bytes.len() && bytes[k] == b']' {
            return (k + 1, Part { abs: false, n: sign * acc });
        }
        return (j, Part { abs: false, n: 0 });
    }
    let start = j;
    let mut acc = 0i64;
    while j < bytes.len() && bytes[j].is_ascii_digit() {
        acc = acc.saturating_mul(10).saturating_add(i64::from(bytes[j] - b'0'));
        j += 1;
    }
    if j > start {
        return (j, Part { abs: true, n: acc.max(1) });
    }
    (i, Part { abs: false, n: 0 })
}

/// 绝对行写成 `B$3`（列绝对才是 `$B3`）
fn push_a1(out: &mut String, r: &Part, c: &Part, row0: u32, col0: u32) {
    use std::fmt::Write as _;
    let row = (if r.abs { r.n } else { row0 as i64 + r.n + 1 }).max(1);
    let col = (if c.abs { c.n } else { col0 as i64 + c.n + 1 }).max(1);
    if c.abs {
        out.push('$');
    }
    let mut cv = col as u32;
    let mut col_buf = [0u8; 4];
    let mut clen = 0usize;
    while cv > 0 {
        col_buf[clen] = b'A' + ((cv - 1) % 26) as u8;
        clen += 1;
        cv = (cv - 1) / 26;
    }
    for idx in (0..clen).rev() {
        out.push(col_buf[idx] as char);
    }
    if r.abs {
        out.push('$');
    }
    let _ = write!(out, "{row}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_relative_and_absolute() {
        // B1（row0=0,col0=1）
        assert_eq!(r1c1_to_a1("=RC[-1]+R[1]C[-1]", 0, 1), "=A1+A2");
        // B2
        assert_eq!(r1c1_to_a1("=R[-1]C[-1]*10", 1, 1), "=A1*10");
        // 绝对
        assert_eq!(r1c1_to_a1("=R1C1+R2C3", 5, 5), "=$A$1+$C$2");
        // 混合
        assert_eq!(r1c1_to_a1("=R3C[-1]", 0, 2), "=B$3");
    }

    #[test]
    fn keeps_strings_and_functions() {
        assert_eq!(r1c1_to_a1("=\"R2C3\"&RC[-1]", 0, 1), "=\"R2C3\"&A1");
        assert_eq!(r1c1_to_a1("=ROUND(RC[-1],2)", 0, 3), "=ROUND(C1,2)");
        assert_eq!(r1c1_to_a1("=SUM(R[-1]C:R[1]C)", 2, 2), "=SUM(C2:C4)");
    }

    #[test]
    fn parses_real_clipboard_sample() {
        let xml = r##"<?xml version="1.0"?>
<?mso-application progid="Excel.Sheet"?>
<Workbook xmlns="urn:schemas-microsoft-com:office:spreadsheet"
 xmlns:ss="urn:schemas-microsoft-com:office:spreadsheet">
 <Styles>
  <Style ss:ID="Default" ss:Name="Normal"><Font ss:FontName="等线" ss:Size="11" ss:Color="#000000"/></Style>
  <Style ss:ID="s63"><Font ss:FontName="等线" ss:Size="11" ss:Color="#000000" ss:Bold="1"/><Interior ss:Color="#FFFF00" ss:Pattern="Solid"/></Style>
 </Styles>
 <Worksheet ss:Name="Sheet1">
  <Table ss:ExpandedColumnCount="2" ss:ExpandedRowCount="2">
   <Row>
    <Cell ss:StyleID="s63"><Data ss:Type="Number">1</Data></Cell>
    <Cell ss:StyleID="s63" ss:Formula="=RC[-1]+R[1]C[-1]"><Data ss:Type="Number">3</Data></Cell>
   </Row>
   <Row>
    <Cell ss:StyleID="s63"><Data ss:Type="Number">2</Data></Cell>
    <Cell ss:StyleID="s63" ss:Formula="=R[-1]C[-1]*10"><Data ss:Type="Number">10</Data></Cell>
   </Row>
  </Table>
 </Worksheet>
</Workbook>"##;
        let b = parse(xml);
        assert_eq!(b.rows.len(), 2);
        assert_eq!(b.rows[0].len(), 2);
        assert_eq!(b.rows[0][1].formula.as_deref(), Some("=A1+A2"));
        assert_eq!(b.rows[0][1].text, "3");
        assert_eq!(b.rows[1][1].formula.as_deref(), Some("=A1*10"));
        assert_eq!(b.rows[0][0].style.bold, true);
        assert_eq!(b.rows[0][0].style.fill, Some([255, 255, 0]));
        assert_eq!(b.rows[0][0].style.font, "等线");
        assert_eq!(b.rows[0][0].style.size, 11.0);
        // 内置格式名要翻成格式代码
        assert_eq!(normalize_num_format("Fixed"), "0.00");
        assert_eq!(normalize_num_format("General"), "");
        assert_eq!(normalize_num_format("#,##0"), "#,##0");
    }

    #[test]
    fn parses_merges_and_text() {
        let xml = r##"<Workbook xmlns:ss="urn:schemas-microsoft-com:office:spreadsheet"><Worksheet><Table>
<Row><Cell ss:MergeAcross="1" ss:MergeDown="1"><Data ss:Type="String">合计</Data></Cell><Cell ss:Index="4"><Data ss:Type="Boolean">1</Data></Cell></Row>
</Table></Worksheet></Workbook>"##;
        let b = parse(xml);
        assert_eq!(b.rows[0][0].colspan, 2);
        assert_eq!(b.rows[0][0].rowspan, 2);
        assert_eq!(b.rows[0][0].kind, Kind::Text);
        assert_eq!(b.rows[0][0].text, "合计");
        assert_eq!(b.rows[0].len(), 3, "合并格占两列，D 列之前只补 C 列空格");
        assert_eq!(b.rows[0][2].kind, Kind::Bool);
    }

    #[test]
    fn keeps_char_refs_in_cell_text() {
        let xml = r##"<Workbook xmlns:ss="urn:schemas-microsoft-com:office:spreadsheet"><Worksheet><Table>
<Row><Cell><Data ss:Type="String">第一行&#10;第二行 &amp; 结束</Data></Cell></Row>
</Table></Worksheet></Workbook>"##;
        let b = parse(xml);
        assert_eq!(b.rows[0][0].text, "第一行\n第二行 & 结束");
    }

    #[test]
    fn sparse_indices_are_exact_or_the_whole_block_is_rejected() {
        let xml = |table: &str, rows: &str| format!(
            "<Workbook xmlns:ss='urn:schemas-microsoft-com:office:spreadsheet'><Worksheet><Table {table}>{rows}</Table></Worksheet></Workbook>"
        );
        let row = "<Row ss:Index='20001'><Cell ss:Index='3' ss:Formula='=RC[-1]'/></Row>";
        let block = parse(&xml("", row));
        assert_eq!(block.rows.len(), 20001);
        assert_eq!(block.rows[20000][2].formula.as_deref(), Some("=B20001"));
        assert!(parse(&xml("ss:ExpandedRowCount='1048576' ss:ExpandedColumnCount='3'", row)).is_empty());
        for bad in ["<Row ss:Index='1048576'><Cell/></Row>", "<Row><Cell ss:Index='16385'/></Row>",
            "<Row><Cell ss:MergeDown='4294967295'/></Row>", "<Row ss:Index='0'/>"] {
            assert!(parse(&xml("", &format!("<Row><Cell/></Row>{bad}"))).is_empty(), "{bad}");
        }
    }

    #[test]
    fn a1_formula_converts_to_relative_r1c1() {
        assert_eq!(a1_to_r1c1("=B1*2", 1, 1), "=R[-1]C*2");
        assert_eq!(a1_to_r1c1("=SUM(A1:A4)", 3, 0), "=SUM(R[-3]C:RC)");
        assert_eq!(a1_to_r1c1("=$A$1+C2", 0, 1), "=R1C1+R[1]C[1]");
        assert_eq!(a1_to_r1c1("=Sheet1!A1", 0, 0), "=Sheet1!RC");
        assert_eq!(a1_to_r1c1("='参数 表'!B2", 0, 0), "='参数 表'!R[1]C[1]");
        // 字符串里的 A1 不动；A1:B2 两个端点都转
        assert_eq!(a1_to_r1c1("=\"A1\"&A1", 0, 0), "=\"A1\"&RC");
        assert_eq!(a1_to_r1c1("=A1:B2", 0, 0), "=RC:R[1]C[1]");
    }

    #[test]
    fn parse_sparse_handles_full_columns_and_million_cell_sparse_blocks() {
        let mut wb = crate::model::Workbook::new_blank();
        let bold = wb.styles.intern(crate::style::CellStyle {
            bold: true,
            fill: Some([255, 255, 0]),
            ..Default::default()
        });
        let sh = &mut wb.sheets[0];
        sh.set(0, 0, crate::model::Cell {
            value: crate::model::Value::Number(10.0),
            formula: None,
        });
        sh.set_style(0, 0, bold);
        sh.set(900_000, 1, crate::model::Cell {
            value: crate::model::Value::Number(20.0),
            formula: Some("=A$1*2".into()),
        });
        let xml = sparse_column_xmlss(&wb, 0, (0, 0, 1_048_575, 2)).unwrap();
        // Dense `parse` still rejects >500k extent per existing contract
        assert!(parse(&xml).is_empty());
        // Sparse `parse_sparse` parses full-column XMLSS without dense allocation
        let sp = parse_sparse(&xml).expect("parse_sparse should accept 1,048,576-row full columns");
        assert_eq!(sp.rows, 1_048_576);
        assert_eq!(sp.cols, 3);
        assert!(sp.full_columns);
        assert_eq!(sp.cells.len(), 2);
        assert_eq!((sp.cells[0].row, sp.cells[0].col), (0, 0));
        assert!(sp.styles[sp.cells[0].style_idx as usize].bold);
        assert_eq!((sp.cells[1].row, sp.cells[1].col), (900_000, 1));
        assert_eq!(sp.cells[1].r1c1_formula.as_deref(), Some("=R1C[-1]*2"));

        // Invalid / backward / out-of-bounds indices are still strictly rejected
        for bad in [
            "<Workbook xmlns:ss='urn:schemas-microsoft-com:office:spreadsheet'><Worksheet><Table><Row ss:Index='1048577'><Cell/></Row></Table></Worksheet></Workbook>",
            "<Workbook xmlns:ss='urn:schemas-microsoft-com:office:spreadsheet'><Worksheet><Table><Row><Cell ss:Index='16385'/></Row></Table></Worksheet></Workbook>",
            "<Workbook xmlns:ss='urn:schemas-microsoft-com:office:spreadsheet'><Worksheet><Table><Row ss:Index='5'><Cell/></Row><Row ss:Index='3'><Cell/></Row></Table></Worksheet></Workbook>",
        ] {
            assert!(parse_sparse(bad).is_none(), "should reject {bad}");
        }
    }

    #[test]
    fn borrowed_sparse_values_only_and_column_grouped_apply() {
        let xml = r##"<?xml version="1.0"?>
<Workbook xmlns="urn:schemas-microsoft-com:office:spreadsheet"
 xmlns:ss="urn:schemas-microsoft-com:office:spreadsheet">
 <Styles>
  <Style ss:ID="Default" ss:Name="Normal"/>
  <Style ss:ID="s1"><Font ss:Bold="1"/><Interior ss:Color="#00FF00" ss:Pattern="Solid"/></Style>
 </Styles>
 <Worksheet ss:Name="Sheet1">
  <Table ss:ExpandedColumnCount="2" ss:ExpandedRowCount="2">
   <Row>
    <Cell ss:StyleID="s1" ss:MergeAcross="1"><Data ss:Type="String">标题</Data></Cell>
   </Row>
   <Row>
    <Cell ss:StyleID="s1"><Data ss:Type="Number">12.5</Data></Cell>
    <Cell ss:StyleID="s1" ss:Formula="=RC[-1]*2"><Data ss:Type="Number">25</Data></Cell>
   </Row>
  </Table>
 </Worksheet>
</Workbook>"##;

        let borrowed = parse_sparse_borrowed(xml, false).unwrap();
        assert!(matches!(borrowed.cells[0].text, std::borrow::Cow::Borrowed("标题")));
        assert!(matches!(borrowed.cells[1].text, std::borrow::Cow::Borrowed("12.5")));

        // Full apply (formulas + styles + merges + default font/size preservation)
        let mut wb_full = crate::model::Workbook::new_blank();
        assert_eq!(apply_sparse_xmlss_to_sheet(xml, &mut wb_full, 0, (2, 3), false), Some((2, 2)));
        assert_eq!(wb_full.sheets[0].value(2, 3), crate::model::Value::Text("标题".into()));
        assert_eq!(wb_full.sheets[0].merges, vec![(2, 3, 2, 4)]);
        let s33 = wb_full.styles.get(wb_full.sheets[0].style_of(3, 3));
        assert!(s33.bold);
        assert!((s33.size - 11.0).abs() < 0.01, "must keep default 11pt font size, got {}", s33.size);
        assert_eq!(s33.fill, Some([0, 255, 0]));
        assert_eq!(
            wb_full.sheets[0].get(3, 4).unwrap().formula.as_deref(),
            Some("=D4*2")
        );

        // Values-only apply & parse_sparse_ext (skips styles, formulas, and merges)
        let sparse_vo = parse_sparse_ext(xml, true).unwrap();
        assert_eq!(sparse_vo.styles.len(), 1);
        assert!(sparse_vo.cells.iter().all(|c| c.r1c1_formula.is_none() && c.style_idx == 0));

        let mut wb_val = crate::model::Workbook::new_blank();
        assert_eq!(apply_sparse_xmlss_to_sheet(xml, &mut wb_val, 0, (2, 3), true), Some((2, 2)));
        assert_eq!(wb_val.sheets[0].value(3, 3), crate::model::Value::Number(12.5));
        assert_eq!(wb_val.sheets[0].value(3, 4), crate::model::Value::Number(25.0));
        assert!(wb_val.sheets[0].get(3, 4).unwrap().formula.is_none());
        assert_eq!(wb_val.sheets[0].style_of(3, 3), 0);
        assert!(wb_val.sheets[0].merges.is_empty());
    }
}
