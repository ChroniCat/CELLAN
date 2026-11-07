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
