//! CSV / TSV 解析：替代 Power Query 的“外部数据”入口。
//!
//! 支持：逗号/制表符/分号自动判别、双引号包裹、字段内换行、`""` 转义。

/// 猜分隔符：看表头一行里哪个出现最多
pub fn sniff(text: &str) -> char {
    let first = text.lines().next().unwrap_or("");
    let count = |c: char| first.matches(c).count();
    let (comma, tab, semi) = (count(','), count('\t'), count(';'));
    if tab > comma && tab >= semi {
        '\t'
    } else if semi > comma {
        ';'
    } else {
        ','
    }
}

/// 解析成二维字符串；空行跳过
pub fn parse(text: &str, delim: char) -> Vec<Vec<String>> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut chars = text.chars().peekable();

    while let Some(ch) = chars.next() {
        if in_quotes {
            if ch == '"' {
                if chars.peek() == Some(&'"') {
                    field.push('"');
                    chars.next();
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(ch);
            }
            continue;
        }
        match ch {
            '"' if field.is_empty() => in_quotes = true,
            c if c == delim => {
                row.push(std::mem::take(&mut field));
            }
            '\r' => {}
            '\n' => {
                row.push(std::mem::take(&mut field));
                if !(row.len() == 1 && row[0].is_empty()) {
                    rows.push(std::mem::take(&mut row));
                } else {
                    row.clear();
                }
            }
            c => field.push(c),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        if !(row.len() == 1 && row[0].is_empty()) {
            rows.push(row);
        }
    }
    rows
}

/// 一次性：自动判分隔符再解析
pub fn parse_auto(text: &str) -> Vec<Vec<String>> {
    parse(text, sniff(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_csv() {
        let rows = parse_auto("姓名,部门\n张三,研发\n李四,销售\n");
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1], vec!["张三", "研发"]);
    }

    #[test]
    fn handles_quotes_and_commas_inside() {
        let rows = parse_auto("名称,备注\n\"甲,乙\",\"他说\"\"好\"\"\"\n");
        assert_eq!(rows[1][0], "甲,乙");
        assert_eq!(rows[1][1], "他说\"好\"");
    }

    #[test]
    fn handles_newline_inside_quotes() {
        let rows = parse_auto("a,b\n\"第一行\n第二行\",c\n");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1][0], "第一行\n第二行");
    }

    #[test]
    fn detects_tab_and_semicolon() {
        assert_eq!(sniff("a\tb\tc\n1\t2\t3"), '\t');
        assert_eq!(sniff("a;b;c\n1;2;3"), ';');
        assert_eq!(sniff("a,b\n1,2"), ',');
    }

    #[test]
    fn skips_blank_lines() {
        let rows = parse_auto("a,b\n\n\n1,2\n");
        assert_eq!(rows.len(), 2);
    }
}

/// CSV 文本编码。BOM 优先，其次按 UTF-8 严格解码，失败再按 GB18030。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    Utf16Le,
    Utf16Be,
    Gb18030,
}

impl Encoding {
    pub fn id(self) -> &'static str {
        match self {
            Encoding::Utf8 => "utf-8",
            Encoding::Utf16Le => "utf-16le",
            Encoding::Utf16Be => "utf-16be",
            Encoding::Gb18030 => "gb18030",
        }
    }
    pub fn from_id(s: &str) -> Option<Encoding> {
        match s.trim().to_ascii_lowercase().as_str() {
            "" | "auto" | "utf-8" | "utf8" => Some(Encoding::Utf8),
            "utf-16le" | "utf-16" => Some(Encoding::Utf16Le),
            "utf-16be" => Some(Encoding::Utf16Be),
            "gb18030" | "gbk" | "gb2312" => Some(Encoding::Gb18030),
            _ => None,
        }
    }
}

