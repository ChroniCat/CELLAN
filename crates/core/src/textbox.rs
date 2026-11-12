//! 文本框（drawing 里的 `<xdr:sp>` 且 `cNvSpPr txBox="1"`）。
//!
//! Excel 把文本框存成 drawing 部件里的一个锚点 + 形状：
//! `twoCellAnchor(from/to 各 4 个数) → xdr:sp → spPr(填充/边框) + txBody(文字/字号)`。
//! 这里按 `twoCellAnchor` 原样保存（EMU 偏移），渲染时再用列宽行高换算成像素，
//! 因此「保存 → Excel 打开 → 再改」不会因为我们只记整格而漂移。

use crate::layout::Axis;
use std::path::Path;

/// 1 像素 = 9525 EMU（96 DPI）
pub const EMU_PER_PX: f64 = 9525.0;

pub fn px_to_emu(v: f32) -> i64 {
    (v as f64 * EMU_PER_PX).round() as i64
}
pub fn emu_to_px(v: i64) -> f32 {
    (v as f64 / EMU_PER_PX) as f32
}

/// 千分之一磅（`sz="1800"` = 18 磅）
pub fn pt_to_sz(pt: f32) -> i64 {
    (pt * 100.0).round() as i64
}
pub fn sz_to_pt(sz: i64) -> f32 {
    sz as f32 / 100.0
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextBox {
    /// cNvPr 的 id（写回时保持稳定，Excel 用它做对象标识）
    pub id: u32,
    /// 显示名（`文本框 1`）
    pub name: String,
    /// 左上锚点：(列, 列内偏移 EMU, 行, 行内偏移 EMU)
    pub from: (u32, i64, u32, i64),
    /// 右下锚点（同上）
    pub to: (u32, i64, u32, i64),
    pub text: String,
    /// 字号（磅）
    pub size: f32,
    pub bold: bool,
    pub italic: bool,
    pub color: [u8; 3],
    /// 填充色；None = 无填充（透明）
    pub fill: Option<[u8; 3]>,
    /// 边框色；None = 无边框
    pub line: Option<[u8; 3]>,
    /// 0 左 / 1 中 / 2 右
    pub align: u8,
    /// 自动换行（bodyPr wrap="square"）
    pub wrap: bool,
}

impl Default for TextBox {
    fn default() -> Self {
        Self {
            id: 2,
            name: "文本框 1".into(),
            from: (0, 0, 0, 0),
            to: (2, 0, 2, 0),
            text: String::new(),
            size: 11.0,
            bold: false,
            italic: false,
            color: [0, 0, 0],
            fill: None,
            line: Some([0x7F, 0x7F, 0x7F]),
            align: 0,
            wrap: true,
        }
    }
}

impl TextBox {
    /// 屏幕像素矩形 (x, y, w, h)
    pub fn rect_px(&self, cols: &Axis, rows: &Axis) -> (f32, f32, f32, f32) {
        let x = cols.pos(self.from.0) + emu_to_px(self.from.1);
        let y = rows.pos(self.from.2) + emu_to_px(self.from.3);
        let x2 = cols.pos(self.to.0) + emu_to_px(self.to.1);
        let y2 = rows.pos(self.to.2) + emu_to_px(self.to.3);
        (x, y, (x2 - x).max(8.0), (y2 - y).max(8.0))
    }

    /// 用像素矩形反推锚点（拖拽/缩放后调用）。宽高小于 16×12 时夹住。
    pub fn set_rect_px(&mut self, cols: &Axis, rows: &Axis, x: f32, y: f32, w: f32, h: f32) {
        let (x, y) = (x.max(0.0), y.max(0.0));
        let (w, h) = (w.max(16.0), h.max(12.0));
        self.from = anchor_of(cols, rows, x, y);
        self.to = anchor_of(cols, rows, x + w, y + h);
        // 自相交的锚点在 Excel 里会显示错位，夹一下
        if (self.to.0, self.to.2) < (self.from.0, self.from.2) {
            self.to.0 = self.from.0;
            self.to.2 = self.from.2;
        }
    }
}

/// 像素点 → (列, 列内 EMU, 行, 行内 EMU)
fn anchor_of(cols: &Axis, rows: &Axis, x: f32, y: f32) -> (u32, i64, u32, i64) {
    let c = cols.index_at(x);
    let r = rows.index_at(y);
    let co = (x - cols.pos(c)).max(0.0);
    let ro = (y - rows.pos(r)).max(0.0);
    (c, px_to_emu(co), r, px_to_emu(ro))
}

// ---------------------------------------------------------------- 读

fn attr(tag: &str, key: &str) -> Option<String> {
    let pat = format!("{key}=\"");
    let i = tag.find(&pat)? + pat.len();
    let rest = &tag[i..];
    let q = rest.find('"')?;
    Some(crate::model::xml_unescape(&rest[..q]))
}

fn child_text(xml: &str, key: &str) -> Option<String> {
    for prefix in ["xdr:", ""] {
        let open = format!("<{prefix}{key}>");
        let close = format!("</{prefix}{key}>");
        if let Some(i) = xml.find(&open) {
            let start = i + open.len();
            let rest = &xml[start..];
            if let Some(j) = rest.find(&close) {
                return Some(crate::model::xml_unescape(rest[..j].trim()));
            }
        }
    }
    None
}

fn child_i64(xml: &str, key: &str) -> Option<i64> {
    child_text(xml, key).and_then(|v| v.trim().parse().ok())
}

/// 一个 anchor 里的 from/to
fn anchor_pair(chunk: &str) -> Option<((u32, i64, u32, i64), (u32, i64, u32, i64))> {
    let seg = |which: &str| -> Option<(u32, i64, u32, i64)> {
        // 找 <xdr:from>…</xdr:from>（也认不带前缀）
        let mut body: Option<&str> = None;
        for prefix in ["xdr:", ""] {
            let open = format!("<{prefix}{which}>");
            if let Some(i) = chunk.find(&open) {
                let start = i + open.len();
                let rest = &chunk[start..];
                let close = format!("</{prefix}{which}>");
                let end = rest.find(&close).unwrap_or(rest.len());
                body = Some(&rest[..end]);
                break;
            }
        }
        let body = body?;
        Some((
            child_i64(body, "col")? as u32,
            child_i64(body, "colOff").unwrap_or(0),
            child_i64(body, "row")? as u32,
            child_i64(body, "rowOff").unwrap_or(0),
        ))
    };
    Some((seg("from")?, seg("to")?))
}

/// 从 drawing XML 里挑出文本框（`<xdr:sp>`，排除图片与图表）
pub fn parse_drawing(xml: &str) -> Vec<TextBox> {
    let mut out = Vec::new();
    for tag in ["twoCellAnchor", "oneCellAnchor"] {
        for prefix in ["xdr:", ""] {
            let open = format!("<{prefix}{tag}");
            let close = format!("</{prefix}{tag}>");
            let mut from = 0usize;
            while let Some(rel) = xml[from..].find(&open) {
                let start = from + rel;
                let end = xml[start..]
                    .find(&close)
                    .map(|e| start + e + close.len())
                    .unwrap_or(xml.len());
                let chunk = &xml[start..end];
                from = end;
                if !(chunk.contains("<xdr:sp") || chunk.contains("<sp ")) {
                    continue;
                }
                // 图表是 graphicFrame，图片是 xdr:pic；这里只认形状
                if chunk.contains("<xdr:pic") || chunk.contains("<xdr:graphicFrame") {
                    continue;
                }
                let Some((f, t)) = anchor_pair(chunk) else {
                    continue;
                };
                let mut tb = TextBox {
                    id: attr(chunk, "id")
                        .and_then(|v| v.parse().ok())
                        .unwrap_or(2),
                    name: attr(chunk, "name").unwrap_or_else(|| "文本框".into()),
                    from: f,
                    to: t,
                    ..Default::default()
                };
                // 文字：拼接所有 a:t（段落之间换行）
                let mut lines: Vec<String> = Vec::new();
                let mut p = 0usize;
                while let Some(i) = chunk[p..].find("<a:p>").or_else(|| chunk[p..].find("<a:p ")) {
                    let i = p + i;
                    let Some(gt) = chunk[i..].find('>').map(|e| i + e) else {
                        break;
                    };
                    let Some(close) = chunk[gt..].find("</a:p>").map(|e| gt + e) else {
                        break;
                    };
                    let seg = &chunk[gt..close];
                    p = close + 1;
                    let mut txt = String::new();
                    let mut q = 0usize;
                    while let Some(j) = seg[q..].find("<a:t>") {
                        let j = q + j + "<a:t>".len();
                        let Some(k) = seg[j..].find("</a:t>") else { break };
                        txt.push_str(&crate::model::xml_unescape(&seg[j..j + k]));
                        q = j + k;
                    }
                    if !txt.is_empty() {
                        lines.push(txt);
                    }
                }
                tb.text = lines.join("\n");
                // 字号/粗斜体：取第一个 run 的属性
                if let Some(rpr) = find_tag(chunk, "a:rPr") {
                    if let Some(sz) = attr(&rpr, "sz").and_then(|v| v.parse::<i64>().ok()) {
                        tb.size = sz_to_pt(sz);
                    }
                    tb.bold = matches!(attr(&rpr, "b").as_deref(), Some("1") | Some("true"));
                    tb.italic = matches!(attr(&rpr, "i").as_deref(), Some("1") | Some("true"));
                }
                // 颜色：rPr 里的 solidFill；没有再找 pPr 的
                if let Some(c) = solid_fill_in(chunk) {
                    tb.color = c;
                }
                // 填充 / 边框：spPr 里的 solidFill 与 a:ln
                let sp_pr = find_block(chunk, "xdr:spPr").or_else(|| find_block(chunk, "spPr"));
                if let Some(sp) = sp_pr {
                    // spPr 里第一个 solidFill 是形状填充；边框颜色在 <a:ln> 里，先摘出去
                    let ln = find_block(&sp, "a:ln");
                    let fill_part = strip_block(&sp, "a:ln");
                    tb.fill = first_solid_fill(&fill_part, None);
                    tb.line = ln.as_deref().and_then(|l| first_solid_fill(l, None));
                }
                // 对齐：pPr algn
                if let Some(ppr) = find_tag(chunk, "a:pPr") {
                    tb.align = match attr(&ppr, "algn").as_deref() {
                        Some("ctr") => 1,
                        Some("r") => 2,
                        _ => 0,
                    };
                }
                // 自动换行
                if let Some(bp) = find_tag(chunk, "a:bodyPr") {
                    tb.wrap = attr(&bp, "wrap").as_deref() != Some("none");
                }
                out.push(tb);
            }
        }
    }
    out
}

fn find_tag<'a>(xml: &'a str, tag: &str) -> Option<String> {
    let pat = format!("<{tag}");
    let i = xml.find(&pat)?;
    let gt = xml[i..].find('>').map(|e| i + e)?;
    Some(xml[i..=gt].to_string())
}

