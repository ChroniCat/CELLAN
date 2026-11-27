use anyhow::{Context, Result, bail, ensure};

/// Split only outside quoted sheet names (e.g. ['Sales.Q1'.A1:.C4]).
fn separator(s: &str, wanted: char) -> Option<usize> {
    let mut quoted = false;
    s.char_indices().find_map(|(i, c)| {
        if c == '\'' {
            quoted = !quoted;
        }
        (!quoted && c == wanted).then_some(i)
    })
}

fn operand(s: &str) -> Result<(&str, &str)> {
    let dot = separator(s, '.').context("引用中缺少工作表与坐标分隔符")?;
    let sheet = s[..dot].trim_start_matches('$');
    let cell = &s[dot + 1..];
    ensure!(
        crate::sparse::parse_ref(&cell.replace('$', "")).is_some(),
        "不支持的单元格引用 {s}"
    );
    // External documents use a quoted URI followed by '#'.
    ensure!(separator(sheet, '#').is_none(), "不支持外部文档引用 {s}");
    Ok((sheet, cell))
}

fn reference(raw: &str, out: &mut String) -> Result<()> {
    let (first, last) = separator(raw, ':')
        .map(|i| (&raw[..i], Some(&raw[i + 1..])))
        .unwrap_or((raw, None));
    let (sheet, start) = operand(first)?;
    if !sheet.is_empty() {
        out.push_str(sheet);
        out.push('!');
    }
    out.push_str(start);
    if let Some(last) = last {
        let (end_sheet, end) = operand(last)?;
        ensure!(
            end_sheet.is_empty() || end_sheet == sheet,
            "不支持跨表区域 {raw}"
        );
        out.push(':');
        out.push_str(end);
    }
    Ok(())
}

/// Translate OpenFormula tokens without changing separators or brackets inside string literals.
pub(super) fn to_excel(formula: &str) -> Result<String> {
    let mut rest = ["of:=", "oooc:=", "="]
        .iter()
        .find_map(|p| formula.strip_prefix(p))
        .context("不支持的 OpenDocument 公式命名空间")?;
    let mut out = String::from("=");
    let mut in_string = false;
    let mut in_array = false;
    while let Some(c) = rest.chars().next() {
        rest = &rest[c.len_utf8()..];
        if c == '"' {
            in_string = !in_string;
            out.push(c);
        } else if in_string {
            out.push(c);
        } else {
            match c {
                '[' => {
                    let end = separator(rest, ']').context("引用缺少右方括号")?;
                    reference(&rest[..end], &mut out)?;
                    rest = &rest[end + 1..];
                }
                '{' => {
                    in_array = true;
                    out.push(c);
                }
                '}' => {
                    in_array = false;
                    out.push(c);
                }
                ';' => out.push(','),
                '|' if in_array => out.push(';'),
                '#' => {
                    let error = [
                        "REF!", "VALUE!", "DIV/0!", "NAME?", "NULL!", "NUM!", "N/A", "SPILL!",
                    ]
                    .into_iter()
                    .find(|e| rest.starts_with(e))
                    .context("不支持的错误常量")?;
                    out.push('#');
                    out.push_str(error);
                    rest = &rest[error.len()..];
                }
                '~' | '!' => bail!("不支持的 OpenDocument 引用运算符 {c}"),
                _ => out.push(c),
            }
        }
    }
    ensure!(!in_string && !in_array, "公式字符串或数组未闭合");
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::to_excel;

    #[test]
    fn quoted_literals_external_references_and_array_separators() {
        assert_eq!(
            to_excel(r#"of:=IFERROR("x[.A1];"";""";#REF!)"#).unwrap(),
            r#"=IFERROR("x[.A1];"";""",#REF!)"#
        );
        assert_eq!(
            to_excel("of:=SUM([$'Q.1''s'.$A$2:.$B$3])").unwrap(),
            "=SUM('Q.1''s'!$A$2:$B$3)"
        );
        assert_eq!(to_excel("of:={1;2|3;4}").unwrap(), "={1,2;3,4}");
        assert!(to_excel("of:=['file:///other.ods'#$S.A1]").is_err());
    }
}