/// 解码 CSV 字节流，返回文本与最终采用的编码。
/// 显式传入的 `wanted` 优先；`None`/`auto` 时按 BOM → UTF-8 → GB18030 探测。
/// 有损解码会返回 `had_errors`，调用方可以据此提示（但不静默丢内容）。
pub fn decode(bytes: &[u8], wanted: Option<Encoding>) -> (String, Encoding, bool) {
    // 指定编码就按指定的解，失败也不回退：由调用方报错并保留旧数据。
    // 只有 `None`（自动）才做 BOM → UTF-8 → GB18030 探测。
    if let Some(w) = wanted {
        return match w {
            Encoding::Utf8 => match std::str::from_utf8(bytes) {
                Ok(t) => (
                    t.trim_start_matches('\u{feff}').to_string(),
                    Encoding::Utf8,
                    false,
                ),
                Err(_) => (String::new(), Encoding::Utf8, true),
            },
            Encoding::Utf16Le => {
                let (cow, _, bad) = encoding_rs::UTF_16LE.decode(bytes);
                (cow.into_owned(), Encoding::Utf16Le, bad)
            }
            Encoding::Utf16Be => {
                let (cow, _, bad) = encoding_rs::UTF_16BE.decode(bytes);
                (cow.into_owned(), Encoding::Utf16Be, bad)
            }
            Encoding::Gb18030 => {
                let (cow, _, bad) = encoding_rs::GB18030.decode(bytes);
                (cow.into_owned(), Encoding::Gb18030, bad)
            }
        };
    }
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return match std::str::from_utf8(bytes) {
            Ok(t) => (
                t.trim_start_matches('\u{feff}').to_string(),
                Encoding::Utf8,
                false,
            ),
            Err(_) => {
                let (cow, _, bad) = encoding_rs::GB18030.decode(bytes);
                (cow.into_owned(), Encoding::Gb18030, bad)
            }
        };
    }
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let (cow, _, bad) = encoding_rs::UTF_16LE.decode(bytes);
        return (cow.into_owned(), Encoding::Utf16Le, bad);
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        let (cow, _, bad) = encoding_rs::UTF_16BE.decode(bytes);
        return (cow.into_owned(), Encoding::Utf16Be, bad);
    }
    match std::str::from_utf8(bytes) {
        Ok(t) => (t.to_string(), Encoding::Utf8, false),
        Err(_) => {
            let (cow, _, bad) = encoding_rs::GB18030.decode(bytes);
            (cow.into_owned(), Encoding::Gb18030, bad)
        }
    }
}

/// 严格解析：引号未闭合、闭引号后还有多余字符都报错。
///
/// 刷新外部数据时用它：解析失败必须整体放弃，旧区域一个格都不能动。
pub fn parse_strict(text: &str, delim: char) -> Result<Vec<Vec<String>>, String> {
    parse_strict_q(text, delim, '"')
}

/// 同 `parse_strict`，但可指定引号字符（部分德语/法语 CSV 用 `'`）。
pub fn parse_strict_q(text: &str, delim: char, quote: char) -> Result<Vec<Vec<String>>, String> {
    parse_strict_while(text, delim, quote, false, || true)
}

/// Strict record parsing with cooperative cancellation, including inside multiline fields.
pub fn records_strict_while(text: &str, delim: char, quote: char, keep_going: impl FnMut() -> bool) -> Result<Vec<Vec<String>>, String> {
    parse_strict_while(text, delim, quote, true, keep_going)
}

fn parse_strict_while(text: &str, delim: char, quote: char, skip_whitespace: bool, mut keep_going: impl FnMut() -> bool) -> Result<Vec<Vec<String>>, String> {
    let skip = |row: &[String]| row.len() == 1 && (row[0].is_empty() || (skip_whitespace && row[0].trim().is_empty()));
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut quoted_field = false;
    let mut closed = false;
    let mut line = 1usize;
    let mut chars = text.chars().peekable();
    let mut ticks = 0usize;
    while let Some(ch) = chars.next() {
        if ticks % 8192 == 0 && !keep_going() { return Err("CSV 读取已取消".into()); }
        ticks += 1;
        if in_quotes {
            if ch == quote {
                if chars.peek() == Some(&quote) {
                    field.push(quote);
                    chars.next();
                } else {
                    in_quotes = false;
                    closed = true;
                }
            } else {
                if ch == '\n' {
                    line += 1;
                }
                field.push(ch);
            }
            continue;
        }
        if closed && ch != delim && ch != '\r' && ch != '\n' {
            return Err(format!("第 {line} 行：引号字段结束后还有多余字符"));
        }
        match ch {
            c if c == quote && field.is_empty() => {
                in_quotes = true;
                quoted_field = true;
            }
            c if c == quote && quoted_field => {
                return Err(format!("第 {line} 行：引号字段结束后还有多余字符"));
            }
            c if c == delim => {
                row.push(std::mem::take(&mut field));
                quoted_field = false;
                closed = false;
            }
            '\r' => {}
            '\n' => {
                row.push(std::mem::take(&mut field));
                quoted_field = false;
                closed = false;
                if !skip(&row) {
                    rows.push(std::mem::take(&mut row));
                } else {
                    row.clear();
                }
                line += 1;
            }
            c => field.push(c),
        }
    }
    if in_quotes {
        return Err(format!("第 {line} 行：引号没有闭合"));
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        if !skip(&row) {
            rows.push(row);
        }
    }
    Ok(rows)
}

