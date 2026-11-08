//! 外部数据源：导入 CSV/TSV、从另一张表取数，并且可以刷新。
//!
//! 这不是真正的 Power Query —— 没有 M 语言、没有查询折叠、没有多表连接。
//! 它解决的是最常见的两件事：把网盘上的 CSV 拉进表里、把另一张表的区间
//! 同步过来，而且改完源文件后能一键刷新。
//!
//! 数据源信息存在工作簿的自定义名称里（`_sc_src_1` …），文本是一个字符串常量，
//! Excel 打开时只会看到几个名字，不影响任何计算。

use crate::model::{Cell, Sheet, Value};

#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    /// 本地 CSV/TSV：路径、分隔符、首行是否表头
    Csv { path: String, delim: char, header: bool },
    /// 同一工作簿里的另一张表：表名 + 区间
    Internal { sheet: String, rect: (u32, u32, u32, u32) },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Source {
    pub name: String,
    pub kind: Kind,
    /// 落到哪张表的哪个位置
    pub at: (u32, u32),
    /// 上次刷新写出的行列数
    pub rows: u32,
    pub cols: u32,
}

impl Source {
    /// 存进名称管理器的文本（不带等号，外面包一层双引号当字符串常量）
    ///
    /// CSV|路径|分隔符|表头|行|列|上次行数|上次列数
    /// SHEET|表名|r1,c1,r2,c2|行|列|上次行数|上次列数
    pub fn to_name_text(&self) -> String {
        match &self.kind {
            Kind::Csv { path, delim, header } => format!(
                "\"CSV|{}|{}|{}|{}|{}|{}|{}\"",
                path.replace('"', "\"\""),
                delim,
                if *header { 1 } else { 0 },
                self.at.0,
                self.at.1,
                self.rows,
                self.cols
            ),
            Kind::Internal { sheet, rect } => format!(
                "\"SHEET|{}|{},{},{},{}|{}|{}|{}|{}\"",
                sheet.replace('"', "\"\""),
                rect.0, rect.1, rect.2, rect.3,
                self.at.0, self.at.1, self.rows, self.cols
            ),
        }
    }

    /// 从名称文本还原；不认识的返回 None（别的软件写的名字一律忽略）
    pub fn from_name_text(name: &str, text: &str) -> Option<Source> {
        // 名称文本是从 XML 里读出来的，实体要先还原（" 会写成 &quot;）
        let text = text
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&");
        let body = text.trim().trim_start_matches('"').trim_end_matches('"');
        let f: Vec<&str> = body.split('|').collect();
        let num = |i: usize| -> u32 { f.get(i).and_then(|x| x.parse().ok()).unwrap_or(0) };
        let kind = match f.first().copied()? {
            "CSV" if f.len() >= 6 => Kind::Csv {
                path: f[1].replace("\"\"", "\""),
                delim: f[2].chars().next().unwrap_or(','),
                header: f[3] != "0",
            },
            "SHEET" if f.len() >= 6 => {
                let r: Vec<u32> = f[2].split(',').filter_map(|x| x.parse().ok()).collect();
                if r.len() != 4 {
                    return None;
                }
                Kind::Internal {
                    sheet: f[1].replace("\"\"", "\""),
                    rect: (r[0], r[1], r[2], r[3]),
                }
            }
            _ => return None,
        };
        Some(Source {
            name: name.to_string(),
            kind,
            at: (num(4), num(5)),
            rows: num(6),
            cols: num(7),
        })
    }
}

/// 把存的路径解析成真实路径：
/// - 绝对路径且存在 → 直接用
/// - 否则按工作簿所在目录解析（换机器、换盘符也能找到同一个盘上的文件）
pub fn resolve_path(wb: &crate::model::Workbook, stored: &str) -> std::path::PathBuf {
    let p = std::path::Path::new(stored);
    if p.is_absolute() && p.exists() {
        return p.to_path_buf();
    }
    if let Some(dir) = wb.path.as_ref().and_then(|x| x.parent()) {
        let joined = if p.is_absolute() {
            // 绝对路径失效了：退而求其次，按文件名在同一个目录下找
            match p.file_name() {
                Some(f) => dir.join(f),
                None => dir.join(stored),
            }
        } else {
            dir.join(p)
        };
        if joined.exists() {
            return joined;
        }
        return joined;
    }
    p.to_path_buf()
}

/// 能不能存成相对路径（和文件放一起时就存相对的，换机器还能用）
pub fn store_path(wb: &crate::model::Workbook, path: &std::path::Path) -> String {
    if let (Some(dir), Ok(rel)) = (
        wb.path.as_ref().and_then(|x| x.parent()),
        path.strip_prefix(
            wb.path
                .as_ref()
                .and_then(|x| x.parent())
                .unwrap_or_else(|| std::path::Path::new(".")),
        ),
    ) {
        let _ = dir;
        return rel.to_string_lossy().replace('\\', "/");
    }
    path.to_string_lossy().to_string()
}