/// 找 `<tag>…</tag>` 的内容（不含标签）；自闭合返回空串
fn find_block(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}");
    let i = xml.find(&open)?;
    let gt = xml[i..].find('>').map(|e| i + e)?;
    if xml[i..=gt].ends_with("/>") {
        return Some(String::new());
    }
    let close = format!("</{tag}>");
    let rest = &xml[gt..];
    let j = rest.find(&close)?;
    Some(rest[..j].to_string())
}

/// 找 `<a:srgbClr val="RRGGBB"/>`
fn srgb(tag: &str) -> Option<[u8; 3]> {
    let i = tag.find("srgbClr")?;
    let rest = &tag[i..];
    let v = attr(rest, "val")?;
    let hex = v.trim();
    if hex.len() < 6 {
        return None;
    }
    let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
    let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
    let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
    Some([r, g, b])
}

/// 去掉 `<tag>…</tag>` 整段
fn strip_block(xml: &str, tag: &str) -> String {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let mut x = xml.to_string();
    while let Some(i) = x.find(&open) {
        let Some(gt) = x[i..].find('>').map(|e| i + e) else { break };
        if x[i..=gt].ends_with("/>") {
            x.replace_range(i..=gt, "");
            continue;
        }
        let Some(j) = x[gt..].find(&close).map(|e| gt + e + close.len()) else { break };
        x.replace_range(i..j, "");
    }
    x
}