/// 严格模式的记录视图（跳过空行）
pub fn records_strict(text: &str, delim: char) -> Result<Vec<Vec<String>>, String> {
    records_strict_q(text, delim, '"')
}

/// 可指定引号字符的严格记录视图
pub fn records_strict_q(text: &str, delim: char, quote: char) -> Result<Vec<Vec<String>>, String> {
    records_strict_while(text, delim, quote, || true)
}

/// Summary of a validated CSV stream keeping only a bounded row preview in memory.
pub struct CsvSummary {
    pub preview_rows: Vec<Vec<String>>,
    pub row_count: usize,
    pub cols: usize,
    pub delim: char,
    pub encoding: Encoding,
}

/// Borrow UTF-8 CSV text without copying when valid; fall back to `decode` for UTF-16 / GB18030.
pub fn decode_cow(bytes: &[u8], wanted: Option<Encoding>) -> (std::borrow::Cow<'_, str>, Encoding, bool) {
    if let Some(w) = wanted {
        return match w {
            Encoding::Utf8 => match std::str::from_utf8(bytes) {
                Ok(t) => (
                    std::borrow::Cow::Borrowed(t.strip_prefix('\u{feff}').unwrap_or(t)),
                    Encoding::Utf8,
                    false,
                ),
                Err(_) => (std::borrow::Cow::Borrowed(""), Encoding::Utf8, true),
            },
            Encoding::Utf16Le => {
                let (cow, _, bad) = encoding_rs::UTF_16LE.decode(bytes);
                (cow, Encoding::Utf16Le, bad)
            }
            Encoding::Utf16Be => {
                let (cow, _, bad) = encoding_rs::UTF_16BE.decode(bytes);
                (cow, Encoding::Utf16Be, bad)
            }
            Encoding::Gb18030 => {
                let (cow, _, bad) = encoding_rs::GB18030.decode(bytes);
                (cow, Encoding::Gb18030, bad)
            }
        };
    }
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        if let Ok(t) = std::str::from_utf8(&bytes[3..]) {
            return (std::borrow::Cow::Borrowed(t), Encoding::Utf8, false);
        }
        let (cow, _, bad) = encoding_rs::GB18030.decode(bytes);
        return (cow, Encoding::Gb18030, bad);
    }
    if bytes.starts_with(&[0xFF, 0xFE]) {
        let (cow, _, bad) = encoding_rs::UTF_16LE.decode(bytes);
        return (cow, Encoding::Utf16Le, bad);
    }
    if bytes.starts_with(&[0xFE, 0xFF]) {
        let (cow, _, bad) = encoding_rs::UTF_16BE.decode(bytes);
        return (cow, Encoding::Utf16Be, bad);
    }
    match std::str::from_utf8(bytes) {
        Ok(t) => (std::borrow::Cow::Borrowed(t), Encoding::Utf8, false),
        Err(_) => {
            let (cow, _, bad) = encoding_rs::GB18030.decode(bytes);
            (cow, Encoding::Gb18030, bad)
        }
    }
}

/// Fast-path preview from the first 64 KiB of a CSV file so the import dialog can render
/// preview rows immediately before background full-tail validation completes.
pub fn read_preview_head(
    path: &std::path::Path,
    wanted: Option<Encoding>,
    quote: char,
    max_rows: usize,
) -> Option<(Vec<Vec<String>>, usize, char, Encoding)> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).ok()?;
    let mut buf = vec![0u8; 64 * 1024];
    let mut total = 0usize;
    while total < buf.len() {
        match file.read(&mut buf[total..]) {
            Ok(0) => break,
            Ok(n) => total += n,
            Err(_) => return None,
        }
    }
    if total == 0 {
        return None;
    }
    let eof = total < buf.len();
    buf.truncate(total);
    // Trim incomplete trailing multi-byte sequence when we stopped mid-file.
    if !eof {
        if matches!(wanted, Some(Encoding::Utf16Le | Encoding::Utf16Be))
            || buf.starts_with(&[0xFF, 0xFE])
            || buf.starts_with(&[0xFE, 0xFF])
        {
            if buf.len() % 2 == 1 {
                buf.pop();
            }
        } else if let Err(err) = std::str::from_utf8(&buf) {
            if err.error_len().is_none() {
                buf.truncate(err.valid_up_to());
            }
        }
    }
    let (cow, enc, bad) = decode_cow(&buf, wanted);
    if bad {
        return None;
    }
    let sample = if eof {
        cow.as_ref()
    } else if let Some(nl) = cow.rfind('\n') {
        &cow[..=nl]
    } else {
        return None;
    };
    let delim = sniff(sample);
    let mut rows = parse(sample, delim);
    rows.truncate(max_rows);
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    Some((rows, cols, delim, enc))
}