/// 从包里读数据源：优先新部件 `xl/sheetcore/sources.xml`，
/// 老文件里存在 definedNames 的 `_sc_src_*` 也认。
pub fn read_sources(path: &std::path::Path) -> Vec<Source> {
    let props = read_sources_from_props(path);
    if !props.is_empty() {
        return props;
    }
    let mut out = Vec::new();
    if let Some(xml) = crate::fixups::read_part_file(path, "xl/sheetcore/sources.xml")
        .and_then(|b| String::from_utf8(b).ok())
    {
        let mut rest = xml.as_str();
        while let Some(i) = rest.find("<source ") {
            rest = &rest[i..];
            let Some(gt) = rest.find('>') else { break };
            let head = &rest[..gt];
            let name = head
                .find("name=\"")
                .map(|p| {
                    let after = &head[p + 6..];
                    after.split('"').next().unwrap_or("").to_string()
                })
                .unwrap_or_default();
            let Some(close) = rest.find("</source>") else { break };
            let text = &rest[gt + 1..close];
            if let Some(src) = Source::from_name_text(&name, text) {
                out.push(src);
            }
            rest = &rest[close + 9..];
        }
    }
    if out.is_empty() {
        for (n, _, t) in crate::patch::read_defined_names(path) {
            if n.starts_with("_sc_src_") {
                if let Some(src) = Source::from_name_text(&n, &t) {
                    out.push(src);
                }
            }
        }
    }
    out
}


/// 把数据源写进 `docProps/custom.xml`（Excel 的自定义文档属性）。
pub fn write_sources(path: &std::path::Path, sources: &[Source]) -> anyhow::Result<()> {
    let mut body = String::new();
    for (i, src) in sources.iter().enumerate() {
        let name = if src.name.is_empty() { format!("_sc_src_{}", i + 1) } else { src.name.clone() };
        let text = src.to_name_text().trim_matches('"').replace('&', "&amp;").replace('<', "&lt;");
        body.push_str(&format!(
            "<property fmtid=\"{{D5CDD505-2E9C-101B-9397-08002B2CF9AE}}\" pid=\"{}\" name=\"{}\"><vt:lpwstr>{}</vt:lpwstr></property>",
            i + 2,
            name,
            text
        ));
    }
    let xml = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n\
<Properties xmlns=\"http://schemas.openxmlformats.org/officeDocument/2006/custom-properties\" \
xmlns:vt=\"http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes\">{body}</Properties>"
    );

    let mut parts: Vec<(String, Vec<u8>)> = vec![("docProps/custom.xml".into(), xml.into_bytes())];

    // 关系：包级 _rels/.rels 要指向它
    let rels = crate::fixups::read_part(path, "_rels/.rels")
        .and_then(|b| String::from_utf8(b).ok())
        .unwrap_or_default();
    if !rels.contains("custom-properties") && !rels.is_empty() {
        let max_id = rels
            .match_indices("Id=\"rId")
            .filter_map(|(i, _)| {
                let after = &rels[i + 7..];
                after.chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse::<u32>().ok()
            })
            .max()
            .unwrap_or(0);
        let rel = format!(
            "<Relationship Id=\"rId{}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/custom-properties\" Target=\"docProps/custom.xml\"/>",
            max_id + 1
        );
        if let Some(i) = rels.rfind("</Relationships>") {
            parts.push(("_rels/.rels".into(), format!("{}{}{}", &rels[..i], rel, &rels[i..]).into_bytes()));
        }
    }

    // 内容类型
    if let Some(ct) = crate::fixups::read_part(path, "[Content_Types].xml").and_then(|b| String::from_utf8(b).ok()) {
        if !ct.contains("/docProps/custom.xml") {
            if let Some(i) = ct.rfind("</Types>") {
                let mut out = ct.clone();
                out.insert_str(
                    i,
                    "<Override PartName=\"/docProps/custom.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.custom-properties+xml\"/>",
                );
                parts.push(("[Content_Types].xml".into(), out.into_bytes()));
            }
        }
    }
    crate::fixups::write_parts(path, &parts)?;
    Ok(())
}