/// 第一个 solidFill 的颜色，跳过 `skip`（边框区）
fn first_solid_fill(xml: &str, skip: Option<(usize, usize)>) -> Option<[u8; 3]> {
    let mut from = 0usize;
    while let Some(i) = xml[from..].find("<a:solidFill>") {
        let i = from + i;
        let end = xml[i..]
            .find("</a:solidFill>")
            .map(|e| i + e)
            .unwrap_or(xml.len());
        let inside = &xml[i..end];
        from = end.max(i + 1);
        if let Some((p, l)) = skip {
            let here = xml.as_ptr() as usize + i;
            if here >= p && here < p + l {
                continue;
            }
        }
        if let Some(c) = srgb(inside) {
            return Some(c);
        }
    }
    None
}

fn solid_fill_in(xml: &str) -> Option<[u8; 3]> {
    let rpr = find_block(xml, "a:rPr")?;
    first_solid_fill(&rpr, None)
}

// ---------------------------------------------------------------- 写

/// 一个文本框 → `<xdr:twoCellAnchor>` 片段
pub fn anchor_xml(tb: &TextBox) -> String {
    let algn = match tb.align {
        1 => "ctr",
        2 => "r",
        _ => "l",
    };
    let color = format!(
        "{:02X}{:02X}{:02X}",
        tb.color[0], tb.color[1], tb.color[2]
    );
    let mut sp_pr = String::from(
        "<a:prstGeom prst=\"rect\"><a:avLst/></a:prstGeom>",
    );
    if let Some(f) = tb.fill {
        sp_pr.push_str(&format!(
            "<a:solidFill><a:srgbClr val=\"{:02X}{:02X}{:02X}\"/></a:solidFill>",
            f[0], f[1], f[2]
        ));
    } else {
        sp_pr.push_str("<a:noFill/>");
    }
    match tb.line {
        Some(l) => sp_pr.push_str(&format!(
            "<a:ln w=\"9525\"><a:solidFill><a:srgbClr val=\"{:02X}{:02X}{:02X}\"/></a:solidFill></a:ln>",
            l[0], l[1], l[2]
        )),
        None => sp_pr.push_str("<a:ln><a:noFill/></a:ln>"),
    }
    // 多段文字 → 多个 <a:p>；空文本也要留一个空段落，否则 Excel 认为形状没有正文
    let paras: Vec<&str> = if tb.text.is_empty() {
        vec![""]
    } else {
        tb.text.split('\n').collect()
    };
    let mut body = String::new();
    for p in paras {
        body.push_str(&format!("<a:p><a:pPr algn=\"{algn}\"/>"));
        if !p.is_empty() {
            body.push_str(&format!(
                "<a:r><a:rPr lang=\"zh-CN\" sz=\"{}\"{}{}>\
                 <a:solidFill><a:srgbClr val=\"{color}\"/></a:solidFill>\
                 <a:latin typeface=\"等线\"/></a:rPr><a:t>{}</a:t></a:r>",
                pt_to_sz(tb.size),
                if tb.bold { " b=\"1\"" } else { "" },
                if tb.italic { " i=\"1\"" } else { "" },
                esc(p)
            ));
        }
        body.push_str("</a:p>");
    }
    format!(
        "<xdr:twoCellAnchor editAs=\"oneCell\">\
<xdr:from><xdr:col>{}</xdr:col><xdr:colOff>{}</xdr:colOff><xdr:row>{}</xdr:row><xdr:rowOff>{}</xdr:rowOff></xdr:from>\
<xdr:to><xdr:col>{}</xdr:col><xdr:colOff>{}</xdr:colOff><xdr:row>{}</xdr:row><xdr:rowOff>{}</xdr:rowOff></xdr:to>\
<xdr:sp macro=\"\" textlink=\"\">\
<xdr:nvSpPr><xdr:cNvPr id=\"{}\" name=\"{}\"/><xdr:cNvSpPr txBox=\"1\"/></xdr:nvSpPr>\
<xdr:spPr><a:xfrm><a:off x=\"0\" y=\"0\"/><a:ext cx=\"0\" cy=\"0\"/></a:xfrm>{sp_pr}</xdr:spPr>\
<xdr:txBody><a:bodyPr wrap=\"{}\" rtlCol=\"0\" anchor=\"t\"/><a:lstStyle/>{body}</xdr:txBody>\
</xdr:sp><xdr:clientData/></xdr:twoCellAnchor>",
        tb.from.0,
        tb.from.1,
        tb.from.2,
        tb.from.3,
        tb.to.0,
        tb.to.1,
        tb.to.2,
        tb.to.3,
        tb.id,
        esc(&tb.name),
        if tb.wrap { "square" } else { "none" }
    )
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

const NS: &str = "xmlns:xdr=\"http://schemas.openxmlformats.org/drawingml/2006/spreadsheetDrawing\" \
xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" \
xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"";

/// 新建一个只放文本框的 drawing 部件
pub fn new_drawing_xml(list: &[TextBox]) -> String {
    let mut body = String::new();
    for tb in list {
        body.push_str(&anchor_xml(tb));
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n<xdr:wsDr {NS}>{body}</xdr:wsDr>"
    )
}

/// 把 drawing XML 里所有形状（`<xdr:sp>` 锚点）换成模型里的这一批，图片/图表原样保留
pub fn rewrite_drawing(xml: &str, list: &[TextBox]) -> String {
    let mut kept = strip_shape_anchors(xml);
    let mut body = String::new();
    for tb in list {
        body.push_str(&anchor_xml(tb));
    }
    if body.is_empty() {
        return kept;
    }
    if let Some(i) = kept.rfind("</xdr:wsDr>") {
        kept.insert_str(i, &body);
    } else if let Some(i) = kept.rfind("</wsDr>") {
        kept.insert_str(i, &body);
    }
    kept
}

fn strip_shape_anchors(xml: &str) -> String {
    let mut x = xml.to_string();
    for tag in ["twoCellAnchor", "oneCellAnchor"] {
        for prefix in ["xdr:", ""] {
            let open = format!("<{prefix}{tag}");
            let close = format!("</{prefix}{tag}>");
            let mut from = 0usize;
            loop {
                let Some(rel) = x[from..].find(&open) else { break };
                let start = from + rel;
                let Some(endrel) = x[start..].find(&close) else { break };
                let end = start + endrel + close.len();
                let chunk = x[start..end].to_string();
                let is_shape = (chunk.contains("<xdr:sp") || chunk.contains("<sp "))
                    && !chunk.contains("<xdr:pic")
                    && !chunk.contains("<xdr:graphicFrame");
                if is_shape {
                    x.replace_range(start..end, "");
                    from = start;
                } else {
                    from = end;
                }
            }
        }
    }
    x
}

// ---------------------------------------------------------------- 存

fn rels_part(sheet_part: &str) -> String {
    match sheet_part.rsplit_once('/') {
        Some((d, b)) => format!("{d}/_rels/{b}.rels"),
        None => format!("_rels/{sheet_part}.rels"),
    }
}

fn has_part(path: &Path, name: &str) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let Ok(mut zip) = zip::ZipArchive::new(file) else {
        return false;
    };
    zip.by_name(name).is_ok()
}

