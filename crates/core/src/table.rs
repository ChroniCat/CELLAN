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
        if t.totals_row {
            if i == 0 {
                cols.push_str(&format!(
                    "<tableColumn id=\"{}\" name=\"{}\" totalsRowLabel=\"总计\"/>",
                    i + 1,
                    esc(name)
                ));
            } else {
                cols.push_str(&format!(
                    "<tableColumn id=\"{}\" name=\"{}\" totalsRowFunction=\"sum\"/>",
                    i + 1,
                    esc(name)
                ));
            }
        } else {
            cols.push_str(&format!("<tableColumn id=\"{}\" name=\"{}\"/>", i + 1, esc(name)));
        }
    }
    let count = t.columns.len();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<table xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" id=\"{id}\" \
name=\"{name}\" displayName=\"{display}\" ref=\"{range}\" totalsRowShown=\"{totals}\"{totals_count}>\
<autoFilter ref=\"{filter_range}\"/><tableColumns count=\"{count}\">{cols}</tableColumns>\
<tableStyleInfo name=\"TableStyleMedium2\" showFirstColumn=\"0\" showLastColumn=\"0\" \
showRowStripes=\"1\" showColumnStripes=\"0\"/></table>",
        name = esc(&t.name),
        totals = if t.totals_row { 1 } else { 0 },
        totals_count = if t.totals_row { " totalsRowCount=\"1\"" } else { "" },
        filter_range = if t.totals_row {
            a1((t.range.0, t.range.1, t.range.2.saturating_sub(1), t.range.3))
        } else {
            a1(t.range)
        },
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
/// 支持 Excel 的结构化引用主体：
/// - 裸列：`Table[金额]`（默认数据区，不含标题/汇总）
/// - 区域：`Table[[#Data],[金额]]`、`#Headers`、`#Totals`、`#All`
/// - 当前行：`Table[@金额]`、`Table[[#This Row],[金额]]`
/// - 列区间：`Table[[金额]:[税额]]`
///
/// 找不到表格时保留原文（让解析器报 #NAME?）；找到表格但列/区域非法时
/// 返回 Excel 的错误值文本（`#REF!`/`#VALUE!`），不能静默改成别的区域。
pub fn expand_structured(wb: &crate::model::Workbook, si: usize, body: &str) -> Option<String> {
    expand_structured_at(wb, si, None, None, body)
}

/// 带当前公式位置的结构化引用展开。`cur_row`/`cur_col` 决定 `@` / `#This Row`
/// 的归属表；同一行有多张表时必须用公式列定位，不能取第一张。
pub fn expand_structured_at(
    wb: &crate::model::Workbook,
    si: usize,
    cur_row: Option<u32>,
    cur_col: Option<u32>,
    body: &str,
) -> Option<String> {
    if !body.contains('[') {
        return None;
    }
    // 引号里是数据：`="Sales[Amount]"` 不能变成 `="A2:A4"`；
    // `'Sales'!A1` 里的 `Sales` 是工作表名，也不能当表格展开。
    let mut out = String::with_capacity(body.len() + 16);
    let mut changed = false;
    for (in_quote, seg) in crate::split_quoted(body) {
        if in_quote {
            out.push_str(seg);
            continue;
        }
        match expand_segment_at(wb, si, cur_row, cur_col, seg) {
            Some(s) => {
                out.push_str(&s);
                changed = true;
            }
            None => out.push_str(seg),
        }
    }
    if changed { Some(out) } else { None }
}

