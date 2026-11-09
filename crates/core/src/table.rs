//! Excel 表格对象（ListObject）：读取 / 写回 / 结构化引用展开。
//!
//! 表格在文件里是独立的部件：`xl/tables/tableN.xml`，
//! 工作表 XML 里用 `<tableParts><tablePart r:id="rIdN"/></tableParts>` 引它。
//! 结构化引用（`Table1[金额]`）在求值前展开成 A1 区域。

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Table {
    /// 程序用的名字（不能有空格），公式里写这个
    pub name: String,
    /// 显示名（默认同 name）
    pub display: String,
    /// 表格区域（含标题行、含汇总行）
    pub range: (u32, u32, u32, u32),
    /// 列名，顺序与列一致
    pub columns: Vec<String>,
    /// 首行是不是标题
    pub header_row: bool,
    /// 末行是不是汇总行
    pub totals_row: bool,
}

fn col_label(mut c: u32) -> String {
    let mut s = String::new();
    loop {
        s.insert(0, (b'A' + (c % 26) as u8) as char);
        if c < 26 {
            break;
        }
        c = c / 26 - 1;
    }
    s
}

fn col_num(s: &str) -> Option<u32> {
    let mut n: u32 = 0;
    let mut any = false;
    for ch in s.chars() {
        let d = ch.to_ascii_uppercase();
        if !d.is_ascii_uppercase() {
            break;
        }
        n = n * 26 + (d as u32 - 'A' as u32 + 1);
        any = true;
    }
    if any { Some(n - 1) } else { None }
}

pub fn a1(r: (u32, u32, u32, u32)) -> String {
    format!("{}{}:{}{}", col_label(r.1), r.0 + 1, col_label(r.3), r.2 + 1)
}

pub fn parse_a1(s: &str) -> Option<(u32, u32, u32, u32)> {
    let (a, b) = s.trim().split_once(':')?;
    let one = |t: &str| -> Option<(u32, u32)> {
        let t = t.replace('$', "");
        let letters: String = t.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
        let digits: String = t.chars().skip(letters.len()).collect();
        Some((digits.parse::<u32>().ok()?.checked_sub(1)?, col_num(&letters)?))
    };
    let (r1, c1) = one(a)?;
    let (r2, c2) = one(b)?;
    Some((r1.min(r2), c1.min(c2), r1.max(r2), c1.max(c2)))
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// 从一段 tableDefinition XML 里抠出字段
pub fn parse_table_xml(xml: &str) -> Option<Table> {
    let attr = |key: &str| -> Option<String> {
        let pat = format!("{key}=\"");
        let i = xml.find(&pat)? + pat.len();
        let rest = &xml[i..];
        let end = rest.find('"')?;
        Some(unescape(&rest[..end]))
    };
    let mut t = Table {
        name: attr("name").unwrap_or_default(),
        display: attr("displayName").unwrap_or_default(),
        range: parse_a1(&attr("ref")?)?,
        columns: Vec::new(),
        header_row: attr("headerRowCount").map(|v| v != "0").unwrap_or(true),
        totals_row: attr("totalsRowCount").map(|v| v != "0").unwrap_or(false),
    };
    if t.display.is_empty() {
        t.display = t.name.clone();
    }
    // 列名：<tableColumn ... name="xxx"/> 里的 name，注意 id 在前面
    let mut rest = xml;
    while let Some(pos) = rest.find("<tableColumn") {
        let tag_end = rest[pos..].find('>')?;
        let tag = &rest[pos..pos + tag_end];
        if let Some(npos) = tag.find(" name=\"") {
            let v = &tag[npos + 7..];
            if let Some(q) = v.find('"') {
                t.columns.push(unescape(&v[..q]));
            }
        }
        rest = &rest[pos + tag_end..];
    }
    Some(t)
}

/// 读整本文件：表名 → 该表上的表格对象
/// 关系里的 Target 是相对工作表部件所在目录的：
/// xl/stress/worksheets/sheet5.xml + ../tables/table1.xml → xl/stress/tables/table1.xml
fn resolve_rel(base_dir: &str, target: &str) -> String {
    if let Some(abs) = target.strip_prefix('/') {
        return abs.to_string();
    }
    let mut segs: Vec<&str> = base_dir.split('/').filter(|s| !s.is_empty()).collect();
    for p in target.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                segs.pop();
            }
            other => segs.push(other),
        }
    }
    segs.join("/")
}