fn next_part_no(path: &Path, prefix: &str) -> u32 {
    let Ok(file) = std::fs::File::open(path) else {
        return 1;
    };
    let Ok(mut zip) = zip::ZipArchive::new(file) else {
        return 1;
    };
    let mut max = 0u32;
    for i in 0..zip.len() {
        let Ok(f) = zip.by_index(i) else { continue };
        let name = f.name().to_string();
        if let Some(rest) = name.strip_prefix(prefix) {
            let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(v) = digits.parse::<u32>() {
                max = max.max(v);
            }
        }
    }
    max + 1
}

/// 找到（没有就建）工作表对应的 drawing 部件；返回部件名与是否需要补关系
fn ensure_drawing(
    path: &Path,
    sheet_part: &str,
    xml: &str,
) -> anyhow::Result<(String, String, Option<String>)> {
    // 已有 <drawing r:id> → 从 rels 拿 Target
    if let Some(tag) = find_tag(xml, "drawing") {
        if let Some(rid) = attr(&tag, "r:id") {
            let rels = crate::hyperlinks::read_rels(path, sheet_part);
            if let Some((target, _)) = rels.get(&rid) {
                let part = resolve(sheet_part, target);
                let rels_p = rels_part(&part);
                return Ok((part, rels_p, None));
            }
        }
    }
    let no = next_part_no(path, "xl/drawings/drawing");
    let part = format!("xl/drawings/drawing{no}.xml");
    let rels_p = rels_part(&part);
    let rid = format!("rIdDrawTb{no}");
    Ok((part, rels_p, Some(rid)))
}

