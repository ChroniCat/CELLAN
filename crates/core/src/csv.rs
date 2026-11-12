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
    let bom_enc = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        Some(Encoding::Utf8)
    } else if bytes.starts_with(&[0xFF, 0xFE]) {
        Some(Encoding::Utf16Le)
    } else if bytes.starts_with(&[0xFE, 0xFF]) {
        Some(Encoding::Utf16Be)
    } else {
        None
    };
    let enc = wanted.filter(|e| *e != Encoding::Utf8).or(bom_enc).unwrap_or(Encoding::Utf8);
    match enc {
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
        Encoding::Utf8 => match std::str::from_utf8(bytes) {
            Ok(t) => (t.trim_start_matches('\u{feff}').to_string(), Encoding::Utf8, false),
            Err(_) => {
                // 不是合法 UTF-8 又没有 BOM：按中文环境最常见的 GB18030 解
                let (cow, _, bad) = encoding_rs::GB18030.decode(bytes);
                (cow.into_owned(), Encoding::Gb18030, bad)
            }
        },
    }
}

/// 记录模式：跳过完全空行，但**保留**字段内的换行与引号。
pub fn records(text: &str, delim: char) -> Vec<Vec<String>> {
    parse(text, delim)
        .into_iter()
        .filter(|r| !(r.len() == 1 && r[0].trim().is_empty()))
        .collect()
}