/// 只展开一段**引号外**文本里的结构化引用。
fn expand_segment_at(
    wb: &crate::model::Workbook,
    si: usize,
    cur_row: Option<u32>,
    cur_col: Option<u32>,
    body: &str,
) -> Option<String> {
    if !body.contains('[') {
        return None;
    }
    let chars: Vec<char> = body.chars().collect();
    let mut out = String::with_capacity(body.len() + 16);
    let mut changed = false;
    let mut i = 0usize;
    while i < chars.len() {
        let ch = chars[i];
        // 表内当前行简写：`[@金额]` / `[[#This Row],[金额]]`（没有表名）。
        // 必须先按公式单元格定位所属表，否则同一行多张表会取错。
        if ch == '[' {
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
                if inner.trim_start().starts_with('@')
                    || inner.to_ascii_uppercase().contains("#THIS ROW")
                {
                    let (Some(row), Some(col)) = (cur_row, cur_col) else {
                        out.push_str("#VALUE!");
                        changed = true;
                        i = j + 1;
                        continue;
                    };
                    if let Some((tbl, on_si)) = find_table_at(wb, si, row, col) {
                        out.push_str(&expand_table_spec(wb, si, on_si, cur_row, &tbl, &inner));
                    } else {
                        out.push_str("#VALUE!");
                    }
                    changed = true;
                    i = j + 1;
                    continue;
                }
            }
        }
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '.' || ch == '\\' {
            let start = i;
            while i < chars.len()
                && (chars[i].is_ascii_alphanumeric()
                    || chars[i] == '_'
                    || chars[i] == '.'
                    || chars[i] == '\\')
            {
                i += 1;
            }
            let name: String = chars[start..i].iter().collect();
            // 表名可以出现在公式体开头（`Sales[Amount]`），不能要求 start > 0。
            if i < chars.len() && chars[i] == '[' {
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
                    if let Some((tbl, on_si)) = find_table(wb, si, &name) {
                        out.push_str(&expand_table_spec(wb, si, on_si, cur_row, &tbl, &inner));
                        changed = true;
                        i = j + 1;
                        continue;
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

fn sheet_prefix(wb: &crate::model::Workbook, on_si: usize, si: usize) -> String {
    if on_si == si {
        String::new()
    } else {
        format!("'{}'!", wb.sheets[on_si].name.replace('\'', "''"))
    }
}

fn ref_text(r1: u32, c1: u32, r2: u32, c2: u32) -> String {
    if r1 == r2 && c1 == c2 {
        format!("{}{}", col_label(c1), r1 + 1)
    } else {
        format!(
            "{}{}:{}{}",
            col_label(c1),
            r1 + 1,
            col_label(c2),
            r2 + 1
        )
    }
}

fn strip_outer_brackets(s: &str) -> &str {
    let t = s.trim();
    if t.len() >= 2 && t.starts_with('[') && t.ends_with(']') {
        &t[1..t.len() - 1]
    } else {
        t
    }
}

/// 按顶层逗号切分说明符；列名里的逗号在 `[...]` 内，不切。
fn split_specs(s: &str) -> Vec<String> {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut depth = 0i32;
    for (i, ch) in chars.iter().enumerate() {
        match ch {
            '[' => depth += 1,
            ']' => depth -= 1,
            ',' if depth == 0 => {
                let piece: String = chars[start..i].iter().collect();
                if !piece.trim().is_empty() {
                    out.push(piece);
                }
                start = i + 1;
            }
            _ => {}
        }
    }
    if start < chars.len() {
        let piece: String = chars[start..].iter().collect();
        if !piece.trim().is_empty() {
            out.push(piece);
        }
    }
    out
}

fn split_colon(s: &str) -> Option<(String, String)> {
    let chars: Vec<char> = s.chars().collect();
    let mut depth = 0i32;
    for (i, ch) in chars.iter().enumerate() {
        match ch {
            '[' => depth += 1,
            ']' => depth -= 1,
            ':' if depth == 0 => {
                let a: String = chars[..i].iter().collect();
                let b: String = chars[i + 1..].iter().collect();
                return Some((a, b));
            }
            _ => {}
        }
    }
    None
}

fn expand_table_spec(
    wb: &crate::model::Workbook,
    si: usize,
    on_si: usize,
    cur_row: Option<u32>,
    tbl: &Table,
    inner: &str,
) -> String {
    let (r1, c1, r2, c2) = tbl.range;
    let data_start = if tbl.header_row { r1 + 1 } else { r1 };
    let data_end = if tbl.totals_row {
        r2.saturating_sub(1)
    } else {
        r2
    };
    let col_count = (c2.saturating_sub(c1) + 1) as usize;
    let mut area: Option<&str> = None;
    let mut this_row = false;
    let mut col_start: Option<usize> = None;
    let mut col_end: Option<usize> = None;
    let mut bad = false;

    let specs = split_specs(inner);
    if specs.is_empty() {
        return "#REF!".to_string();
    }
    for raw in specs {
        let mut tok = raw.trim().to_string();
        if let Some(rest) = tok.strip_prefix('@') {
            this_row = true;
            tok = rest.trim().to_string();
        }
        // 列区间 [Col1]:[Col2] 的冒号在方括号外，必须先切冒号，
        // 否则整串会被误当成一个 `[... ]` 说明符。
        if let Some((a, b)) = split_colon(&tok) {
            let ca = strip_outer_brackets(&a).trim().to_string();
            let cb = strip_outer_brackets(&b).trim().to_string();
            if ca.is_empty() || cb.is_empty() {
                bad = true;
                continue;
            }
            let ia = tbl
                .columns
                .iter()
                .position(|c| c.eq_ignore_ascii_case(&ca));
            let ib = tbl
                .columns
                .iter()
                .position(|c| c.eq_ignore_ascii_case(&cb));
            match (ia, ib) {
                (Some(x), Some(y)) => {
                    col_start = Some(x.min(y));
                    col_end = Some(x.max(y));
                }
                _ => bad = true,
            }
            continue;
        }
        // 普通说明符去掉一层方括号：[#Data] / [Amount] / [@[Amount]]
        if tok.starts_with('[') && tok.ends_with(']') {
            tok = strip_outer_brackets(&tok).to_string();
        }
        let up = tok.to_ascii_uppercase();
        if up == "#THIS ROW" {
            this_row = true;
            continue;
        }
        if up.starts_with('#') {
            let a = match up.as_str() {
                "#DATA" => "data",
                "#HEADERS" => "headers",
                "#TOTALS" => "totals",
                "#ALL" => "all",
                _ => {
                    bad = true;
                    continue;
                }
            };
            if area.is_some() && area != Some(a) {
                bad = true;
            } else {
                area = Some(a);
            }
            continue;
        }
        if tok.is_empty() {
            // `Table[@]` / `Table[[#This Row]]`：当前行所有列，允许没有列名。
            if this_row {
                continue;
            }
            bad = true;
            continue;
        }
        let idx = tbl.columns.iter().position(|c| c.eq_ignore_ascii_case(&tok));
        match idx {
            Some(x) => {
                if col_start.is_some() {
                    bad = true;
                } else {
                    col_start = Some(x);
                    col_end = Some(x);
                }
            }
            None => bad = true,
        }
    }

    if bad {
        return "#REF!".to_string();
    }
    if this_row && area.is_some() && area != Some("data") {
        return "#REF!".to_string();
    }

    let (cs, ce) = match (col_start, col_end) {
        (Some(a), Some(b)) => (a, b),
        _ => (0, col_count.saturating_sub(1) as usize),
    };
    if col_count == 0 || cs >= col_count || ce >= col_count {
        return "#REF!".to_string();
    }
    let abs_c1 = c1 + cs as u32;
    let abs_c2 = c1 + ce as u32;
    let prefix = sheet_prefix(wb, on_si, si);

    let (out_r1, out_r2) = if this_row {
        let Some(row) = cur_row else {
            return "#VALUE!".to_string();
        };
        if row < data_start || row > data_end {
            return "#VALUE!".to_string();
        }
        (row, row)
    } else {
        match area.unwrap_or("data") {
            "all" => (r1, r2),
            "headers" => {
                if !tbl.header_row {
                    return "#REF!".to_string();
                }
                (r1, r1)
            }
            "totals" => {
                if !tbl.totals_row || r2 <= r1 {
                    return "#REF!".to_string();
                }
                (r2, r2)
            }
            "data" => {
                if data_start > data_end {
                    return "#REF!".to_string();
                }
                (data_start, data_end)
            }
            _ => return "#REF!".to_string(),
        }
    };
    format!("{prefix}{}", ref_text(out_r1, abs_c1, out_r2, abs_c2))
}

fn find_table_at(
    wb: &crate::model::Workbook,
    si: usize,
    row: u32,
    col: u32,
) -> Option<(Table, usize)> {
    wb.sheets.get(si)?.tables.iter().find_map(|t| {
        if t.range.0 <= row && row <= t.range.2 && t.range.1 <= col && col <= t.range.3 {
            Some((t.clone(), si))
        } else {
            None
        }
    })
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
        let xml = r#"<table id="1" name="Table1" displayName="Table1" ref="A1:C4" totalsRowShown="{}"><tableColumns count="3"><tableColumn id="1" name="姓名"/><tableColumn id="2" name="部门"/><tableColumn id="3" name="金额"/></tableColumns></table>"#;
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

fn attr_of(tag: &str, key: &str) -> Option<String> {
    let pat = format!("{key}=\"");
    let i = tag.find(&pat)? + pat.len();
    let rest = &tag[i..];
    let j = rest.find('"')?;
    Some(rest[..j].to_string())
}

/// 把 sheet 的表格部件同步成 `tables`：删除旧的 tableN.xml / rels / tableParts，
/// 再按当前模型重新生成（`tables` 为空就是“转换为区域”）。
pub fn sync(path: &Path, sheet_name: &str, tables: &[Table]) -> anyhow::Result<()> {
    let targets = crate::patch::sheet_targets(path)?;
    let Some(part) = targets.get(sheet_name).cloned() else {
        return Ok(());
    };
    let rels_path = part.replace("worksheets/", "worksheets/_rels/") + ".rels";

    let mut rels_xml = String::new();
    if let Ok(file) = std::fs::File::open(path) {
        if let Ok(mut zip) = zip::ZipArchive::new(file) {
            if let Ok(mut f) = zip.by_name(&rels_path) {
                let _ = f.read_to_string(&mut rels_xml);
            }
        }
    }
    let mut old_parts: Vec<String> = Vec::new();
    for chunk in rels_xml.split("<Relationship").skip(1) {
        let end = chunk.find("/>").map(|i| i + 2).unwrap_or(chunk.len());
        let tag = &chunk[..end];
        if tag.contains("relationships/table") {
            if let Some(t) = attr_of(tag, "Target") {
                let p = if let Some(rest) = t.strip_prefix("../") {
                    // 相对 xl/worksheets/ 的上级是 xl/
                    let dir = part.rsplit_once('/').map(|x| x.0).unwrap_or("");
                    let parent = dir.rsplit_once('/').map(|x| x.0).unwrap_or("");
                    format!("{parent}/{rest}")
                } else if let Some(rest) = t.strip_prefix('/') {
                    rest.to_string()
                } else {
                    t
                };
                old_parts.push(p);
            }
        }
    }
    if old_parts.is_empty() && tables.is_empty() {
        return Ok(());
    }
    let skip: std::collections::HashSet<String> = old_parts.iter().cloned().collect();
    let originals = old_parts.clone();
    crate::patch::rewrite_zip_skip(path, &skip, |name, buf| {
        if name == rels_path {
            if let Ok(xml) = String::from_utf8(buf.clone()) {
                let mut out = String::with_capacity(xml.len());
                let mut rest = xml.as_str();
                while let Some(i) = rest.find("<Relationship") {
                    out.push_str(&rest[..i]);
                    let tail = &rest[i..];
                    // 结束位置取第一个 '>'：根元素 <Relationships ...> 和自闭合 <Relationship .../> 都适用
                    let end = tail.find('>').map(|v| v + 1).unwrap_or(tail.len());
                    let tag = &tail[..end];
                    // 注意 <Relationships> 根元素也以 "<Relationship" 开头，不能删
                    if tag.starts_with("<Relationships") || !tag.contains("relationships/table") {
                        out.push_str(tag);
                    }
                    rest = &tail[end..];
                }
                out.push_str(rest);
                return out.into_bytes();
            }
        }
        if name == "[Content_Types].xml" {
            if let Ok(xml) = String::from_utf8(buf.clone()) {
                let mut out = String::with_capacity(xml.len());
                let mut rest = xml.as_str();
                while let Some(i) = rest.find("<Override") {
                    out.push_str(&rest[..i]);
                    let tail = &rest[i..];
                    let end = tail.find("/>").map(|v| v + 2).unwrap_or(tail.len());
                    let tag = &tail[..end];
                    let drop = originals.iter().any(|p| tag.contains(&format!("\"/{p}\"")));
                    if !drop {
                        out.push_str(tag);
                    }
                    rest = &tail[end..];
                }
                out.push_str(rest);
                return out.into_bytes();
            }
        }
        if name == part {
            if let Ok(xml) = String::from_utf8(buf.clone()) {
                if let Some(i) = xml.find("<tableParts") {
                    let tail = &xml[i..];
                    let first_gt = tail.find('>').unwrap_or(tail.len() - 1);
                    let self_closing = tail[..first_gt].trim_end().ends_with('/');
                    let end = if self_closing {
                        first_gt + 1
                    } else {
                        tail.find("</tableParts>")
                            .map(|c| c + "</tableParts>".len())
                            .unwrap_or(first_gt + 1)
                    };
                    let mut out = String::with_capacity(xml.len());
                    out.push_str(&xml[..i]);
                    out.push_str(&xml[i + end..]);
                    return out.into_bytes();
                }
            }
        }
        buf
    })?;
    if !tables.is_empty() {
        attach(path, sheet_name, tables)?;
    }
    Ok(())
}

#[cfg(test)]
mod sync_tests {
    use super::*;
    use crate::model::{Cell, Value};

    fn zip_has(path: &Path, prefix: &str) -> Vec<String> {
        let mut out = Vec::new();
        if let Ok(file) = std::fs::File::open(path) {
            if let Ok(mut zip) = zip::ZipArchive::new(file) {
                for i in 0..zip.len() {
                    if let Ok(e) = zip.by_index(i) {
                        if e.name().starts_with(prefix) {
                            out.push(e.name().to_string());
                        }
                    }
                }
            }
        }
        out
    }

    fn zip_text(path: &Path, name: &str) -> String {
        let mut s = String::new();
        if let Ok(file) = std::fs::File::open(path) {
            if let Ok(mut zip) = zip::ZipArchive::new(file) {
                if let Ok(mut f) = zip.by_name(name) {
                    use std::io::Read;
                    let _ = f.read_to_string(&mut s);
                }
            }
        }
        s
    }

    #[test]
    fn sync_removes_and_reattaches_table_parts() {
        let dir = std::env::temp_dir().join("sheets_native_test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("table_sync_{}.xlsx", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut wb = crate::model::Workbook::new_blank();
        wb.sheets[0].set(0, 0, Cell { value: Value::Text("产品".into()), formula: None });
        wb.sheets[0].set(0, 1, Cell { value: Value::Text("数量".into()), formula: None });
        wb.sheets[0].set(1, 0, Cell { value: Value::Text("笔".into()), formula: None });
        wb.sheets[0].set(1, 1, Cell { value: Value::Number(3.0), formula: None });
        crate::xlsx::save(&path, &wb).unwrap();

        let t = Table {
            name: "表1".into(),
            display: "表1".into(),
            range: (0, 0, 1, 1),
            columns: vec!["产品".into(), "数量".into()],
            header_row: true,
            totals_row: false,
        };
        attach(&path, &wb.sheets[0].name, std::slice::from_ref(&t)).unwrap();
        assert!(!zip_has(&path, "xl/tables/table").is_empty(), "attach 后应有 table 部件");
        let sheet = &wb.sheets[0].name;
        let targets = crate::patch::sheet_targets(&path).unwrap();
        let part = targets.get(sheet).unwrap().clone();
        assert!(zip_text(&path, &part).contains("<tableParts"), "应有 tableParts");

        sync(&path, sheet, &[]).unwrap();
        assert!(zip_has(&path, "xl/tables/table").is_empty(), "转换后 table 部件应删除");
        assert!(!zip_text(&path, &part).contains("<tableParts"), "转换后不应有 tableParts");
        let rels = part.replace("worksheets/", "worksheets/_rels/") + ".rels";
        assert!(!zip_text(&path, &rels).contains("relationships/table"), "rels 里不应再有 table");
        assert!(!zip_text(&path, "[Content_Types].xml").contains("/xl/tables/"), "CT 里不应再有 table");

        // 再挂回去也要能用
        attach(&path, sheet, std::slice::from_ref(&t)).unwrap();
        assert!(!zip_has(&path, "xl/tables/table").is_empty());
        let back = crate::xlsx::load(&path).unwrap();
        assert_eq!(back.sheets[0].tables.len(), 1, "重新 attach 后能读回表格");
        assert_eq!(back.sheets[0].value(1, 1), Value::Number(3.0), "内容不能丢");
    }
}