fn resolve(sheet_part: &str, target: &str) -> String {
    // drawing 的 Target 一般是 ../drawings/drawing1.xml
    if let Some(rest) = target.strip_prefix("../") {
        let dir = sheet_part.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
        // 工作表在 xl/worksheets 下，../drawings → xl/drawings
        let base = dir.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
        return format!("{base}/{rest}");
    }
    if target.starts_with('/') {
        return target.trim_start_matches('/').to_string();
    }
    match sheet_part.rsplit_once('/') {
        Some((d, _)) => format!("{d}/{target}"),
        None => target.to_string(),
    }
}

/// 保存收尾：把文本框写回各表（只处理 dirty 的表）
pub fn apply(path: &Path, wb: &crate::model::Workbook) -> anyhow::Result<()> {
    let targets = crate::patch::sheet_targets(path)?;
    let mut additions: Vec<(String, Vec<u8>)> = Vec::new();
    // 需要回填的已有部件
    let mut patch_rels: Vec<(String, String)> = Vec::new();
    let mut patch_sheet: Vec<(String, String)> = Vec::new();
    let mut ct_parts: Vec<String> = Vec::new();
    for sheet in &wb.sheets {
        if !sheet.textboxes_dirty {
            continue;
        }
        let Some(sheet_part) = targets.get(&sheet.name) else {
            continue;
        };
        let sheet_xml = crate::fixups::read_part_file(path, sheet_part)
            .and_then(|b| String::from_utf8(b).ok())
            .unwrap_or_default();
        let (draw_part, draw_rels, rid) = ensure_drawing(path, sheet_part, &sheet_xml)?;
        // drawing 本体
        let body = match crate::fixups::read_part_file(path, &draw_part)
            .and_then(|b| String::from_utf8(b).ok())
        {
            Some(xml) => rewrite_drawing(&xml, &sheet.textboxes),
            None => new_drawing_xml(&sheet.textboxes),
        };
        if has_part(path, &draw_part) {
            patch_sheet_drawing(&mut patch_sheet, &draw_part, body);
        } else {
            additions.push((draw_part.clone(), body.into_bytes()));
            // 新 drawing 部件的关系表（空表也得有，Excel 才不会报缺部件）
            additions.push((
                draw_rels.clone(),
                "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"></Relationships>"
                    .as_bytes()
                    .to_vec(),
            ));
        }
        ct_parts.push(draw_part.clone());
        // 工作表关系 + <drawing r:id>
        if let Some(rid) = rid {
            let rels_name = rels_part(sheet_part);
            let mut rels_xml = crate::fixups::read_part_file(path, &rels_name)
                .and_then(|b| String::from_utf8(b).ok())
                .unwrap_or_else(|| {
                    "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"></Relationships>"
                        .to_string()
                });
            let target = format!(
                "../{}",
                draw_part.strip_prefix("xl/").unwrap_or(&draw_part)
            );
            let rel = format!(
                "<Relationship Id=\"{rid}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/drawing\" Target=\"{target}\"/>"
            );
            if !rels_xml.contains(&format!("Id=\"{rid}\"")) {
                if rels_xml.contains("</Relationships>") {
                    rels_xml = rels_xml.replace("</Relationships>", &format!("{rel}</Relationships>"));
                } else {
                    rels_xml.push_str(&rel);
                }
            }
            if has_part(path, &rels_name) {
                patch_rels.push((rels_name, rels_xml));
            } else {
                additions.push((rels_name, rels_xml.into_bytes()));
            }
            patch_sheet.push((sheet_part.clone(), insert_drawing_tag(&sheet_xml, &rid)));
        }
    }
    if additions.is_empty() && patch_rels.is_empty() && patch_sheet.is_empty() {
        return Ok(());
    }
    crate::chart_write::rewrite_with_additions_pub(path, additions, move |part, buf| {
        for (name, body) in &patch_rels {
            if part == name {
                return body.clone().into_bytes();
            }
        }
        for (name, body) in &patch_sheet {
            if part == name {
                return body.clone().into_bytes();
            }
        }
        if part == "[Content_Types].xml" {
            if let Ok(xml) = String::from_utf8(buf.clone()) {
                let mut x = xml;
                for d in &ct_parts {
                    let name = d.rsplit_once('/').map(|(_, b)| b).unwrap_or(d);
                    if !x.contains(&format!("PartName=\"/{d}\"")) && !x.contains(name) {
                        x = x.replace(
                            "</Types>",
                            &format!(
                                "<Override PartName=\"/{d}\" ContentType=\"application/vnd.openxmlformats-officedocument.drawing+xml\"/></Types>"
                            ),
                        );
                    }
                }
                return x.into_bytes();
            }
        }
        buf
    })?;
    Ok(())
}

