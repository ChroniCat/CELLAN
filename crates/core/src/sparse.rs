//! 稀疏工作表的旁路读取。
//!
//! calamine 的 `worksheet_range` 会按声明区域分配稠密网格：一旦有人在
//! A1048576 / XFD1048576 这种角落留了格子（很常见：整列刷格式），
//! 声明区域就是 1048576×16384，直接要 512 GiB。
//!
//! 这条通道直接流式扫 sheet XML，只处理真实存在的 `<c>`，内存与格子数成正比。

use anyhow::Result;
use quick_xml::events::Event;
use quick_xml::Reader;
use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

use crate::model::{Cell, Value};

/// 声明区域超过这个格数就走稀疏通道
pub const DENSE_LIMIT: u64 = 2_000_000;

fn col_of(letters: &str) -> Option<u32> {
    let mut n: u32 = 0;
    let mut any = false;
    for ch in letters.chars() {
        let d = ch.to_ascii_uppercase();
        if !d.is_ascii_uppercase() {
            return None;
        }
        n = n * 26 + (d as u32 - 'A' as u32 + 1);
        any = true;
    }
    if any {
        Some(n - 1)
    } else {
        None
    }
}

/// "B12" → (11, 1)
pub fn parse_ref(s: &str) -> Option<(u32, u32)> {
    let letters: String = s.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
    let digits: String = s.chars().skip(letters.len()).collect();
    if letters.is_empty() || digits.is_empty() {
        return None;
    }
    let col = col_of(&letters)?;
    let row: u32 = digits.parse().ok()?;
    Some((row.saturating_sub(1), col))
}


fn tag_start(e: &quick_xml::events::BytesStart<'_>) -> String {
    e.name().as_ref().to_string()
}

fn tag_end(e: &quick_xml::events::BytesEnd<'_>) -> String {
    e.name().as_ref().to_string()
}

/// 直接从 zip 里读某个部件的声明区域（只读开头一小段）
pub fn declared_area_of(path: &Path, part: &str) -> u64 {
    let Ok(file) = std::fs::File::open(path) else {
        return 0;
    };
    let Ok(mut zip) = zip::ZipArchive::new(file) else {
        return 0;
    };
    let Ok(mut f) = zip.by_name(part) else {
        return 0;
    };
    let mut buf = vec![0u8; 4096];
    use std::io::Read;
    let n = f.read(&mut buf).unwrap_or(0);
    declared_area(&String::from_utf8_lossy(&buf[..n]))
}

/// 声明区域格数（拿不到维度就返回 0）
pub fn declared_area(xml_head: &str) -> u64 {
    let Some(pos) = xml_head.find("<dimension ref=\"") else {
        return 0;
    };
    let rest = &xml_head[pos + 16..];
    let Some(q) = rest.find(char::from(34)) else { return 0 };
    let r = &rest[..q];
    let (a, b) = match r.split_once(':') {
        Some((a, b)) => (a, b),
        None => (r, r),
    };
    let (Some((r1, c1)), Some((r2, c2))) = (parse_ref(a), parse_ref(b)) else {
        return 0;
    };
    (r2 as u64 - r1 as u64 + 1) * (c2 as u64 - c1 as u64 + 1)
}

/// 读共享字符串表
pub fn shared_strings(path: &Path) -> Vec<String> {
    shared_strings_checked(path).unwrap_or_default()
}

/// 读共享字符串表；部件缺失/读失败要能区分出来（上层据此判定打开不完整），
/// 不能像 `shared_strings` 那样一律返回空表。
pub fn shared_strings_checked(path: &Path) -> Result<Vec<String>> {
    let mut out = Vec::new();
    let file = std::fs::File::open(path)?;
    let mut zip = zip::ZipArchive::new(file)?;
    let mut f = zip.by_name("xl/sharedStrings.xml")?;
    let mut xml = String::new();
    f.read_to_string(&mut xml)?;
    let mut reader = Reader::from_str(&xml);
    reader.config_mut().trim_text(false);
    let mut in_si = false;
    let mut cur = String::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                if tag_start(&e) == "si" {
                    in_si = true;
                    cur.clear();
                }
            }
            Ok(Event::End(e)) => {
                if tag_end(&e) == "si" {
                    out.push(std::mem::take(&mut cur));
                    in_si = false;
                }
            }
            Ok(Event::Text(t)) => {
                if in_si {
                    cur.push_str(t.as_ref());
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => anyhow::bail!("sharedStrings.xml 解析失败：{e}"),
            _ => {}
        }
    }
    Ok(out)
}