/// 从 `docProps/custom.xml` 里读数据源
fn read_sources_from_props(path: &std::path::Path) -> Vec<Source> {
    let Some(xml) = crate::fixups::read_part_file(path, "docProps/custom.xml").and_then(|b| String::from_utf8(b).ok())
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut rest = xml.as_str();
    while let Some(i) = rest.find("<property ") {
        rest = &rest[i..];
        let Some(gt) = rest.find('>') else { break };
        let head = &rest[..gt];
        let name = head
            .find("name=\"")
            .map(|p| {
                let after = &head[p + 6..];
                after.split('"').next().unwrap_or("").to_string()
            })
            .unwrap_or_default();
        let Some(close) = rest.find("</property>") else { break };
        let body = &rest[gt + 1..close];
        if name.starts_with("_sc_src_") {
            let text = body
                .trim_start_matches("<vt:lpwstr>")
                .trim_end_matches("</vt:lpwstr>")
                .replace("&amp;", "&")
                .replace("&lt;", "<");
            if let Some(src) = Source::from_name_text(&name, &text) {
                out.push(src);
            }
        }
        rest = &rest[close + 11..];
    }
    out
}

/// 每行按分隔符切分，支持双引号包裹（`""` 是转义的双引号）
pub fn split_line(line: &str, delim: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    cur.push('"');
                    chars.next();
                } else {
                    quoted = false;
                }
            } else {
                cur.push(c);
            }
        } else if c == '"' {
            quoted = true;
        } else if c == delim {
            out.push(std::mem::take(&mut cur));
        } else if c != '\r' {
            cur.push(c);
        }
    }
    out.push(cur);
    out
}

/// 读 CSV：返回 (行, 列) 数量。写进 sheet 的 at 起点，超出部分不写。
pub fn import_csv(
    sheet: &mut Sheet,
    path: &str,
    delim: char,
    header: bool,
    at: (u32, u32),
) -> std::io::Result<(u32, u32)> {
    let text = std::fs::read_to_string(path)?;
    let mut rows = 0u32;
    let mut cols = 0u32;
    for (ri, line) in text.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        let fields = split_line(line, delim);
        cols = cols.max(fields.len() as u32);
        for (ci, f) in fields.iter().enumerate() {
            let cell = if header && ri == 0 {
                Cell { value: Value::Text(f.as_str().into()), formula: None }
            } else {
                parse_field(f)
            };
            sheet.set(at.0 + ri as u32, at.1 + ci as u32, cell);
        }
        rows = ri as u32 + 1;
    }
    Ok((rows, cols))
}

/// 数字就当数字，空就是空，别的原样当文本
fn parse_field(f: &str) -> Cell {
    let t = f.trim();
    if t.is_empty() {
        return Cell::default();
    }
    if let Ok(n) = t.parse::<f64>() {
        if t.chars().all(|c| c.is_ascii_digit() || "+-.eE".contains(c)) {
            return Cell { value: Value::Number(n), formula: None };
        }
    }
    Cell { value: Value::Text(t.into()), formula: None }
}

/// 从另一张表取一段，原样贴到目标位置
pub fn copy_from_sheet(
    src: &Sheet,
    rect: (u32, u32, u32, u32),
    dst: &mut Sheet,
    at: (u32, u32),
) -> (u32, u32) {
    let (r1, c1, r2, c2) = rect;
    let mut rows = 0;
    let mut cols = 0;
    // 表名带空格或特殊字符时要加单引号，跟 Excel 一致
    let quoted = if src.name.chars().any(|c| !c.is_alphanumeric() && c != '_') {
        format!("'{}'", src.name.replace('\'', "''"))
    } else {
        src.name.clone()
    };
    for r in r1..=r2 {
        for c in c1..=c2 {
            let formula = Some(format!("={quoted}!{}", crate::model::cell_name(r, c)).into());
            let value = src.get(r, c).map(|x| x.value.clone()).unwrap_or_default();
            dst.set(
                at.0 + (r - r1),
                at.1 + (c - c1),
                Cell { value: value.clone(), formula },
            );
            cols = cols.max(c - c1 + 1);
        }
        rows = rows.max(r - r1 + 1);
    }
    (rows, cols)
}

/// 刷新一个数据源；返回写出的 (行, 列)
pub fn refresh(wb: &mut crate::model::Workbook, sheet_idx: usize, src: &Source) -> std::io::Result<(u32, u32)> {
    match &src.kind {
        Kind::Csv { path, delim, header } => {
            let real = resolve_path(wb, path);
            import_csv(&mut wb.sheets[sheet_idx], &real.to_string_lossy(), *delim, *header, src.at)
        }
        Kind::Internal { sheet, rect } => {
            let Some(si) = wb.sheets.iter().position(|s| &s.name == sheet) else {
                return Err(std::io::Error::new(std::io::ErrorKind::NotFound, "找不到来源表"));
            };
            let (lo, hi) = (si.min(sheet_idx), si.max(sheet_idx));
            let (left, right) = wb.sheets.split_at_mut(hi);
            let copied = if si == lo {
                copy_from_sheet(&left[lo], *rect, &mut right[0], src.at)
            } else {
                copy_from_sheet(&right[0], *rect, &mut left[lo], src.at)
            };
            Ok(copied)
        }
    }
}