/// 已有 drawing 部件的改写记到 patch_sheet 队列（同一个部件只写一次）
fn patch_sheet_drawing(queue: &mut Vec<(String, String)>, part: &str, body: String) {
    if let Some(e) = queue.iter_mut().find(|(n, _)| n == part) {
        e.1 = body;
    } else {
        queue.push((part.to_string(), body));
    }
}

/// 在 worksheet 里插入 `<drawing r:id>`（schema 顺序：legacyDrawing/tableParts/extLst 之前）
fn insert_drawing_tag(xml: &str, rid: &str) -> String {
    if xml.contains("<drawing ") || xml.contains("<drawing/>") {
        return xml.to_string();
    }
    let tag = format!(
        "<drawing xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" r:id=\"{rid}\"/>"
    );
    let mut pos = xml.find("</worksheet>").unwrap_or(xml.len());
    for t in ["<legacyDrawing", "<tableParts", "<extLst"] {
        if let Some(i) = xml.find(t) {
            pos = pos.min(i);
        }
    }
    format!("{}{}{}", &xml[..pos], tag, &xml[pos..])
}

// ---------------------------------------------------------------- 载入

/// 读出某张表的文本框
pub fn load(path: &Path, sheet_part: &str) -> Vec<TextBox> {
    let Some(xml) = crate::fixups::read_part_file(path, sheet_part)
        .and_then(|b| String::from_utf8(b).ok())
    else {
        return Vec::new();
    };
    let Some(tag) = find_tag(&xml, "drawing") else {
        return Vec::new();
    };
    let Some(rid) = attr(&tag, "r:id") else {
        return Vec::new();
    };
    let rels = crate::hyperlinks::read_rels(path, sheet_part);
    let Some((target, _)) = rels.get(&rid) else {
        return Vec::new();
    };
    let part = resolve(sheet_part, target);
    let Some(draw) = crate::fixups::read_part_file(path, &part)
        .and_then(|b| String::from_utf8(b).ok())
    else {
        return Vec::new();
    };
    parse_drawing(&draw)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHEET: &str = "<worksheet><sheetData/><drawing r:id=\"rId1\"/></worksheet>";

    #[test]
    fn rewrite_keeps_pictures() {
        let xml = format!(
            "<?xml version=\"1.0\"?><xdr:wsDr {NS}>\
<xdr:twoCellAnchor><xdr:from><xdr:col>0</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>0</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:from>\
<xdr:to><xdr:col>2</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>2</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:to>\
<xdr:sp><xdr:nvSpPr><xdr:cNvPr id=\"2\" name=\"文本框 1\"/><xdr:cNvSpPr txBox=\"1\"/></xdr:nvSpPr>\
<xdr:spPr/><xdr:txBody><a:p><a:r><a:t>旧文字</a:t></a:r></a:p></xdr:txBody></xdr:sp></xdr:twoCellAnchor>\
<xdr:oneCellAnchor><xdr:from><xdr:col>3</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>3</xdr:row><xdr:rowOff>0</xdr:rowOff></xdr:from>\
<xdr:ext cx=\"100\" cy=\"100\"/><xdr:pic><xdr:blipFill><a:blip r:embed=\"rId9\"/></xdr:blipFill></xdr:pic></xdr:oneCellAnchor>\
</xdr:wsDr>"
        );
        let tb = TextBox {
            text: "新文字".into(),
            ..Default::default()
        };
        let out = rewrite_drawing(&xml, std::slice::from_ref(&tb));
        assert!(out.contains("新文字"), "新文字要写进去");
        assert!(!out.contains("旧文字"), "旧形状要被替换掉");
        assert!(out.contains("r:embed=\"rId9\""), "图片锚点必须保留");
        assert_eq!(out.matches("<xdr:pic>").count(), 1);
    }

    #[test]
    fn parse_roundtrip() {
        let tb = TextBox {
            id: 7,
            name: "文本框 2".into(),
            from: (1, 5000, 2, 6000),
            to: (4, 10000, 8, 12000),
            text: "第一行\n第二行".into(),
            size: 14.0,
            bold: true,
            italic: false,
            color: [0xFF, 0, 0],
            fill: Some([0xFF, 0xFF, 0]),
            line: None,
            align: 1,
            wrap: true,
        };
        let xml = new_drawing_xml(std::slice::from_ref(&tb));
        let back = parse_drawing(&xml);
        assert_eq!(back.len(), 1);
        let b = &back[0];
        assert_eq!(b.from, tb.from);
        assert_eq!(b.to, tb.to);
        assert_eq!(b.text, tb.text);
        assert!((b.size - 14.0).abs() < 0.01);
        assert!(b.bold);
        assert_eq!(b.color, [0xFF, 0, 0]);
        assert_eq!(b.fill, Some([0xFF, 0xFF, 0]));
        assert_eq!(b.line, None, "无边框要能读回");
        assert_eq!(b.align, 1);
    }

    #[test]
    fn insert_drawing_respects_schema_order() {
        let xml = "<worksheet><sheetData/><tableParts count=\"0\"/><extLst></extLst></worksheet>";
        let out = insert_drawing_tag(xml, "rId5");
        let d = out.find("<drawing").unwrap();
        assert!(d < out.find("<tableParts").unwrap(), "drawing 要在 tableParts 前");
        assert!(d < out.find("<extLst").unwrap());
        // 已经有时不重复插
        let again = insert_drawing_tag(&out, "rId5");
        assert_eq!(again.matches("<drawing ").count(), 1);
        let _ = SHEET;
    }

    #[test]
    fn rect_px_roundtrip() {
        let mut cols = Axis::new(72.0);
        let mut rows = Axis::new(19.0);
        cols.set(2, 120.0);
        rows.set(3, 40.0);
        let mut tb = TextBox::default();
        tb.set_rect_px(&cols, &rows, 200.0, 60.0, 180.0, 90.0);
        let (x, y, w, h) = tb.rect_px(&cols, &rows);
        assert!((x - 200.0).abs() < 1.5, "x={x}");
        assert!((y - 60.0).abs() < 1.5, "y={y}");
        assert!((w - 180.0).abs() < 2.0, "w={w}");
        assert!((h - 90.0).abs() < 2.0, "h={h}");
    }
}