/// Strictly validate `text` and collect `(preview_rows, row_count, max_cols)` without
/// allocating `String` / `Vec<String>` for rows beyond `preview_limit`.
pub fn scan_summary_while(
    text: &str,
    delim: char,
    quote: char,
    encoding: Encoding,
    preview_limit: usize,
    mut keep_going: impl FnMut() -> bool,
) -> Result<CsvSummary, String> {
    let mut preview_rows: Vec<Vec<String>> = Vec::with_capacity(preview_limit.min(16));
    let mut preview_row: Vec<String> = Vec::new();
    let mut preview_field = String::new();

    let mut row_count = 0usize;
    let mut max_cols = 0usize;
    let mut cur_cols = 0usize;
    let mut field_non_empty = false;
    let mut field_non_ws = false;
    let mut in_quotes = false;
    let mut quoted_field = false;
    let mut closed = false;
    let mut line = 1usize;

    // Fast byte-level state machine when delimiter and quote are ASCII (standard CSV/TSV).
    if delim.is_ascii() && quote.is_ascii() && delim != '\r' && delim != '\n' && quote != '\r' && quote != '\n' {
        let bytes = text.as_bytes();
        let db = delim as u8;
        let qb = quote as u8;
        let mut i = 0usize;
        let mut seg_start = 0usize;
        while i < bytes.len() {
            if (i & 8191) == 0 && !keep_going() {
                return Err("CSV 读取已取消".into());
            }
            let b = bytes[i];
            let collecting = row_count < preview_limit;
            if in_quotes {
                if b == qb {
                    if bytes.get(i + 1) == Some(&qb) {
                        if collecting {
                            preview_field.push_str(&text[seg_start..i]);
                            preview_field.push(quote);
                        }
                        field_non_empty = true;
                        field_non_ws = true;
                        i += 2;
                        seg_start = i;
                        continue;
                    }
                    if collecting {
                        preview_field.push_str(&text[seg_start..i]);
                    }
                    in_quotes = false;
                    closed = true;
                    i += 1;
                    seg_start = i;
                    continue;
                }
                if b == b'\n' {
                    line += 1;
                }
                field_non_empty = true;
                if !b.is_ascii_whitespace() {
                    field_non_ws = true;
                }
                i += 1;
                continue;
            }
            if closed && b != db && b != b'\r' && b != b'\n' {
                return Err(format!("第 {line} 行：引号字段结束后还有多余字符"));
            }
            if b == qb && !field_non_empty && !quoted_field {
                in_quotes = true;
                quoted_field = true;
                i += 1;
                seg_start = i;
                continue;
            }
            if b == qb && quoted_field {
                return Err(format!("第 {line} 行：引号字段结束后还有多余字符"));
            }
            if b == db {
                if collecting {
                    if !quoted_field {
                        preview_field.push_str(&text[seg_start..i]);
                    }
                    preview_row.push(std::mem::take(&mut preview_field));
                }
                cur_cols += 1;
                field_non_empty = false;
                field_non_ws = false;
                quoted_field = false;
                closed = false;
                i += 1;
                seg_start = i;
                continue;
            }
            if b == b'\r' {
                if collecting && !quoted_field {
                    preview_field.push_str(&text[seg_start..i]);
                }
                i += 1;
                seg_start = i;
                continue;
            }
            if b == b'\n' {
                if collecting {
                    if !quoted_field {
                        preview_field.push_str(&text[seg_start..i]);
                    }
                    preview_row.push(std::mem::take(&mut preview_field));
                }
                cur_cols += 1;
                let skip_row = cur_cols == 1 && !quoted_field && !field_non_ws;
                if !skip_row {
                    max_cols = max_cols.max(cur_cols);
                    if collecting {
                        preview_rows.push(std::mem::take(&mut preview_row));
                    }
                    row_count += 1;
                } else if collecting {
                    preview_row.clear();
                }
                cur_cols = 0;
                field_non_empty = false;
                field_non_ws = false;
                quoted_field = false;
                closed = false;
                line += 1;
                i += 1;
                seg_start = i;
                continue;
            }
            field_non_empty = true;
            if !b.is_ascii_whitespace() {
                // Non-ASCII UTF-8 bytes (`>= 0x80`): check Unicode whitespace only if single-field candidate.
                if b < 0x80 {
                    field_non_ws = true;
                } else if let Some(ch) = text[i..].chars().next() {
                    if !ch.is_whitespace() {
                        field_non_ws = true;
                    }
                    i += ch.len_utf8();
                    continue;
                }
            }
            i += 1;
        }
        if in_quotes {
            return Err(format!("第 {line} 行：引号没有闭合"));
        }
        if field_non_empty || quoted_field || cur_cols > 0 {
            let collecting = row_count < preview_limit;
            if collecting {
                if !quoted_field && seg_start < bytes.len() {
                    preview_field.push_str(&text[seg_start..bytes.len()]);
                }
                preview_row.push(preview_field);
            }
            cur_cols += 1;
            let skip_row = cur_cols == 1 && !quoted_field && !field_non_ws;
            if !skip_row {
                max_cols = max_cols.max(cur_cols);
                if collecting {
                    preview_rows.push(preview_row);
                }
                row_count += 1;
            }
        }
        return Ok(CsvSummary {
            preview_rows,
            row_count,
            cols: max_cols,
            delim,
            encoding,
        });
    }

    // Fallback for non-ASCII delimiter/quote characters.
    let rows = records_strict_while(text, delim, quote, keep_going)?;
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    let row_count = rows.len();
    let preview_rows = rows.into_iter().take(preview_limit).collect();
    Ok(CsvSummary {
        preview_rows,
        row_count,
        cols,
        delim,
        encoding,
    })
}