/// 流式读一张表：返回 (行, 列, 单元格)
pub fn read_sheet(path: &Path, part: &str, ss: &[String]) -> Result<HashMap<(u32, u32), Cell>> {
    let file = std::fs::File::open(path)?;
    let mut zip = zip::ZipArchive::new(file)?;
    let mut f = zip.by_name(part)?;
    let mut xml = String::new();
    f.read_to_string(&mut xml)?;
    let cells = parse_sheet_xml(&xml, ss)?;
    Ok(cells)
}


/// 把公式里的 A1 引用按 (dr, dc) 平移；$ 锁定的部分不动。
/// 共享公式的子格没有自己的文本，靠这个从主格文本推出来。
pub fn shift_refs(formula: &str, dr: i64, dc: i64) -> String {
    if dr == 0 && dc == 0 {
        return formula.to_string();
    }
    let chars: Vec<char> = formula.chars().collect();
    let mut out = String::with_capacity(formula.len() + 8);
    let mut i = 0usize;
    let mut in_str = false;
    while i < chars.len() {
        let c = chars[i];
        if c == '"' {
            in_str = !in_str;
            out.push(c);
            i += 1;
            continue;
        }
        if in_str {
            out.push(c);
            i += 1;
            continue;
        }
        // 解析一个可能的引用：[$]字母{1,3}[$]数字{1,7}
        let start = i;
        let mut j = i;
        let col_abs = chars[j] == '$';
        if col_abs {
            j += 1;
        }
        let ls = j;
        while j < chars.len() && chars[j].is_ascii_alphabetic() && j - ls < 3 {
            j += 1;
        }
        if j == ls {
            out.push(c);
            i += 1;
            continue;
        }
        let letters: String = chars[ls..j].iter().collect();
        let row_abs = j < chars.len() && chars[j] == '$';
        if row_abs {
            j += 1;
        }
        let ds = j;
        while j < chars.len() && chars[j].is_ascii_digit() {
            j += 1;
        }
        let too_long = j - ds > 7;
        let is_ref = j > ds && !too_long
            && !(j < chars.len() && (chars[j].is_ascii_alphanumeric() || chars[j] == '(' || chars[j] == '_'))
            && !(start > 0 && (chars[start - 1].is_ascii_alphanumeric() || chars[start - 1] == '_' || chars[start - 1] == '$' || chars[start - 1] == '.'));
        if !is_ref {
            out.push(c);
            i += 1;
            continue;
        }
        let mut col = 0i64;
        for ch in letters.chars() {
            col = col * 26 + (ch.to_ascii_uppercase() as i64 - 'A' as i64 + 1);
        }
        col -= 1;
        let row: i64 = chars[ds..j].iter().collect::<String>().parse().unwrap_or(1) - 1;
        let nc = if col_abs { col } else { col + dc };
        let nr = if row_abs { row } else { row + dr };
        if nc < 0 || nr < 0 {
            out.extend(&chars[start..j]);
        } else {
            if col_abs {
                out.push('$');
            }
            out.push_str(&crate::col_label(nc as u32));
            if row_abs {
                out.push('$');
            }
            out.push_str(&(nr + 1).to_string());
        }
        i = j;
    }
    out
}


/// XML 实体还原：Excel 把公式里的 > < 写成 &gt; &lt;
fn unescape(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}


