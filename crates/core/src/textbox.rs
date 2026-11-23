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

pub(crate) fn attr(tag: &str, key: &str) -> Option<String> {
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
                // 图表是 graphicFrame，图片是 xdr:pic；组合里的子形状、连接线也不归我们管，
                // 当成独立文本框会在保存时被复制/拆平（实测 shapes 4 → 6）。
                if chunk.contains("<xdr:pic")
                    || chunk.contains("<xdr:graphicFrame")
                    || chunk.contains("<xdr:grpSp")
                    || chunk.contains("<xdr:cxnSp")
                {
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

pub(crate) fn find_tag<'a>(xml: &'a str, tag: &str) -> Option<String> {
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

/// 把 drawing XML 里属于我们的形状锚点换成模型里的这一批。
///
/// 只动「纯 `<xdr:sp>` 锚点」：图片（`<xdr:pic`）、图表（`<xdr:graphicFrame`）、
/// 组合（`<xdr:grpSp`）、连接线（`<xdr:cxnSp`）整块原样保留 —— 以前只排除
/// pic/graphicFrame，组合会被拆成普通文本框。
///
/// 名字能对上的形状，如果文本和锚点几何都没变，也整块保留：我们的 `anchor_xml`
/// 写不出 Excel 的 spPr 细节（off/ext、style、effectLst），无改动的形状不该被重写。
pub fn rewrite_drawing(xml: &str, list: &[TextBox]) -> String {
    let mut out = String::with_capacity(xml.len() + 256);
    let mut rest = xml;
    let mut used = vec![false; list.len()];
    while let Some((start, _)) = next_anchor(rest) {
        out.push_str(&rest[..start]);
        let Some(len) = anchor_len(&rest[start..]) else {
            out.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let block = &rest[start..start + len];
        let foreign = block.contains("<xdr:pic")
            || block.contains("<xdr:graphicFrame")
            || block.contains("<xdr:grpSp")
            || block.contains("<xdr:cxnSp");
        let mut handled = false;
        let name = anchor_name(block);
        if let Some(name) = name.as_deref() {
            if let Some(k) = list.iter().position(|tb| tb.name == name) {
                used[k] = true;
                if foreign {
                    // 图片/图表/组合/连接线：整块保留，绝不能按文本框重写
                    out.push_str(block);
                } else if block_matches(block, &list[k]) {
                    out.push_str(block);
                } else if let Some(edited) = edit_block(block, &list[k]) {
                    // 原地改文字/字号，保住 Excel 的 spPr、prstGeom、style
                    out.push_str(&edited);
                } else {
                    out.push_str(&anchor_xml(&list[k]));
                }
                handled = true;
            }
        }
        if !handled {
            // 独立 `<xdr:sp>` 以模型为准：模型里没有它就等于用户删了（清空文本框时
            // 必须整块消失）。外来块（图片/图表/组合/连接线）无条件保留。
            let own_shape = !foreign && block.contains("<xdr:sp");
            if !own_shape {
                out.push_str(block);
            }
        }
        rest = &rest[start + len..];
    }
    out.push_str(rest);
    let mut add = String::new();
    for (k, tb) in list.iter().enumerate() {
        if !used[k] {
            add.push_str(&anchor_xml(tb));
        }
    }
    if !add.is_empty() {
        if let Some(i) = out.rfind("</xdr:wsDr>") {
            out.insert_str(i, &add);
        } else if let Some(i) = out.rfind("</wsDr>") {
            out.insert_str(i, &add);
        }
    }
    out
}

/// 找下一个锚点起始位置（两种前缀都认）
fn next_anchor(xml: &str) -> Option<(usize, usize)> {
    let mut best: Option<usize> = None;
    for tag in ["twoCellAnchor", "oneCellAnchor", "absoluteAnchor"] {
        for prefix in ["xdr:", ""] {
            let open = format!("<{prefix}{tag}");
            if let Some(i) = xml.find(&open) {
                // 排除 <twoCellAnchorXxx> 这种更长名字
                let after = xml[i + open.len()..].chars().next();
                if !matches!(after, Some(' ') | Some('>') | Some('/')) {
                    continue;
                }
                best = Some(best.map_or(i, |b: usize| b.min(i)));
            }
        }
    }
    best.map(|i| (i, 0))
}

/// 从锚点起点算出整块长度（含闭合标签）
fn anchor_len(xml: &str) -> Option<usize> {
    let mut used_tag: Option<String> = None;
    for tag in ["twoCellAnchor", "oneCellAnchor", "absoluteAnchor"] {
        for prefix in ["xdr:", ""] {
            let open = format!("<{prefix}{tag}");
            if xml.starts_with(&open) {
                used_tag = Some(format!("{prefix}{tag}"));
            }
        }
    }
    let tag = used_tag?;
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    // 自闭合
    let head_end = xml.find('>')?;
    if xml[..head_end].ends_with('/') {
        return Some(head_end + 1);
    }
    let mut depth = 1usize;
    let mut from = head_end + 1;
    let open_bytes = format!("<{tag}");
    let close_bytes = format!("</{tag}>");
    while depth > 0 {
        let next_open = xml[from..].find(&open_bytes).map(|x| from + x);
        let next_close = xml[from..].find(&close_bytes).map(|x| from + x);
        // 忽略自闭合的 open
        let next_open = next_open.filter(|&ix| {
            let end = xml[ix..].find('>').map(|e| ix + e).unwrap_or(ix);
            !xml[ix..end].ends_with('/')
        });
        match (next_open, next_close) {
            (Some(o), Some(c)) if o < c => {
                depth += 1;
                from = o + open_bytes.len();
            }
            (_, Some(c)) => {
                depth -= 1;
                from = c + close_bytes.len();
                if depth == 0 {
                    return Some(from);
                }
            }
            _ => return None,
        }
    }
    Some(from)
}

/// 命中形状的「原地改」：只换文字，并按模型调整 rPr（字号/粗体/斜体/颜色）。
/// 返回 None 表示结构不认识，让调用方退回整块重写。
fn edit_block(block: &str, tb: &TextBox) -> Option<String> {
    // 空形状（Excel 写 <a:p/>，没有 <a:t>）：把文字插进第一个段落，仍然保留整块
    if !block.contains("<a:t") {
        // <a:endParaRPr> 必须是 <a:p> 的最后一个子元素，文字要插在它前面
        let para_end = block.find("</a:p>")?;
        let p = match block[..para_end].rfind("<a:endParaRPr") {
            Some(i) => i,
            None => para_end,
        };
        let run = format!("<a:r><a:t>{}</a:t></a:r>", esc(&tb.text));
        let mut out = String::with_capacity(block.len() + run.len());
        out.push_str(&block[..p]);
        out.push_str(&run);
        out.push_str(&block[p..]);
        return Some(tune_runs(&out, tb));
    }
    let mut out = String::with_capacity(block.len() + tb.text.len());
    let mut rest = block;
    let mut done = false;
    while let Some(i) = rest.find("<a:t") {
        out.push_str(&rest[..i]);
        let seg = &rest[i..];
        let gt = seg.find('>')?;
        let close = seg.find("</a:t>")?;
        out.push_str(&seg[..=gt]);
        if !done {
            out.push_str(&esc(&tb.text));
            done = true;
        }
        out.push_str("</a:t>");
        rest = &seg[close + 6..];
    }
    out.push_str(rest);
    Some(tune_runs(&out, tb))
}

/// 把 `<a:rPr>` 的字号/粗体/斜体/颜色对齐模型；没有 rPr 就不动
fn tune_runs(xml: &str, tb: &TextBox) -> String {
    let mut out = String::with_capacity(xml.len() + 32);
    let mut rest = xml;
    while let Some(i) = rest.find("<a:rPr") {
        out.push_str(&rest[..i]);
        let seg = &rest[i..];
        let Some(gt) = seg.find('>') else {
            out.push_str(seg);
            return out;
        };
        let mut tag = seg[..=gt].to_string();
        let self_closing = tag.ends_with("/>");
        // 字号：sz 以 1/100 磅计
        if tb.size > 0.0 {
            tag = set_attr(&tag, "sz", &format!("{}", (tb.size * 100.0).round() as i64));
        }
        tag = set_attr(&tag, "b", if tb.bold { "1" } else { "0" });
        tag = set_attr(&tag, "i", if tb.italic { "1" } else { "0" });
        let _ = self_closing;
        out.push_str(&tag);
        rest = &seg[gt + 1..];
        // 颜色：紧跟着的 solidFill/srgbClr
        if let Some(rel) = rest.find("</a:rPr>") {
            let inner = &rest[..rel];
            if let Some(cp) = inner.find("<a:srgbClr val=\"") {
                let val = format!("{:02X}{:02X}{:02X}", tb.color[0], tb.color[1], tb.color[2]);
                let mut new_inner = inner.to_string();
                let p = cp + "<a:srgbClr val=\"".len();
                if let Some(q) = new_inner[p..].find('"') {
                    new_inner.replace_range(p..p + q, &val);
                }
                out.push_str(&new_inner);
            } else {
                out.push_str(inner);
            }
            out.push_str("</a:rPr>");
            rest = &rest[rel + "</a:rPr>".len()..];
        }
    }
    out.push_str(rest);
    out
}

fn set_attr(tag: &str, name: &str, value: &str) -> String {
    let pat = format!(" {name}=\"");
    if let Some(i) = tag.find(&pat) {
        let p = i + pat.len();
        if let Some(q) = tag[p..].find('"') {
            let mut t = tag.to_string();
            t.replace_range(p..p + q, value);
            return t;
        }
    }
    let self_closing = tag.ends_with("/>");
    let insert_at = if self_closing { tag.len() - 2 } else { tag.len() - 1 };
    let mut t = tag.to_string();
    t.insert_str(insert_at, &format!(" {name}=\"{value}\""));
    t
}

fn anchor_name(block: &str) -> Option<String> {
    let i = block.find("<xdr:cNvPr").or_else(|| block.find("<cNvPr"))?;
    let tag_end = block[i..].find('>').map(|e| i + e)?;
    let tag = &block[i..=tag_end];
    let p = tag.find("name=\"")? + 6;
    let rest = &tag[p..];
    let q = rest.find('"')?;
    Some(crate::model::xml_unescape(&rest[..q]))
}

/// 文本与锚点几何都没变 → 保留原块（我们的 anchor_xml 写不出 Excel 的细节）
fn block_matches(block: &str, tb: &TextBox) -> bool {
    if anchor_text(block) != tb.text {
        return false;
    }
    anchor_points(block).map(|(f, t)| f == tb.from && t == tb.to).unwrap_or(false)
}

fn anchor_text(block: &str) -> String {
    let mut out = String::new();
    let mut from = 0usize;
    loop {
        let Some(i) = block[from..].find("<a:t") else { break };
        let i = from + i;
        let Some(gt) = block[i..].find('>').map(|e| i + e) else { break };
        let Some(end) = block[gt..].find("</a:t>").map(|e| gt + e) else { break };
        out.push_str(&crate::model::xml_unescape(&block[gt + 1..end]));
        from = end + 6;
    }
    out
}

/// 取 <xdr:from> / <xdr:to> 里的四个整数（col, colOff, row, rowOff）
fn anchor_points(block: &str) -> Option<((u32, i64, u32, i64), (u32, i64, u32, i64))> {
    let from = point_after(block, "from")?;
    let to = point_after(block, "to")?;
    Some((from, to))
}

fn point_after(block: &str, tag: &str) -> Option<(u32, i64, u32, i64)> {
    let i = block.find(&format!("<xdr:{tag}>")).or_else(|| block.find(&format!("<{tag}>")))?;
    let seg = &block[i..];
    let end = seg.find(&format!("</{tag}>")).or_else(|| seg.find(&format!("</xdr:{tag}>")))?;
    let seg = &seg[..end];
    let grab = |name: &str| -> Option<i64> {
        let p = seg.find(&format!("<{name}>"))?;
        let rest = &seg[p + name.len() + 2..];
        let q = rest.find('<')?;
        rest[..q].trim().parse::<i64>().ok()
    };
    // 兼容带 xdr: 前缀的子元素
    let grab2 = |name: &str| -> Option<i64> {
        grab(name).or_else(|| grab(&format!("xdr:{name}")))
    };
    Some((
        grab2("col")? as u32,
        grab2("colOff")?,
        grab2("row")? as u32,
        grab2("rowOff")?,
    ))
}

// ---------------------------------------------------------------- 存

pub(crate) fn rels_part(sheet_part: &str) -> String {
    match sheet_part.rsplit_once('/') {
        Some((d, b)) => format!("{d}/_rels/{b}.rels"),
        None => format!("_rels/{sheet_part}.rels"),
    }
}

pub(crate) fn has_part(path: &Path, name: &str) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let Ok(mut zip) = zip::ZipArchive::new(file) else {
        return false;
    };
    zip.by_name(name).is_ok()
}

pub(crate) fn next_part_no(path: &Path, prefix: &str) -> u32 {
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
pub(crate) fn ensure_drawing(
    path: &Path,
    sheet_part: &str,
    xml: &str,
) -> anyhow::Result<(String, String, Option<String>)> {
    // 已有 <drawing r:id> → 从 rels 拿 Target
    let slot = drawing_slot(xml)?;
    anyhow::ensure!(!slot.strict, "暂不支持改写 Strict 工作表中的绘图部件");
    if let Some(rid) = slot.rid {
        let part = drawing_target(path,sheet_part,&rid,false)?;
        anyhow::ensure!(has_part(path,&part), "缺少工作表引用的绘图部件 {part}");
        let rels = rels_part(&part);
        return Ok((part, rels, None));
    }
    let no = next_part_no(path, "xl/drawings/drawing");
    let part = format!("xl/drawings/drawing{no}.xml");
    let rels_p = rels_part(&part);
    let rels = crate::hyperlinks::read_rels(path, sheet_part);
    let rid = (no..).map(|i| format!("rIdDrawTb{i}")).find(|id| !rels.contains_key(id))
        .ok_or_else(|| anyhow::anyhow!("工作表的关系标识不足"))?;
    Ok((part, rels_p, Some(rid)))
}


/// 保存收尾：把文本框写回各表（只处理 dirty 的表）
pub fn apply(path: &Path, wb: &crate::model::Workbook) -> anyhow::Result<()> {
    if !wb.sheets.iter().any(|s| s.textboxes_dirty) {
        return Ok(());
    }
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
        let sheet_bytes = crate::fixups::read_part_file(path,sheet_part).ok_or_else(|| anyhow::anyhow!("缺少工作表 {sheet_part}"))?;
        let sheet_xml = crate::xlsx::patch_xml_text(&sheet_bytes,sheet_part)?;
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
            let target = format!("../{}", draw_part.strip_prefix("xl/").unwrap_or(&draw_part));
            let rels_xml = add_drawing_relationship(&drawing_relationships(path,&rels_name)?, &rid, &target)?;
            if has_part(path, &rels_name) {
                patch_rels.push((rels_name, rels_xml));
            } else {
                additions.push((rels_name, rels_xml.into_bytes()));
            }
            patch_sheet.push((sheet_part.clone(), insert_drawing_tag(&sheet_xml, &rid)?));
        }
    }
    if additions.is_empty() && patch_rels.is_empty() && patch_sheet.is_empty() {
        return Ok(());
    }
    let targets: Vec<_> = patch_rels.iter().chain(&patch_sheet).map(|(name, _)| name.as_str())
        .chain(std::iter::once("[Content_Types].xml")).collect();
    crate::chart_write::rewrite_with_additions_pub(path, additions, &targets, |part, buf| {
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
pub(crate) fn patch_sheet_drawing(queue: &mut Vec<(String, String)>, part: &str, body: String) {
    if let Some(e) = queue.iter_mut().find(|(n, _)| n == part) {
        e.1 = body;
    } else {
        queue.push((part.to_string(), body));
    }
}

pub(crate) fn add_drawing_relationship(xml: &str, rid: &str, target: &str) -> anyhow::Result<String> {
    let doc = roxmltree::Document::parse(xml)?;
    let root = doc.root_element();
    anyhow::ensure!(root.tag_name().name() == "Relationships", "无效的工作表关系清单");
    let kind = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/drawing";
    if let Some(old) = root.children().find(|n| n.attribute("Id") == Some(rid)) {
        anyhow::ensure!(old.attribute("Type") == Some(kind) && old.attribute("Target") == Some(target),
            "工作表关系标识 {rid} 已被使用");
        return Ok(xml.to_owned());
    }
    let root_xml = &xml[root.range()];
    let root_name = root_xml[1..].split([' ', '\t','\n','\r','/','>']).next().unwrap();
    let prefix = root_name.strip_suffix("Relationships").unwrap_or("");
    // Both inputs are generated internally from part numbers.
    let child = format!("<{prefix}Relationship Id=\"{rid}\" Type=\"{kind}\" Target=\"{target}\"/>");
    let mut result = xml.to_owned();
    if root_xml.ends_with("/>") {
        result.replace_range(root.range().end-2..root.range().end,&format!(">{child}</{root_name}>"));
    } else {
        let end = root_xml.rfind("</").ok_or_else(|| anyhow::anyhow!("工作表关系清单不完整"))?;
        result.insert_str(root.range().start+end,&child);
    }
    Ok(result)
}

pub(crate) fn drawing_relationships(path: &Path, part: &str) -> anyhow::Result<String> {
    if has_part(path,part) {
        let bytes = crate::fixups::read_part_file(path,part).ok_or_else(|| anyhow::anyhow!("无法读取绘图关系 {part}"))?;
        Ok(crate::xlsx::patch_xml_text(&bytes,part)?.into_owned())
    } else {
        Ok("<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"/>".into())
    }
}

/// 在 worksheet 里插入 `<drawing r:id>`（schema 顺序：legacyDrawing/tableParts/extLst 之前）
struct DrawingSlot {
    position: usize,
    prefix: String,
    strict: bool,
    rid: Option<String>,
}

fn drawing_slot(xml: &str) -> anyhow::Result<DrawingSlot> {
    use anyhow::{ensure, Context};
    use quick_xml::{NsReader, events::Event, name::ResolveResult};
    let mut reader = NsReader::from_str(xml);
    let mut root = None;
    let mut insert = None;
    let mut rid = None;
    loop {
        let start = reader.buffer_position() as usize;
        let (ns,event) = reader.read_resolved_event()?;
        let ns = match ns {
            ResolveResult::Bound(n) => n.0,
            ResolveResult::Unbound => "",
            _ => anyhow::bail!("工作表使用未声明的命名空间"),
        };
        match event {
            Event::Start(tag) if root.is_none() => {
                ensure!(tag.local_name().as_ref() == "worksheet", "缺少 worksheet 根元素");
                let prefix = tag.name().as_ref().strip_suffix("worksheet").unwrap().to_owned();
                root = Some((ns.to_owned(),prefix));
            }
            e @ (Event::Start(_) | Event::Empty(_)) if root.is_some() => {
                let closed = matches!(e,Event::Empty(_));
                let (Event::Start(tag) | Event::Empty(tag)) = e else { unreachable!() };
                if ns == root.as_ref().unwrap().0 {
                    match tag.local_name().as_ref() {
                        "drawing" => {
                            ensure!(rid.is_none(), "工作表有重复的 drawing");
                            let expected = if ns == "http://purl.oclc.org/ooxml/spreadsheetml/main" {
                                "http://purl.oclc.org/ooxml/officeDocument/relationships"
                            } else { "http://schemas.openxmlformats.org/officeDocument/2006/relationships" };
                            let attr = tag.attributes().filter_map(Result::ok)
                                .find(|a| {
                                    let (ns,local) = reader.resolver().resolve_attribute(a.key);
                                    local.as_ref() == "id" && matches!(ns, ResolveResult::Bound(n) if n.0 == expected)
                                })
                                .context("绘图缺少关系标识")?;
                            rid = Some(attr.value.into_owned());
                        }
                        "legacyDrawing" | "legacyDrawingHF" | "drawingHF" | "picture" |
                        "oleObjects" | "controls" | "webPublishItems" | "tableParts" | "extLst" => {
                            insert.get_or_insert(start);
                        }
                        _ => {}
                    }
                }
                if !closed { reader.read_to_end(tag.name())?; }
            }
            Event::End(_) if root.is_some() => {
                let (uri,prefix) = root.unwrap();
                return Ok(DrawingSlot { position:insert.unwrap_or(start), prefix, rid,
                    strict:uri == "http://purl.oclc.org/ooxml/spreadsheetml/main" });
            }
            Event::Eof => anyhow::bail!("工作表 drawing 所属的 XML 不完整"),
            _ => {}
        }
    }
}

pub(crate) fn insert_drawing_tag(xml: &str, rid: &str) -> anyhow::Result<String> {
    let DrawingSlot {position, prefix, strict, rid: existing} = drawing_slot(xml)?;
    if let Some(existing) = existing {
        anyhow::ensure!(existing == rid, "工作表已有不同的绘图关系 {existing}");
        return Ok(xml.to_owned());
    }
    anyhow::ensure!(!strict, "暂不支持向 Strict 工作表挂接 Transitional 绘图部件");
    Ok(format!("{}<{prefix}drawing xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\" r:id=\"{rid}\"/>{}", &xml[..position], &xml[position..]))
}

// ---------------------------------------------------------------- 载入

/// 读出某张表的文本框
pub fn load(path: &Path, sheet_part: &str) -> Vec<TextBox> {
    let Some(bytes) = crate::fixups::read_part_file(path,sheet_part) else { return Vec::new() };
    let Ok(xml) = crate::xlsx::patch_xml_text(&bytes,sheet_part) else { return Vec::new() };
    load_from_sheet(path, sheet_part, &xml)
}

fn drawing_target(path: &Path, sheet_part: &str, rid: &str, strict: bool) -> anyhow::Result<String> {
    let part = rels_part(sheet_part);
    let bytes = crate::fixups::read_part(path,&part).ok_or_else(|| anyhow::anyhow!("缺少工作表关系清单 {part}"))?;
    let xml = crate::xlsx::patch_xml_text(&bytes,&part)?;
    let doc = roxmltree::Document::parse(&xml)?;
    let kind = if strict { "http://purl.oclc.org/ooxml/officeDocument/relationships/drawing" }
        else { "http://schemas.openxmlformats.org/officeDocument/2006/relationships/drawing" };
    let rel = doc.root_element().children().find(|n| n.tag_name().name() == "Relationship"
        && n.attribute("Type") == Some(kind) && n.attribute("Id") == Some(rid)
        && n.attribute("TargetMode") != Some("External")).ok_or_else(|| anyhow::anyhow!("找不到工作表的内部绘图关系 {rid}"))?;
    let target = rel.attribute("Target").ok_or_else(|| anyhow::anyhow!("缺少绘图关系目标"))?;
    Ok(crate::fixups::resolve_target(&part,target))
}

pub(crate) fn load_from_sheet(path: &Path, sheet_part: &str, xml: &str) -> Vec<TextBox> {
    if !xml.contains("drawing") { return Vec::new(); }
    let Ok(DrawingSlot {rid:Some(rid), strict,..}) = drawing_slot(xml) else { return Vec::new() };
    let Ok(part) = drawing_target(path,sheet_part,&rid,strict) else { return Vec::new() };
    let Some(bytes) = crate::fixups::read_part_file(path,&part) else { return Vec::new() };
    let Ok(draw) = crate::xlsx::patch_xml_text(&bytes,&part) else { return Vec::new() };
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
    fn attaching_a_drawing_requires_a_complete_sheet_and_the_expected_relationship() {
        assert!(insert_drawing_tag("<worksheet><sheetData>", "rId1").is_err());
        let xml = r#"<worksheet xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheetData/><drawing r:id="rId2"/></worksheet>"#;
        assert!(insert_drawing_tag(xml,"rId1").is_err());
        assert_eq!(insert_drawing_tag(xml,"rId2").unwrap(),xml);
    }

    #[test]
    fn append_to_self_closing_prefixed_drawing_relationships_preserves_the_namespace() {
        let xml = r#"<p:Relationships xmlns:p="http://schemas.openxmlformats.org/package/2006/relationships"/>"#;
        let output = add_drawing_relationship(xml,"rId1","../drawings/drawing1.xml").unwrap();
        let doc = roxmltree::Document::parse(&output).unwrap();
        let child = doc.root_element().first_element_child().unwrap();
        assert_eq!(child.tag_name().namespace(),doc.root_element().tag_name().namespace());
        assert_eq!(child.attribute("Target"),Some("../drawings/drawing1.xml"));
        assert!(add_drawing_relationship(&output,"rId1","other.xml").is_err());
    }

    #[test]
    fn prefixed_worksheet_and_relationship_preserve_the_textbox_through_save() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("prefix.xlsx");
        let mut wb = crate::Workbook::new_blank();
        wb.sheets[0].textboxes.push(TextBox { text:"before".into(),..Default::default() });
        wb.sheets[0].textboxes_dirty = true;
        crate::xlsx::save_atomic(&path,&wb).unwrap();
        let name = "xl/worksheets/sheet1.xml";
        let bytes = crate::fixups::read_part_file(&path,name).unwrap();
        let xml = String::from_utf8(bytes).unwrap()
            .replace("<worksheet ",r#"<s:worksheet xmlns:s="http://schemas.openxmlformats.org/spreadsheetml/2006/main" "#)
            .replace("</worksheet>","</s:worksheet>").replace("<drawing ","<s:drawing ")
            .replace("xmlns:r=","xmlns:rel=").replace("r:id=","rel:id=");
        assert!(xml.contains("<s:drawing ") && xml.contains("rel:id="));
        crate::fixups::write_parts(&path,&[(name.into(),xml.into_bytes())]).unwrap();
        let mut loaded = crate::xlsx::load(&path).unwrap();
        assert_eq!(loaded.sheets[0].textboxes[0].text,"before");
        loaded.sheets[0].textboxes[0].text = "after".into();
        loaded.sheets[0].textboxes_dirty = true;
        crate::xlsx::save_atomic(&path,&loaded).unwrap();
        let back = crate::xlsx::load(&path).unwrap();
        assert_eq!(back.sheets[0].textboxes.len(),1);
        assert_eq!(back.sheets[0].textboxes[0].text,"after");
    }

    #[test]
    fn strict_sheet_requires_the_strict_relationship_namespace() {
        let xml = r#"<s:worksheet xmlns:s="http://purl.oclc.org/ooxml/spreadsheetml/main" xmlns:k="http://purl.oclc.org/ooxml/officeDocument/relationships"><s:sheetData/><s:drawing k:id="a"/></s:worksheet>"#;
        assert_eq!(drawing_slot(xml).unwrap().rid.as_deref(),Some("a"));
        assert!(drawing_slot(&xml.replace("http://purl.oclc.org/ooxml/officeDocument/relationships",
            "http://schemas.openxmlformats.org/officeDocument/2006/relationships")).is_err());
        assert!(insert_drawing_tag(&xml.replace(r#"<s:drawing k:id="a"/>"#,""),"a").is_err());
    }

    #[test]
    fn insert_drawing_respects_schema_order() {
        let xml = "<worksheet><sheetData/><tableParts count=\"0\"/><extLst></extLst></worksheet>";
        let out = insert_drawing_tag(xml, "rId5").unwrap();
        let d = out.find("<drawing").unwrap();
        assert!(d < out.find("<tableParts").unwrap(), "drawing 要在 tableParts 前");
        assert!(d < out.find("<extLst").unwrap());
        // 已经有时不重复插
        let again = insert_drawing_tag(&out, "rId5").unwrap();
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