/// Partition validated ASCII-delimited/quoted CSV `text` into up to `target_chunks` slices
/// `(byte_start, byte_end, starting_logical_row_index)` split strictly at unquoted `\n`.
pub fn partition_csv_chunks(
    text: &str,
    delim: char,
    quote: char,
    target_chunks: usize,
) -> Option<Vec<(usize, usize, usize)>> {
    if !delim.is_ascii()
        || !quote.is_ascii()
        || delim == '\r'
        || delim == '\n'
        || quote == '\r'
        || quote == '\n'
        || text.is_empty()
    {
        return None;
    }
    let chunks = target_chunks.max(1);
    let bytes = text.as_bytes();
    let target_bytes = (bytes.len() / chunks).max(256 * 1024);
    let db = delim as u8;
    let qb = quote as u8;

    let mut out = Vec::with_capacity(chunks);
    let mut chunk_start = 0usize;
    let mut chunk_row_start = 0usize;
    let mut next_split_at = target_bytes;

    let mut row_count = 0usize;
    let mut cur_cols = 0usize;
    let mut field_non_empty = false;
    let mut field_non_ws = false;
    let mut in_quotes = false;
    let mut quoted_field = false;
    let mut i = 0usize;

    while i < bytes.len() {
        let b = bytes[i];
        if in_quotes {
            if b == qb {
                if bytes.get(i + 1) == Some(&qb) {
                    i += 2;
                    continue;
                }
                in_quotes = false;
            }
            i += 1;
            continue;
        }
        if b == qb && !field_non_empty && !quoted_field {
            in_quotes = true;
            quoted_field = true;
            i += 1;
            continue;
        }
        if b == db {
            cur_cols += 1;
            field_non_empty = false;
            field_non_ws = false;
            quoted_field = false;
            i += 1;
            continue;
        }
        if b == b'\r' {
            i += 1;
            continue;
        }
        if b == b'\n' {
            cur_cols += 1;
            let skip_row = cur_cols == 1 && !quoted_field && !field_non_ws;
            if !skip_row {
                row_count += 1;
            }
            cur_cols = 0;
            field_non_empty = false;
            field_non_ws = false;
            quoted_field = false;
            let line_end = i + 1;
            if line_end >= next_split_at && out.len() + 1 < chunks && line_end < bytes.len() {
                out.push((chunk_start, line_end, chunk_row_start));
                chunk_start = line_end;
                chunk_row_start = row_count;
                next_split_at = line_end + target_bytes;
            }
            i = line_end;
            continue;
        }
        field_non_empty = true;
        if b < 0x80 {
            if !b.is_ascii_whitespace() {
                field_non_ws = true;
            }
            i += 1;
        } else if let Some(ch) = text[i..].chars().next() {
            if !ch.is_whitespace() {
                field_non_ws = true;
            }
            i += ch.len_utf8();
        } else {
            i += 1;
        }
    }
    if chunk_start < bytes.len() {
        out.push((chunk_start, bytes.len(), chunk_row_start));
    }
    Some(out)
}

/// 记录模式：跳过完全空行，但**保留**字段内的换行与引号。
pub fn records(text: &str, delim: char) -> Vec<Vec<String>> {
    parse(text, delim)
        .into_iter()
        .filter(|r| !(r.len() == 1 && r[0].trim().is_empty()))
        .collect()
}