/// 共享公式：calamine 交给子格的是主格原文（没按位移改），
/// 这里按 XML 里的 si 组重新推导，返回需要覆盖的 (行, 列, 公式)。
pub fn fix_shared_children(xml: &str) -> Vec<(u32, u32, String)> {
    use quick_xml::events::Event;
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut out: Vec<(u32, u32, String)> = Vec::new();
    let mut masters: HashMap<String, (String, (u32, u32))> = HashMap::new();
    let mut cell_ref: Option<(u32, u32)> = None;
    let mut si: Option<String> = None;
    let mut body = String::new();
    let mut in_f = false;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => match tag_start(&e).as_str() {
                "c" => {
                    cell_ref = None;
                    for a in e.attributes().flatten() {
                        if a.key.as_ref() == "r" {
                            cell_ref = parse_ref(a.value.as_ref());
                        }
                    }
                }
                "f" => {
                    in_f = true;
                    body.clear();
                    si = None;
                    for a in e.attributes().flatten() {
                        if a.key.as_ref() == "si" {
                            si = Some(a.value.as_ref().to_string());
                        }
                    }
                }
                _ => {}
            },
            Ok(Event::End(e)) => {
                if tag_end(&e).as_str() == "f" {
                    in_f = false;
                    if let (Some(si), Some(pos)) = (si.clone(), cell_ref) {
                        let text = body.trim().to_string();
                        if !text.is_empty() {
                            masters.insert(si, (text, pos));
                        } else if let Some((mt, anchor)) = masters.get(&si) {
                            let dr = pos.0 as i64 - anchor.0 as i64;
                            let dc = pos.1 as i64 - anchor.1 as i64;
                            out.push((pos.0, pos.1, format!("={}", shift_refs(mt, dr, dc))));
                        }
                    }
                }
            }
            Ok(Event::Empty(e)) => {
                if tag_start(&e) == "f" {
                    let mut si2: Option<String> = None;
                    for a in e.attributes().flatten() {
                        if a.key.as_ref() == "si" {
                            si2 = Some(a.value.as_ref().to_string());
                        }
                    }
                    if let (Some(si), Some(pos)) = (si2, cell_ref) {
                        if let Some((mt, anchor)) = masters.get(&si) {
                            let dr = pos.0 as i64 - anchor.0 as i64;
                            let dc = pos.1 as i64 - anchor.1 as i64;
                            out.push((pos.0, pos.1, format!("={}", shift_refs(mt, dr, dc))));
                        }
                    }
                }
            }
            Ok(Event::Text(t)) => {
                if in_f {
                    body.push_str(t.as_ref());
                }
            }
            Ok(Event::GeneralRef(r)) => {
                if in_f {
                    let name = String::from_utf8_lossy(r.as_ref().as_ref()).to_string();
                    let ch = match name.as_str() {
                        "gt" => Some('>'),
                        "lt" => Some('<'),
                        "amp" => Some('&'),
                        "quot" => Some('"'),
                        "apos" => Some('\''),
                        _ => None,
                    };
                    if let Some(c) = ch {
                        body.push(c);
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => break,
            _ => {}
        }
    }
    out
}

/// 工作表 XML → 单元格表。单拎出来是为了能直接用字符串写测试。
pub fn parse_sheet_xml(xml: &str, ss: &[String]) -> Result<HashMap<(u32, u32), Cell>> {
    let mut out: HashMap<(u32, u32), Cell> = HashMap::new();
    // 共享字符串索引取不到时不能静默变空值：要么整份 SST 没读出来，要么文件本身坏了
    let mut bad_shared = 0usize;
    let mut xml_err: Option<String> = None;
    let mut reader = Reader::from_str(&xml);
    reader.config_mut().trim_text(false);

    let mut row: u32 = 0;
    let mut cell_ref: Option<(u32, u32)> = None;
    let mut cell_type = String::new();
    let mut in_v = false;
    let mut in_t = false;
    let mut in_is = false;
    let mut val = String::new();
    let mut formula: Option<String> = None;
    // 共享公式：主格存文本，子格只有 si，需要按位移推出来
    let mut masters: HashMap<String, (String, (u32, u32))> = HashMap::new();
    let mut f_si: Option<String> = None;

    macro_rules! flush {
        () => {
            if let Some((r, c)) = cell_ref.take() {
                let text = std::mem::take(&mut val);
                let value = match cell_type.as_str() {
                    "s" => {
                        let idx: usize = text.trim().parse().unwrap_or(usize::MAX);
                        match ss.get(idx) {
                            Some(s) if !s.is_empty() => Value::Text(s.clone().into()),
                            Some(_) => Value::Empty,
                            None => {
                                bad_shared += 1;
                                Value::Empty
                            }
                        }
                    }
                    "b" => Value::Bool(text.trim() == "1" || text.trim().eq_ignore_ascii_case("true")),
                    "str" | "inlineStr" | "e" => {
                        if text.is_empty() {
                            Value::Empty
                        } else {
                            Value::Text(text.clone().into())
                        }
                    }
                    _ => {
                        if text.trim().is_empty() {
                            Value::Empty
                        } else {
                            match text.trim().parse::<f64>() {
                                Ok(n) => Value::Number(n),
                                Err(_) => Value::Text(text.clone().into()),
                            }
                        }
                    }
                };
                let f = formula.take();
                if !matches!(value, Value::Empty) || f.is_some() {
                    out.insert((r, c), Cell { value, formula: f.map(|s| s.into()) });
                }
            } else {
                val.clear();
                formula = None;
            }
        };
    }

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => match tag_start(&e).as_str() {
                "row" => {
                    if let Some(a) = e.attributes().flatten().find(|a| a.key.as_ref() == "r") {
                        row = a.value.as_ref()
                            .parse::<u32>()
                            .map(|v| v.saturating_sub(1))
                            .unwrap_or(row);
                    }
                }
                "c" => {
                    cell_type.clear();
                    val.clear();
                    formula = None;
                    let mut r_attr: Option<(u32, u32)> = None;
                    for a in e.attributes().flatten() {
                        match a.key.as_ref() {
                            "r" => {
                                r_attr = parse_ref(a.value.as_ref());
                            }
                            "t" => cell_type = a.value.as_ref().to_string(),
                            _ => {}
                        }
                    }
                    cell_ref = r_attr.or(Some((row, 0)));
                }
                "v" => in_v = true,
                "f" => {
                    in_t = false;
                    f_si = None;
                    let mut is_shared = false;
                    for a in e.attributes().flatten() {
                        match a.key.as_ref() {
                            "t" => is_shared = a.value.as_ref() == "shared",
                            "si" => f_si = Some(a.value.as_ref().to_string()),
                            _ => {}
                        }
                    }
                    if !is_shared {
                        f_si = None;
                    }
                    // 公式文本在紧随的 Text 事件里
                    formula = Some(String::new());
                }
                "is" => in_is = true,
                "t" if in_is => in_t = true,
                _ => {}
            },
            // quick_xml 把 &gt; 这类实体单独发成 GeneralRef 事件；
            // 不处理就会把比较运算符整个丢掉（"A1&gt;5" 变成 "A15"）
            Ok(Event::GeneralRef(r)) => {
                let name = String::from_utf8_lossy(r.as_ref().as_ref()).to_string();
                let ch = match name.as_str() {
                    "gt" => Some('>'),
                    "lt" => Some('<'),
                    "amp" => Some('&'),
                    "quot" => Some('"'),
                    "apos" => Some('\''),
                    _ => None,
                };
                if let Some(c) = ch {
                    if in_v || in_t {
                        val.push(c);
                    } else if let Some(f) = formula.as_mut() {
                        f.push(c);
                    }
                }
            }
            Ok(Event::Text(t)) => {
                let s = t.as_ref().to_string();
                if in_v {
                    val.push_str(&s);
                } else if in_t {
                    val.push_str(&s);
                } else if formula.is_some() {
                    if let Some(f) = formula.as_mut() {
                        f.push_str(&s);
                    }
                }
            }
            Ok(Event::Empty(e)) => {
                if tag_start(&e) == "f" {
                    let mut shared = false;
                    let mut si = String::new();
                    for a in e.attributes().flatten() {
                        match a.key.as_ref() {
                            "t" => shared = a.value.as_ref() == "shared",
                            "si" => si = a.value.as_ref().to_string(),
                            _ => {}
                        }
                    }
                    if shared && !si.is_empty() {
                        if let Some((r, c)) = cell_ref {
                            if let Some((text, anchor)) = masters.get(&si) {
                                let dr = r as i64 - anchor.0 as i64;
                                let dc = c as i64 - anchor.1 as i64;
                                formula = Some(crate::normalize_formula(&format!("={}", shift_refs(text, dr, dc))));
                            }
                        }
                    }
                }
            }
            Ok(Event::End(e)) => match tag_end(&e).as_str() {
                "v" => in_v = false,
                "t" => in_t = false,
                "is" => in_is = false,
                "f" => {
                    let here = cell_ref;
                    if let Some(si) = f_si.take() {
                        let body = formula
                        .as_ref()
                        .map(|x| unescape(x.trim()))
                        .unwrap_or_default();
                        if !body.is_empty() {
                            if let Some(pos) = here {
                                masters.insert(si.clone(), (body.clone(), pos));
                            }
                            if let Some(f) = formula.as_mut() {
                                *f = crate::normalize_formula(&format!("={body}"));
                            }
                        } else {
                            let derived = here
                                .and_then(|pos| masters.get(&si).map(|(t, a)| {
                                    let dr = pos.0 as i64 - a.0 as i64;
                                    let dc = pos.1 as i64 - a.1 as i64;
                                    crate::normalize_formula(&format!("={}", shift_refs(t, dr, dc)))
                                }))
                                .unwrap_or_default();
                            formula = Some(derived);
                        }
                    } else if let Some(f) = formula.as_mut() {
                        let t = unescape(f.trim());
                        if !t.is_empty() {
                            *f = crate::normalize_formula(&format!("={t}"));
                        }
                    }
                }
                "c" => flush!(),
                _ => {}
            },
            Ok(Event::Eof) => break,
            Err(e) => { xml_err = Some(format!("{e}")); break; }
            _ => {}
        }
    }
    if let Some(e) = xml_err {
        anyhow::bail!("工作表 XML 解析失败：{e}");
    }
    // 文档必须以 </worksheet> 收尾：calamine/quick-xml 对"截断但在标签边界"的文件
    // 都可能安静地返回半张表，这里补一道结构检查。
    let closed = {
        let t = xml.trim_end();
        match t.rfind("</") {
            Some(i) => {
                let tail = &t[i + 2..];
                let name = tail.trim_end_matches('>').trim();
                name.rsplit(':').next().unwrap_or(name) == "worksheet"
            }
            None => false,
        }
    };
    if !closed {
        anyhow::bail!("工作表 XML 未正常结束（文件可能被截断）");
    }
    if bad_shared > 0 {
        anyhow::bail!("{bad_shared} 个单元格引用了不存在的共享字符串（sharedStrings 缺失或损坏）");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cell_refs() {
        assert_eq!(parse_ref("A1"), Some((0, 0)));
        assert_eq!(parse_ref("B12"), Some((11, 1)));
        assert_eq!(parse_ref("XFD1048576"), Some((1048575, 16383)));
        assert_eq!(parse_ref("12"), None);
    }


    #[test]
    fn shift_moves_relative_refs_only() {
        assert_eq!(shift_refs("SUM(A1:B2)", 1, 0), "SUM(A2:B3)");
        assert_eq!(shift_refs("$A$1+A1", 2, 3), "$A$1+D3");
        assert_eq!(shift_refs("$A1+A$1", 1, 1), "$A2+B$1");
        assert_eq!(shift_refs("'参数与字典'!$B$7", 0, 0), "'参数与字典'!$B$7");
        assert_eq!(shift_refs("'参数 表'!B7", 1, 0), "'参数 表'!B8");
        assert_eq!(shift_refs("LOG10(A1)", 1, 0), "LOG10(A2)", "函数名不能被当成引用");
        assert_eq!(shift_refs("\"A1\"&A1", 1, 0), "\"A1\"&A2", "字符串里的不动");
    }


    #[test]
    fn formula_entities_are_unescaped() {
        // Excel 把 > < 写成实体；不还原的话比较运算会整条丢掉
        let xml = r#"<worksheet><sheetData>
            <row r="1"><c r="B1"><f>IF(A1&gt;5,1,0)</f><v>1</v></c></row>
            <row r="2"><c r="B2"><f>IF(A2&lt;=3,"小","大")</f><v>2</v></c></row>
            <row r="3"><c r="B3"><f>IF(AND(A3&gt;=1,A3&lt;9),"中","外")</f><v>3</v></c></row>
        </sheetData></worksheet>"#;
        let cells = parse_sheet_xml(xml, &[]).expect("应能解析");
        let f = |r: u32| {
            cells
                .get(&(r, 1))
                .and_then(|c| c.formula.as_ref().map(|x| x.to_string()))
                .unwrap_or_default()
        };
        assert_eq!(f(0), "=IF(A1>5,1,0)");
        assert_eq!(f(1), "=IF(A2<=3,\"小\",\"大\")");
        assert_eq!(f(2), "=IF(AND(A3>=1,A3<9),\"中\",\"外\")");
    }

    #[test]
    fn shared_formula_children_are_derived() {
        // G6 是主格，G7 是带结束标签的空 f，G8 是自闭合 f
        let xml = r#"<worksheet><sheetData>
            <row r="6"><c r="G6"><f t="shared" ref="G6:G8" si="0">IF(ISNUMBER(E6),D6*2,0)</f><v>1</v></c></row>
            <row r="7"><c r="G7"><f t="shared" si="0"></f><v>2</v></c></row>
            <row r="8"><c r="G8"><f t="shared" si="0"/><v>3</v></c></row>
        </sheetData></worksheet>"#;
        let cells = parse_sheet_xml(xml, &[]).expect("应能解析");
        let f = |r: u32| {
            cells
                .get(&(r, 6))
                .and_then(|c| c.formula.as_ref().map(|x| x.to_string()))
                .unwrap_or_default()
        };
        assert_eq!(f(5), "=IF(ISNUMBER(E6),D6*2,0)");
        assert_eq!(f(6), "=IF(ISNUMBER(E7),D7*2,0)", "第 7 行要按 +1 平移");
        assert_eq!(f(7), "=IF(ISNUMBER(E8),D8*2,0)", "自闭合子格也要平移");
    }

    #[test]
    fn computes_declared_area() {
        let head = r#"<worksheet><dimension ref="A1:XFD1048576"/><sheetData>"#;
        assert_eq!(declared_area(head), 1048576 * 16384);
        let small = r#"<worksheet><dimension ref="A1:H20001"/>"#;
        assert_eq!(declared_area(small), 20001 * 8);
        assert_eq!(declared_area("<worksheet>"), 0);
    }
}

/// 工作簿里是否有单元格真的引用了共享字符串（`t="s"`）。
/// 没有 SST 部件且没人引用时不算问题；有人引用才是数据丢失。
pub fn any_sheet_uses_shared_strings(path: &Path) -> bool {
    use std::io::Read;
    let Ok(file) = std::fs::File::open(path) else { return false };
    let Ok(mut zip) = zip::ZipArchive::new(file) else { return false };
    let names: Vec<String> = (0..zip.len())
        .filter_map(|i| zip.name_for_index(i).map(|s| s.to_string()))
        .filter(|n| n.starts_with("xl/worksheets/") && n.ends_with(".xml"))
        .collect();
    for n in names {
        let Ok(mut f) = zip.by_name(&n) else { continue };
        let mut xml = String::new();
        if f.read_to_string(&mut xml).is_err() {
            continue;
        }
        if xml.contains("t=\"s\"") || xml.contains("t='s'") {
            return true;
        }
    }
    false
}