pub fn load(path: &Path) -> HashMap<String, Vec<Table>> {
    let mut out: HashMap<String, Vec<Table>> = HashMap::new();
    let Ok(targets) = crate::patch::sheet_targets(path) else {
        return out;
    };
    let Ok(file) = std::fs::File::open(path) else {
        return out;
    };
    let Ok(mut zip) = zip::ZipArchive::new(file) else {
        return out;
    };
    // 工作表 rels：找 table 类型的关系指向哪个部件
    for (sheet_name, part) in targets.iter() {
        let rels = part.replace("worksheets/", "worksheets/_rels/") + ".rels";
        let mut xml = String::new();
        {
            let Ok(mut f) = zip.by_name(&rels) else { continue };
            if f.read_to_string(&mut xml).is_err() {
                continue;
            }
        }
        let mut rest = xml.as_str();
        while let Some(pos) = rest.find("<Relationship ") {
            let end = rest[pos..].find("/>").map(|e| pos + e).unwrap_or(rest.len());
            let tag = &rest[pos..end];
            if tag.contains("/table\"") {
                if let Some(tpos) = tag.find("Target=\"") {
                    let v = &tag[tpos + 8..];
                    if let Some(q) = v.find('"') {
                        let target = v[..q].to_string();
                        let base_dir = part.rsplit_once('/').map(|(d, _)| d).unwrap_or("xl/worksheets");
                        let full = resolve_rel(base_dir, &target);
                        if let Ok(mut tf) = zip.by_name(&full) {
                            let mut tx = String::new();
                            if tf.read_to_string(&mut tx).is_ok() {
                                if let Some(t) = parse_table_xml(&tx) {
                                    out.entry(sheet_name.clone()).or_default().push(t);
                                }
                            }
                        }
                    }
                }
            }
            rest = &rest[end.min(rest.len())..];
        }
    }
    out
}

/// 生成 tableN.xml
pub fn table_xml(t: &Table, id: u32) -> String {
    let mut cols = String::new();
    for (i, name) in t.columns.iter().enumerate() {
        cols.push_str(&format!("<tableColumn id=\"{}\" name=\"{}\"/>", i + 1, esc(name)));
    }
    let count = t.columns.len();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<table xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" id=\"{id}\" \
name=\"{name}\" displayName=\"{display}\" ref=\"{range}\" totalsRowShown=\"0\">\
<autoFilter ref=\"{range}\"/><tableColumns count=\"{count}\">{cols}</tableColumns>\
<tableStyleInfo name=\"TableStyleMedium2\" showFirstColumn=\"0\" showLastColumn=\"0\" \
showRowStripes=\"1\" showColumnStripes=\"0\"/></table>",
        name = esc(&t.name),
        display = esc(if t.display.is_empty() { &t.name } else { &t.display }),
        range = a1(t.range),
        count = count,
        cols = cols
    )
}

/// 把表格对象写进文件（追加部件 + 工作表 tableParts + 关系 + Content_Types）
pub fn attach(path: &Path, sheet_name: &str, tables: &[Table]) -> anyhow::Result<()> {
    if tables.is_empty() {
        return Ok(());
    }
    let targets = crate::patch::sheet_targets(path)?;
    let Some(part) = targets.get(sheet_name) else {
        return Ok(());
    };
    let part = part.clone();
    let rels_path = part.replace("worksheets/", "worksheets/_rels/") + ".rels";

    // 找一个没被占用的编号
    let mut next_id = 1u32;
    if let Ok(file) = std::fs::File::open(path) {
        if let Ok(mut zip) = zip::ZipArchive::new(file) {
            for i in 0..zip.len() {
                if let Ok(e) = zip.by_index(i) {
                    if let Some(n) = e.name().strip_prefix("xl/tables/table") {
                        if let Some(num) = n.strip_suffix(".xml").and_then(|v| v.parse::<u32>().ok()) {
                            next_id = next_id.max(num + 1);
                        }
                    }
                }
            }
        }
    }

    let mut parts: Vec<(String, String)> = Vec::new();
    let mut refs = String::new();
    for (i, t) in tables.iter().enumerate() {
        let id = next_id + i as u32;
        let rid = format!("rIdTable{id}");
        parts.push((format!("xl/tables/table{id}.xml"), table_xml(t, id)));
        refs.push_str(&format!(
            "<Relationship Id=\"{rid}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/table\" Target=\"../tables/table{id}.xml\"/>"
        ));
    }
    for (name, body) in parts.iter() {
        crate::patch::append_entry(path, name, body.as_bytes())?;
    }
    // 工作表原本没有 rels 的话要新建，否则 Excel 找不到表格部件
    let rels_exists = {
        let mut has = false;
        if let Ok(file) = std::fs::File::open(path) {
            if let Ok(mut zip) = zip::ZipArchive::new(file) {
                has = zip.by_name(&rels_path).is_ok();
            }
        }
        has
    };
    if !rels_exists {
        let body = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">{refs}</Relationships>"
        );
        crate::patch::append_entry(path, &rels_path, body.as_bytes())?;
    }

    // 工作表：<tableParts>
    let count = tables.len();
    let mut parts_xml = String::new();
    for (i, _) in tables.iter().enumerate() {
        parts_xml.push_str(&format!("<tablePart r:id=\"rIdTable{}\"/>", next_id + i as u32));
    }
    let table_parts = format!("<tableParts count=\"{count}\">{parts_xml}</tableParts>");
    let rels_add = refs.clone();
    let part_c = part.clone();
    crate::patch::rewrite_zip(path, |name, buf| {
        if name == part_c {
            if let Ok(xml) = String::from_utf8(buf.clone()) {
                if !xml.contains("<tableParts") {
                    // schema 顺序：tableParts 在 extLst 之前、pageSetup 之后
                    let x = if let Some(pos) = xml.find("<extLst") {
                        let mut s = xml.clone();
                        s.insert_str(pos, &table_parts);
                        s
                    } else if let Some(pos) = xml.rfind("</worksheet>") {
                        let mut s = xml.clone();
                        s.insert_str(pos, &table_parts);
                        s
                    } else {
                        xml.clone()
                    };
                    return x.into_bytes();
                }
            }
        }
        if name == rels_path {
            if let Ok(xml) = String::from_utf8(buf.clone()) {
                if !xml.contains("/table\"") {
                    return xml
                        .replace("</Relationships>", &format!("{rels_add}</Relationships>"))
                        .into_bytes();
                }
            }
        }
        if name == "[Content_Types].xml" {
            if let Ok(xml) = String::from_utf8(buf.clone()) {
                let mut add = String::new();
                for i in 0..tables.len() {
                    let id = next_id + i as u32;
                    let p = format!("/xl/tables/table{id}.xml");
                    if !xml.contains(&p) {
                        add.push_str(&format!(
                            "<Override PartName=\"{p}\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.table+xml\"/>"
                        ));
                    }
                }
                if !add.is_empty() {
                    return xml.replace("</Types>", &format!("{add}</Types>")).into_bytes();
                }
            }
        }
        buf
    })?;
    Ok(())
}

