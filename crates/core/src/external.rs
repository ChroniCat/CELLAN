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

/// CSV 的持久化配置：编码与“按文本保留”的列（00123、18 位账号）。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CsvOpts {
    /// None = 自动（BOM → UTF-8 → GB18030）
    pub encoding: Option<crate::csv::Encoding>,
    /// 这些列一律按文本写入，不自动转数字
    pub text_cols: Vec<u32>,
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
    /// 仅 CSV 有效；老工作簿里没有这一段时按自动处理
    pub csv_opts: Option<CsvOpts>,
}

impl Source {
    /// 存进名称管理器的文本（不带等号，外面包一层双引号当字符串常量）
    ///
    /// CSV|路径|分隔符|表头|行|列|上次行数|上次列数
    /// SHEET|表名|r1,c1,r2,c2|行|列|上次行数|上次列数
    pub fn to_name_text(&self) -> String {
        match &self.kind {
            Kind::Csv { path, delim, header } => {
                let (enc, cols) = match &self.csv_opts {
                    Some(o) => (
                        o.encoding.map(|e| e.id()).unwrap_or("auto").to_string(),
                        o.text_cols.iter().map(|c| c.to_string()).collect::<Vec<_>>().join(","),
                    ),
                    None => ("auto".to_string(), String::new()),
                };
                format!(
                    "\"CSV|{}|{}|{}|{}|{}|{}|{}|{}|{}\"",
                    path.replace('"', "\"\""),
                    delim,
                    if *header { 1 } else { 0 },
                    self.at.0,
                    self.at.1,
                    self.rows,
                    self.cols,
                    enc,
                    cols
                )
            }
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
        let csv_opts = if matches!(kind, Kind::Csv { .. }) {
            let enc = f.get(8).and_then(|x| crate::csv::Encoding::from_id(x));
            let cols: Vec<u32> = f
                .get(9)
                .map(|x| x.split(',').filter_map(|c| c.trim().parse().ok()).collect())
                .unwrap_or_default();
            if enc.is_some() || !cols.is_empty() {
                Some(CsvOpts { encoding: enc, text_cols: cols })
            } else {
                None
            }
        } else {
            None
        };
        Some(Source {
            name: name.to_string(),
            kind,
            at: (num(4), num(5)),
            rows: num(6),
            cols: num(7),
            csv_opts,
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
///
/// 这是**合并**而不是重写：第三方属性（Customer 之类）的类型、fmtid、pid、值都要原样保留，
/// 只替换本应用自己拥有的 `_sc_src_*`。旧实现每次新造整份 XML，用户原有属性全被删掉。
pub fn write_sources(path: &std::path::Path, sources: &[Source]) -> anyhow::Result<()> {
    write_sources_from(path, sources, None)
}

const CP_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/custom-properties";
const VT_NS: &str = "http://schemas.openxmlformats.org/officeDocument/2006/docPropsVTypes";

/// 解析后的 `docProps/custom.xml` 结构：保留原有根标签与第三方属性的原始片段，
/// 只标记出应用自己的属性范围。
struct CustomPropsDoc {
    /// 根标签原文（`<Properties ...>` 或 `<cp:Properties ...>`）
    root_raw: String,
    /// 根结束标签原文（`</Properties>` / `</cp:Properties>`）
    root_close: String,
    /// 文档声明（含 `<?xml ...?>` 与其后空白）
    decl: String,
    /// 第三方属性原始片段
    kept: Vec<String>,
    /// 保留属性的最大 pid
    max_pid: u32,
    /// 本应用属性现有的 (name, pid)，替换时按名字复用 pid，连续保存不漂移
    owned: Vec<(String, u32)>,
    /// 属性元素用的前缀（"" 或 "cp:"）；同命名空间的元素前缀应当一致
    prop_prefix: String,
    /// VT 命名空间已绑定的前缀（"vt:" 或别的）；None 表示根上没声明
    vt_prefix: Option<String>,
}

/// 用 quick-xml 按**命名空间 + local name** 解析，而不是字符串找 `<property`：
/// 合法文件里可能是 `<cp:property>`、`<property pid='2'>`（单引号）、
/// 类型元素也可能写成 `<v:lpwstr>`。
fn parse_custom_props(xml: &str) -> Option<CustomPropsDoc> {
    use quick_xml::events::Event;
    use quick_xml::name::{Namespace, ResolveResult};

    let mut reader = quick_xml::NsReader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();

    let mut decl_end = 0usize;
    let mut root_raw: Option<String> = None;
    let mut root_close: Option<String> = None;
    let mut kept: Vec<String> = Vec::new();
    let mut max_pid = 1u32;
    let mut owned: Vec<(String, u32)> = Vec::new();
    let mut prop_prefix = String::new();
    let mut vt_prefix: Option<String> = None;
    let mut depth = 0usize;
    let mut prop_start: Option<(usize, String, bool)> = None; // (pos, raw head, owned)

    loop {
        let pos = reader.buffer_position() as usize;
        let (ns, ev) = reader.read_resolved_event_into(&mut buf).ok()?;
        match ev {
            Event::Decl(_) => decl_end = reader.buffer_position() as usize,
            Event::Start(ref e) => {
                let local = e.local_name();
                let name = local.as_ref();
                if depth == 0 {
                    if name != "Properties" || ns != ResolveResult::Bound(Namespace(CP_NS)) {
                        return None; // 根不是自定义属性文档
                    }
                    let end = reader.buffer_position() as usize;
                    let raw = &xml[pos..end];
                    // 记录命名空间声明
                    for a in e.attributes().with_checks(false).flatten() {
                        let key = a.key.as_ref();
                        if key.starts_with("xmlns:") {
                            let prefix = &key[6..];
                            if a.value.as_ref() == VT_NS {
                                vt_prefix = Some(format!("{prefix}:"));
                            }
                        }
                    }
                    root_raw = Some(raw.to_string());
                } else if depth == 1 && name == "property" && ns == ResolveResult::Bound(Namespace(CP_NS)) {
                    let mut pname = String::new();
                    let mut pid = 0u32;
                    for a in e.attributes().with_checks(false).flatten() {
                        let key = a.key.local_name();
                        let val = quick_xml::escape::unescape(a.value.as_ref()).map(|v| v.into_owned()).unwrap_or_default();
                        match key.as_ref() {
                            "name" => pname = val,
                            "pid" => pid = val.trim().parse().unwrap_or(0),
                            _ => {}
                        }
                    }
                    // 前缀沿用文件里的写法（`cp:` 或空），新建属性时保持一致
                    let qname = e.name().as_ref().to_string();
                    if let Some((pre, _)) = qname.split_once(':') {
                        if prop_prefix.is_empty() {
                            prop_prefix = format!("{pre}:");
                        }
                    }
                    max_pid = max_pid.max(pid);
                    let is_owned = pname.starts_with("_sc_src_");
                    if is_owned {
                        owned.push((pname.clone(), pid));
                    }
                    prop_start = Some((pos, qname, is_owned));
                }
                depth += 1;
            }
            Event::End(ref e) => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    let close_start = pos;
                    root_close = Some(xml[close_start..reader.buffer_position() as usize].to_string());
                    break;
                }
                if depth == 1 {
                    if let Some((start_pos, qname, owned)) = prop_start.take() {
                        let expect = format!("</{qname}>");
                        let end_pos = reader.buffer_position() as usize;
                        let raw = xml[start_pos..end_pos].to_string();
                        if !own_starts_with_close(&raw, &expect) {
                            return None;
                        }
                        if !owned {
                            kept.push(raw);
                        }
                    }
                }
                let _ = e;
            }
            Event::Empty(ref e) => {
                if depth == 1 && e.local_name().as_ref() == "property"
                    && ns == ResolveResult::Bound(Namespace(CP_NS))
                {
                    let mut pname = String::new();
                    let mut pid = 0u32;
                    for a in e.attributes().with_checks(false).flatten() {
                        let val = quick_xml::escape::unescape(a.value.as_ref()).map(|v| v.into_owned()).unwrap_or_default();
                        match a.key.local_name().as_ref() {
                            "name" => pname = val,
                            "pid" => pid = val.trim().parse().unwrap_or(0),
                            _ => {}
                        }
                    }
                    max_pid = max_pid.max(pid);
                    if pname.starts_with("_sc_src_") {
                        owned.push((pname, pid));
                    } else {
                        kept.push(xml[pos..reader.buffer_position() as usize].to_string());
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }

    let decl = xml[..decl_end].to_string();
    Some(CustomPropsDoc {
        root_raw: root_raw?,
        root_close: root_close.unwrap_or_else(|| "</Properties>".into()),
        decl,
        kept,
        max_pid,
        owned,
        prop_prefix,
        vt_prefix,
    })
}

fn own_starts_with_close(raw: &str, expect: &str) -> bool {
    raw.trim_end().ends_with(expect)
}

/// 把数据源写进 `docProps/custom.xml`（Excel 的自定义文档属性）。
///
/// 合并而不是重写：第三方属性连同它们用的前缀、类型元素、fmtid、pid 原样保留；
/// 只替换本应用拥有的 `_sc_src_*`。
pub fn write_sources_from(
    path: &std::path::Path,
    sources: &[Source],
    base: Option<&std::path::Path>,
) -> anyhow::Result<()> {
    let existing = base
        .and_then(|b| crate::fixups::read_part(b, "docProps/custom.xml"))
        .or_else(|| crate::fixups::read_part(path, "docProps/custom.xml"))
        .and_then(|b| String::from_utf8(b).ok());

    let parsed = existing.as_deref().and_then(parse_custom_props);
    let kept = parsed.as_ref().map(|d| d.kept.len()).unwrap_or(0);
    if kept == 0 && sources.is_empty() {
        remove_custom_part(path)?;
        return Ok(());
    }

    // 属性名前缀与 VT 前缀
    let prop_prefix = parsed.as_ref().map(|d| d.prop_prefix.clone()).unwrap_or_default();
    let vt_prefix = parsed
        .as_ref()
        .and_then(|d| d.vt_prefix.clone())
        .unwrap_or_else(|| "vt:".into());
    let mut root_raw = parsed
        .as_ref()
        .map(|d| d.root_raw.clone())
        .unwrap_or_else(|| format!("<Properties xmlns=\"{CP_NS}\" xmlns:vt=\"{VT_NS}\">"));
    let root_close = parsed
        .as_ref()
        .map(|d| d.root_close.clone())
        .unwrap_or_else(|| "</Properties>".into());
    let decl = parsed
        .as_ref()
        .map(|d| d.decl.clone())
        .filter(|d| d.contains("<?xml"))
        .unwrap_or_else(|| "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\r\n".into());

    // 根上没声明 VT 命名空间时补一条。判断必须用**解析出来的声明集合**：
    // 合法文件可能写成 xmlns:v='URI'（单引号），按字符串找双引号会误判成"没声明"，
    // 再补一条同名的 xmlns:v 就是重复属性，XML 直接非法。
    let vt_declared = match parsed.as_ref() {
        Some(d) => d.vt_prefix.is_some(),
        None => true, // 新建的根已经带 xmlns:vt
    };
    if !vt_declared
    {
        if let Some(i) = root_raw.rfind('>') {
            let ins = format!(" xmlns:{}=\"{}\"", vt_prefix.trim_end_matches(':'), VT_NS);
            root_raw.insert_str(i, &ins);
        }
    }

    let mut next_pid = parsed.as_ref().map(|d| d.max_pid).unwrap_or(1) + 1;
    let mut existing: Vec<(String, u32)> = parsed.as_ref().map(|d| d.owned.clone()).unwrap_or_default();
    let mut body = String::new();
    // 第三方属性原样搬过来（含它们自己的前缀）
    if let Some(d) = parsed.as_ref() {
        for raw in &d.kept {
            body.push_str(raw);
        }
    }
    let mut used: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (i, src) in sources.iter().enumerate() {
        // 归属只认 `_sc_src_` 前缀：名字必须落在这个命名空间里，否则下次保存会被当成第三方属性。
        let mut name = if src.name.starts_with("_sc_src_") {
            src.name.clone()
        } else {
            format!("_sc_src_{}", i + 1)
        };
        while !used.insert(name.clone()) {
            name = format!("_sc_src_{}_{}", i + 1, used.len() + 1);
        }
        let pid = match existing.iter().position(|(n, _)| *n == name) {
            Some(i) => existing.remove(i).1,
            None => {
                let p = next_pid;
                next_pid += 1;
                p
            }
        };
        let text = src.to_name_text().trim_matches('"').to_string();
        body.push_str(&format!(
            "<{p}property fmtid=\"{{D5CDD505-2E9C-101B-9397-08002B2CF9AE}}\" pid=\"{pid}\" name=\"{name}\"><{v}lpwstr>{text}</{v}lpwstr></{p}property>",
            p = prop_prefix,
            v = vt_prefix,
            pid = pid,
            name = xml_escape_attr(&name),
            text = xml_escape_text(&text)
        ));
    }
    let xml = format!("{decl}{root_raw}{body}{root_close}");

    let mut parts: Vec<(String, Vec<u8>)> = vec![("docProps/custom.xml".into(), xml.into_bytes())];
    // 关系与内容类型（已有就不重复加）
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

fn xml_escape_text(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

fn xml_escape_attr(s: &str) -> String {
    xml_escape_text(s).replace('"', "&quot;")
}

/// 清掉本应用写的自定义属性部件，以及只为它存在的关系/内容类型
fn remove_custom_part(path: &std::path::Path) -> anyhow::Result<()> {
    const PART: &str = "docProps/custom.xml";
    if crate::fixups::read_part(path, PART).is_none() {
        return Ok(());
    }
    crate::patch::remove_entry(path, PART)?;
    if let Some(rels) = crate::fixups::read_part(path, "_rels/.rels").and_then(|b| String::from_utf8(b).ok()) {
        let mut out = String::with_capacity(rels.len());
        let mut rest = rels.as_str();
        while let Some(i) = rest.find("<Relationship ") {
            out.push_str(&rest[..i]);
            let Some(gt) = rest[i..].find("/>") else { break };
            let rel = &rest[i..i + gt + 2];
            if !(rel.contains("custom-properties") && rel.contains(PART)) {
                out.push_str(rel);
            }
            rest = &rest[i + gt + 2..];
        }
        out.push_str(rest);
        if out != rels {
            crate::fixups::write_parts(path, &[("_rels/.rels".into(), out.into_bytes())])?;
        }
    }
    if let Some(ct) = crate::fixups::read_part(path, "[Content_Types].xml").and_then(|b| String::from_utf8(b).ok()) {
        let needle = "<Override PartName=\"/docProps/custom.xml\"";
        if let Some(i) = ct.find(needle) {
            if let Some(end) = ct[i..].find("/>") {
                let mut out = ct[..i].to_string();
                out.push_str(&ct[i + end + 2..]);
                crate::fixups::write_parts(path, &[("[Content_Types].xml".into(), out.into_bytes())])?;
            }
        }
    }
    Ok(())
}

/// 从 `docProps/custom.xml` 里读数据源
fn read_sources_from_props(path: &std::path::Path) -> Vec<Source> {
    let Some(xml) = crate::fixups::read_part_file(path, "docProps/custom.xml").and_then(|b| String::from_utf8(b).ok())
    else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (name, text) in read_all_props(&xml) {
        if name.starts_with("_sc_src_") {
            if let Some(src) = Source::from_name_text(&name, &text) {
                out.push(src);
            }
        }
    }
    out
}

/// 按命名空间解析所有自定义属性 → (name, 文本值)。
/// 只认 CP 命名空间里的 `property`：前缀可以是 `cp:`，属性引号可以是单引号。
fn read_all_props(xml: &str) -> Vec<(String, String)> {
    use quick_xml::events::Event;
    use quick_xml::name::{Namespace, ResolveResult};

    let mut reader = quick_xml::NsReader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut buf = Vec::new();
    let mut out = Vec::new();
    let mut cur: Option<(String, String)> = None;
    let mut in_value = false;
    loop {
        let (ns, ev) = match reader.read_resolved_event_into(&mut buf) {
            Ok(x) => x,
            Err(_) => break,
        };
        match ev {
            Event::Start(e) => {
                let local = e.local_name();
                if local.as_ref() == "property" && ns == ResolveResult::Bound(Namespace(CP_NS)) {
                    let mut name = String::new();
                    for a in e.attributes().with_checks(false).flatten() {
                        if a.key.local_name().as_ref() == "name" {
                            name = quick_xml::escape::unescape(a.value.as_ref()).map(|v| v.into_owned()).unwrap_or_default();
                        }
                    }
                    cur = Some((name, String::new()));
                } else if cur.is_some() {
                    in_value = true;
                }
            }
            Event::Text(t) if in_value => {
                if let Some((_, v)) = cur.as_mut() {
                    v.push_str(&t.xml10_content());
                }
            }
            Event::End(e) => {
                let local = e.local_name();
                if local.as_ref() == "property" && ns == ResolveResult::Bound(Namespace(CP_NS)) {
                    if let Some(p) = cur.take() {
                        out.push(p);
                    }
                }
                in_value = false;
            }
            Event::Eof => break,
            _ => {}
        }
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
    import_csv_with(sheet, path, delim, header, None, &[], (0, 0), at)
}

/// 与首次导入共用的实现：解码（BOM/UTF-16/GB18030）→ 记录解析（引号内换行不拆行）
/// → 按列类型转值 → 一次性写目标区域。
///
/// `old` 是上次刷新的行列数：新数据更小时要把多出来的旧格清掉，
/// 但只清"上一次自己写的区域"，不碰旁边的用户数据。
pub fn import_csv_with(
    sheet: &mut Sheet,
    path: &str,
    delim: char,
    header: bool,
    encoding: Option<crate::csv::Encoding>,
    text_cols: &[u32],
    old: (u32, u32),
    at: (u32, u32),
) -> std::io::Result<(u32, u32)> {
    let bytes = std::fs::read(path)?;
    let (text, _enc, had_errors) = crate::csv::decode(&bytes, encoding);
    if had_errors {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "CSV 编码无法完整解码（文件可能不是声明的编码）",
        ));
    }
    let records = crate::csv::records(&text, delim);
    let mut grid: Vec<Vec<Cell>> = Vec::with_capacity(records.len());
    for (ri, rec) in records.iter().enumerate() {
        let mut row = Vec::with_capacity(rec.len());
        for (ci, f) in rec.iter().enumerate() {
            let cell = if header && ri == 0 {
                Cell { value: Value::Text(f.as_str().into()), formula: None }
            } else if text_cols.contains(&(ci as u32)) {
                if f.is_empty() {
                    Cell::default()
                } else {
                    Cell { value: Value::Text(f.as_str().into()), formula: None }
                }
            } else {
                parse_field(f)
            };
            row.push(cell);
        }
        grid.push(row);
    }
    let rows = grid.len() as u32;
    let cols = grid.iter().map(|r| r.len() as u32).max().unwrap_or(0);
    // 旧区域里超出新数据的部分要清掉（只清自己上次写的那块）
    for r in 0..old.0 {
        for c in 0..old.1 {
            if r >= rows || c >= cols {
                sheet.set(at.0 + r, at.1 + c, Cell::default());
            }
        }
    }
    for (ri, row) in grid.into_iter().enumerate() {
        for (ci, cell) in row.into_iter().enumerate() {
            if !matches!(cell.value, Value::Empty) {
                sheet.set(at.0 + ri as u32, at.1 + ci as u32, cell);
            } else if (ri as u32) < old.0 && (ci as u32) < old.1 {
                sheet.set(at.0 + ri as u32, at.1 + ci as u32, Cell::default());
            }
        }
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
            let (enc, cols) = match &src.csv_opts {
                Some(o) => (o.encoding, o.text_cols.as_slice()),
                None => (None, &[][..]),
            };
            import_csv_with(
                &mut wb.sheets[sheet_idx],
                &real.to_string_lossy(),
                *delim,
                *header,
                enc,
                cols,
                (src.rows, src.cols),
                src.at,
            )
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
