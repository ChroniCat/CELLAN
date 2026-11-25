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

    #[test]
    fn simd_scan_partition_and_streaming_csv_export_roundtrip() {
        let mut csv_text = String::with_capacity(600_000);
        csv_text.push_str("id,label,amount\n");
        for i in 0..20_000usize {
            if i % 500 == 0 {
                csv_text.push_str(&format!("{i},\"含,逗号与\"\"引号\"\"\n第二行\",{}.25\n", i * 10));
            } else {
                csv_text.push_str(&format!("{i},item_{i},{}.5\n", i));
            }
        }
        let summary = scan_summary_while(&csv_text, ',', '"', Encoding::Utf8, 8, || true).unwrap();
        assert_eq!(summary.row_count, 20_001);
        assert_eq!(summary.cols, 3);
        assert_eq!(summary.preview_rows.len(), 8);

        let chunks = partition_csv_chunks(&csv_text, ',', '"', 4).unwrap();
        assert!(chunks.len() >= 2);
        let mut total_rows = 0usize;
        for &(s, e, start_r) in &chunks {
            assert_eq!(start_r, total_rows);
            let parsed = records_strict(&csv_text[s..e], ',').unwrap();
            total_rows += parsed.len();
        }
        assert_eq!(total_rows, 20_001);

        let mut wb = crate::model::Workbook::new_blank();
        let sh = &mut wb.sheets[0];
        sh.set(0, 0, crate::model::Cell { value: crate::model::Value::Text("姓名,部门".into()), formula: None });
        sh.set(0, 2, crate::model::Cell { value: crate::model::Value::Number(123.45), formula: None });
        sh.set(1, 1, crate::model::Cell { value: crate::model::Value::Bool(true), formula: None });
        let mut exported = Vec::new();
        export_sheet_csv_writer(sh, &wb.styles, wb.date_system, &mut exported, ',').unwrap();
        let out_str = String::from_utf8(exported).unwrap();
        assert_eq!(out_str, "\"姓名,部门\",,123.45\n,TRUE,\n");
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
    _quote: char,
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
        let mut last_cancel_check = 0usize;
        while i < bytes.len() {
            if i.saturating_sub(last_cancel_check) >= 8192 {
                last_cancel_check = i;
                if !keep_going() {
                    return Err("CSV 读取已取消".into());
                }
            } else if (i & 8191) == 0 && !keep_going() {
                return Err("CSV 读取已取消".into());
            }
            let collecting = row_count < preview_limit;
            if in_quotes && !collecting {
                let rem = &bytes[i..];
                let window = &rem[..rem.len().min(8192)];
                match crate::xlsx::sheets::stream::simd_memchr2(qb, b'\n', window) {
                    Some(rel) => {
                        if rel > 0 {
                            field_non_empty = true;
                            if !field_non_ws
                                && window[..rel].iter().any(|b| !b.is_ascii_whitespace())
                            {
                                field_non_ws = true;
                            }
                            i += rel;
                        }
                    }
                    None => {
                        field_non_empty = true;
                        if !field_non_ws && window.iter().any(|b| !b.is_ascii_whitespace()) {
                            field_non_ws = true;
                        }
                        i += window.len();
                        continue;
                    }
                }
            } else if !in_quotes
                && !closed
                && !collecting
                && field_non_empty
                && (cur_cols > 0 || field_non_ws)
            {
                let rem = &bytes[i..];
                let window = &rem[..rem.len().min(8192)];
                match crate::xlsx::sheets::stream::simd_memchr2(db, b'\n', window) {
                    Some(rel) => {
                        i += rel;
                    }
                    None => {
                        i += window.len();
                        continue;
                    }
                }
            }
            let b = bytes[i];
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
        if in_quotes {
            if let Some(rel) = crate::xlsx::sheets::stream::simd_memchr(qb, &bytes[i..]) {
                i += rel;
                if bytes.get(i + 1) == Some(&qb) {
                    i += 2;
                    continue;
                }
                in_quotes = false;
                i += 1;
                continue;
            } else {
                break;
            }
        } else if field_non_empty && (cur_cols > 0 || field_non_ws) {
            match crate::xlsx::sheets::stream::simd_memchr2(db, b'\n', &bytes[i..]) {
                Some(rel) => {
                    i += rel;
                }
                None => {
                    break;
                }
            }
        }
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

fn write_csv_escaped_bytes(out: &mut Vec<u8>, s: &str, delim: char) {
    let bytes = s.as_bytes();
    let need_quote = if delim.is_ascii() {
        let db = delim as u8;
        crate::xlsx::sheets::stream::simd_memchr3(db, b'"', b'\n', bytes).is_some()
            || crate::xlsx::sheets::stream::simd_memchr(b'\r', bytes).is_some()
    } else {
        s.chars().any(|c| c == delim || c == '"' || c == '\n' || c == '\r')
    };
    if !need_quote {
        out.extend_from_slice(bytes);
        return;
    }
    out.push(b'"');
    let mut pos = 0usize;
    while pos < bytes.len() {
        match crate::xlsx::sheets::stream::simd_memchr(b'"', &bytes[pos..]) {
            Some(rel) => {
                out.extend_from_slice(&bytes[pos..pos + rel + 1]);
                out.push(b'"');
                pos += rel + 1;
            }
            None => {
                out.extend_from_slice(&bytes[pos..]);
                break;
            }
        }
    }
    out.push(b'"');
}

/// Stream `sheet` to `writer` as RFC-4180 CSV/TSV without materializing an intermediate
/// `Vec<Vec<String>>`, formatting numbers via `zmij` when `num_format` is General/empty.
pub fn export_sheet_csv_writer(
    sheet: &crate::model::Sheet,
    styles: &crate::style::StyleTable,
    date_system: crate::model::DateSystem,
    mut writer: impl std::io::Write,
    delim: char,
) -> std::io::Result<()> {
    let Some((min_r, min_c, max_r, max_c)) = sheet.used_bounds() else {
        return writer.flush();
    };
    let _ = min_r;
    let _ = min_c;
    let mut delim_utf8 = [0u8; 4];
    let delim_bytes = delim.encode_utf8(&mut delim_utf8).as_bytes();
    let mut buf: Vec<u8> = Vec::with_capacity(256 * 1024);
    let mut it = sheet.cells().iter_rect_views((0, 0, max_r, max_c)).peekable();

    for r in 0..=max_r {
        let mut next_col = 0u32;
        while let Some(&(pos, view)) = it.peek() {
            if pos.row != r {
                break;
            }
            let _ = it.next();
            while next_col < pos.col {
                buf.extend_from_slice(delim_bytes);
                next_col += 1;
            }
            let sid = sheet.style_of(pos.row, pos.col);
            let fmt = if sid == 0 {
                ""
            } else {
                styles.get(sid).format.as_str()
            };
            let plain_num = fmt.is_empty() || fmt.eq_ignore_ascii_case("General");
            let val_cow = view.value();
            match val_cow.as_ref() {
                crate::model::Value::Empty => {}
                crate::model::Value::Number(n) if plain_num && n.is_finite() => {
                    let n = if *n == 0.0 { 0.0 } else { *n };
                    let iv = n as i64;
                    if (iv as f64) == n && iv.unsigned_abs() < 10_000_000_000_000_000 {
                        let _ = std::io::Write::write_fmt(&mut buf, format_args!("{iv}"));
                    } else {
                        let _ = serde_json::to_writer(&mut buf, &n);
                    }
                }
                crate::model::Value::Bool(b) if plain_num => {
                    buf.extend_from_slice(if *b { b"TRUE" } else { b"FALSE" });
                }
                crate::model::Value::Error(e) => {
                    buf.extend_from_slice(e.text().as_bytes());
                }
                crate::model::Value::Text(s) if plain_num => {
                    write_csv_escaped_bytes(&mut buf, s.as_ref(), delim);
                }
                owned => {
                    let rendered = crate::style::format_value_in(date_system, owned, fmt);
                    write_csv_escaped_bytes(&mut buf, &rendered, delim);
                }
            }
            if pos.col < max_c {
                buf.extend_from_slice(delim_bytes);
            }
            next_col = pos.col + 1;
        }
        while next_col < max_c {
            buf.extend_from_slice(delim_bytes);
            next_col += 1;
        }
        buf.push(b'\n');
        if buf.len() >= 192 * 1024 {
            writer.write_all(&buf)?;
            buf.clear();
        }
    }
    if !buf.is_empty() {
        writer.write_all(&buf)?;
    }
    writer.flush()
}

/// Stream `sheet` to `path` as UTF-8 CSV/TSV with a 256 KiB buffered writer.
pub fn export_sheet_csv(
    sheet: &crate::model::Sheet,
    styles: &crate::style::StyleTable,
    date_system: crate::model::DateSystem,
    path: &std::path::Path,
    delim: char,
) -> std::io::Result<()> {
    let file = std::fs::File::create(path)?;
    let writer = std::io::BufWriter::with_capacity(256 * 1024, file);
    export_sheet_csv_writer(sheet, styles, date_system, writer, delim)
}

/// 将工作表导出为指定编码（UTF-8 / UTF-8 BOM / GB18030-GBK / UTF-16LE）与自定义分隔符的 CSV 字节流。
pub fn export_sheet_csv_bytes(
    sheet: &crate::model::Sheet,
    styles: &crate::style::StyleTable,
    date_system: crate::model::DateSystem,
    delim: char,
    encoding: Encoding,
    utf8_bom: bool,
) -> std::io::Result<Vec<u8>> {
    let mut utf8_buf = Vec::new();
    export_sheet_csv_writer(sheet, styles, date_system, &mut utf8_buf, delim)?;
    let text = String::from_utf8(utf8_buf)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    match encoding {
        Encoding::Utf8 => {
            if utf8_bom {
                let mut out = Vec::with_capacity(3 + text.len());
                out.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
                out.extend_from_slice(text.as_bytes());
                Ok(out)
            } else {
                Ok(text.into_bytes())
            }
        }
        Encoding::Gb18030 => {
            let (cow, _, _) = encoding_rs::GB18030.encode(&text);
            Ok(cow.into_owned())
        }
        Encoding::Utf16Le => {
            let mut out = Vec::with_capacity(2 + text.len() * 2);
            out.extend_from_slice(&[0xFF, 0xFE]);
            for u in text.encode_utf16() {
                out.extend_from_slice(&u.to_le_bytes());
            }
            Ok(out)
        }
        Encoding::Utf16Be => {
            let mut out = Vec::with_capacity(2 + text.len() * 2);
            out.extend_from_slice(&[0xFE, 0xFF]);
            for u in text.encode_utf16() {
                out.extend_from_slice(&u.to_be_bytes());
            }
            Ok(out)
        }
    }
}

/// 按指定编码与分隔符导出工作表到 CSV/TSV 文件。
pub fn export_sheet_csv_encoded(
    sheet: &crate::model::Sheet,
    styles: &crate::style::StyleTable,
    date_system: crate::model::DateSystem,
    path: &std::path::Path,
    delim: char,
    encoding: Encoding,
    utf8_bom: bool,
) -> std::io::Result<()> {
    let bytes = export_sheet_csv_bytes(sheet, styles, date_system, delim, encoding, utf8_bom)?;
    std::fs::write(path, bytes)
}