/// 结构化引用展开：`Table1[金额]` → `D2:D9`
///
/// 只处理本表/跨表两种常见写法；找不到对应表格或列就原样返回（让解析器报错）。
pub fn expand_structured(wb: &crate::model::Workbook, si: usize, body: &str) -> Option<String> {
    if !body.contains('[') {
        return None;
    }
    let chars: Vec<char> = body.chars().collect();
    let mut out = String::with_capacity(body.len() + 16);
    let mut changed = false;
    let mut i = 0usize;
    while i < chars.len() {
        let ch = chars[i];
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '.' {
            let start = i;
            while i < chars.len()
                && (chars[i].is_ascii_alphanumeric() || chars[i] == '_' || chars[i] == '.')
            {
                i += 1;
            }
            // 前面紧挨着字母/数字说明是长标识符的一部分，不动
            let name: String = chars[start..i].iter().collect();
            if i < chars.len() && chars[i] == '[' && start > 0 {
                let mut j = i;
                let mut depth = 0i32;
                while j < chars.len() {
                    if chars[j] == '[' {
                        depth += 1;
                    } else if chars[j] == ']' {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    j += 1;
                }
                if j < chars.len() {
                    let inner: String = chars[i + 1..j].iter().collect();
                    let col = inner.trim().trim_start_matches('[').trim_end_matches(']').trim();
                    if let Some((tbl, on_si)) = find_table(wb, si, &name) {
                        if let Some(idx) = tbl
                            .columns
                            .iter()
                            .position(|c| c.eq_ignore_ascii_case(col))
                        {
                            let (r1, c1, r2, _) = tbl.range;
                            let head = if tbl.header_row { 1 } else { 0 };
                            let tail = if tbl.totals_row { 1 } else { 0 };
                            let rr1 = r1 + head;
                            let rr2 = r2.saturating_sub(tail);
                            let colc = c1 + idx as u32;
                            let rng = format!("{}{}:{}{}", col_label(colc), rr1 + 1, col_label(colc), rr2 + 1);
                            if on_si == si {
                                out.push_str(&rng);
                            } else {
                                out.push_str(&format!("'{}'!{rng}", wb.sheets[on_si].name));
                            }
                            changed = true;
                            i = j + 1;
                            continue;
                        }
                    }
                }
            }
            out.push_str(&name);
            continue;
        }
        out.push(ch);
        i += 1;
    }
    if changed { Some(out) } else { None }
}

fn find_table(
    wb: &crate::model::Workbook,
    si: usize,
    name: &str,
) -> Option<(Table, usize)> {
    for (idx, sheet) in wb.sheets.iter().enumerate() {
        for t in sheet.tables.iter() {
            if t.name.eq_ignore_ascii_case(name) || t.display.eq_ignore_ascii_case(name) {
                return Some((t.clone(), idx));
            }
        }
    }
    let _ = si;
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_definition() {
        let xml = r#"<table id="1" name="Table1" displayName="Table1" ref="A1:C4" totalsRowShown="0"><tableColumns count="3"><tableColumn id="1" name="姓名"/><tableColumn id="2" name="部门"/><tableColumn id="3" name="金额"/></tableColumns></table>"#;
        let t = parse_table_xml(xml).unwrap();
        assert_eq!(t.name, "Table1");
        assert_eq!(t.range, (0, 0, 3, 2));
        assert_eq!(t.columns, vec!["姓名", "部门", "金额"]);
        assert!(t.header_row);
    }

    #[test]
    fn a1_roundtrip() {
        assert_eq!(a1((0, 0, 3, 2)), "A1:C4");
        assert_eq!(parse_a1("A1:C4"), Some((0, 0, 3, 2)));
    }
}
