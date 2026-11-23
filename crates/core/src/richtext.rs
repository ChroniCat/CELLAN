//! 单元格富文本：一格里的多段文字，各自 bold/italic/color/size/font。
//!
//! 存储用 inlineStr（`<is><r>…</r></is>`）：不必动 sharedStrings 索引，
//! Excel 能正常读；缺点是 Excel 自己写的 sharedStrings 富文本目前只按纯文本读入。

#[derive(Clone, Debug, PartialEq)]
pub struct RichRun {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub color: Option<[u8; 3]>,
    pub size: Option<f32>,
    pub font: Option<String>,
}

impl RichRun {
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            bold: false,
            italic: false,
            color: None,
            size: None,
            font: None,
        }
    }

    pub fn is_plain(&self) -> bool {
        !self.bold && !self.italic && self.color.is_none() && self.size.is_none() && self.font.is_none()
    }
}

pub fn runs_text(runs: &[RichRun]) -> String {
    runs.iter().map(|r| r.text.as_str()).collect()
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn color_hex(c: [u8; 3]) -> String {
    format!("FF{:02X}{:02X}{:02X}", c[0], c[1], c[2])
}

/// 生成 `<is>…</is>`（不含外层 c 元素）
pub fn runs_to_inline(runs: &[RichRun]) -> String {
    let mut out = String::from("<is>");
    for r in runs {
        out.push_str("<r>");
        let mut props = String::new();
        if r.bold {
            props.push_str("<b/>");
        }
        if r.italic {
            props.push_str("<i/>");
        }
        if let Some(c) = r.color {
            props.push_str(&format!("<color rgb=\"{}\"/>", color_hex(c)));
        }
        if let Some(sz) = r.size {
            props.push_str(&format!("<sz val=\"{sz}\"/>"));
        }
        if let Some(f) = &r.font {
            props.push_str(&format!("<rFont val=\"{}\"/>", esc(f).replace('"', "&quot;")));
        }
        if !props.is_empty() {
            out.push_str(&format!("<rPr>{props}</rPr>"));
        }
        out.push_str(&format!("<t xml:space=\"preserve\">{}</t></r>", esc(&r.text)));
    }
    if runs.iter().all(|r| r.is_plain()) {
        // 全普通时写一个 <t> 就够，省字节
        return format!("<is><t xml:space=\"preserve\">{}</t></is>", esc(&runs_text(runs)));
    }
    out.push_str("</is>");
    out
}

/// 从 `<is>…</is>` 片段解析 runs（容错：解析不了的段按纯文本并入上一段）
pub fn parse_inline(xml: &str) -> Option<Vec<RichRun>> {
    let start = xml.find("<is")?;
    let end = xml.rfind("</is>")? + 5;
    let body = &xml[start..end];
    let mut runs: Vec<RichRun> = Vec::new();
    let mut from = 0usize;
    while let Some(i) = body[from..].find("<r>") {
        let i = from + i;
        let Some(j) = body[i..].find("</r>").map(|e| i + e + 4) else {
            break;
        };
        let seg = &body[i..j];
        let props = seg.find("<rPr>").and_then(|p| seg[p..].find("</rPr>").map(|e| (p, p + e)));
        let (bold, italic, color, size, font) = match props {
            Some((a, b)) => {
                let pr = &seg[a..b];
                let color = pr
                    .find("<color rgb=\"")
                    .and_then(|p| {
                        let rest = &pr[p + 12..];
                        let q = rest.find('"')?;
                        let hex = &rest[..q];
                        let hex = hex.strip_prefix("FF").filter(|h| h.len() == 6).unwrap_or(hex);
                        if hex.len() >= 6 {
                            let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
                            let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
                            let b2 = u8::from_str_radix(&hex[4..6], 16).ok()?;
                            Some([r, g, b2])
                        } else {
                            None
                        }
                    });
                let size = pr.find("<sz val=\"").and_then(|p| {
                    let rest = &pr[p + 9..];
                    let q = rest.find('"')?;
                    rest[..q].parse::<f32>().ok()
                });
                let font = pr.find("<rFont val=\"").and_then(|p| {
                    let rest = &pr[p + 12..];
                    let q = rest.find('"')?;
                    Some(unescape(&rest[..q]))
                });
                (
                    pr.contains("<b/>") || pr.contains("<b val=\"1\""),
                    pr.contains("<i/>") || pr.contains("<i val=\"1\""),
                    color,
                    size,
                    font,
                )
            }
            None => (false, false, None, None, None),
        };
        let text = seg
            .find("<t")
            .and_then(|p| {
                let gt = seg[p..].find('>')? + p + 1;
                let lt = seg[gt..].find("</t>")? + gt;
                Some(unescape(&seg[gt..lt]))
            })
            .unwrap_or_default();
        if !text.is_empty() {
            runs.push(RichRun {
                text,
                bold,
                italic,
                color,
                size,
                font,
            });
        }
        from = j;
    }
    if runs.is_empty() {
        // 只有一个 <t>
        let text = body
            .find("<t")
            .and_then(|p| {
                let gt = body[p..].find('>')? + p + 1;
                let lt = body[gt..].find("</t>")? + gt;
                Some(unescape(&body[gt..lt]))
            })
            .unwrap_or_default();
        if text.is_empty() {
            return None;
        }
        return Some(vec![RichRun::plain(text)]);
    }
    Some(runs)
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// 解析 sharedStrings.xml：返回 (整段文字, runs) 列表。
/// Excel 自己的富文本都在这里，加载时按文字回填 `Sheet.rich`。
pub fn sst_runs(xml: &str) -> Vec<(String, Vec<RichRun>)> {
    let mut out = Vec::new();
    let mut from = 0usize;
    while let Some(rel) = xml[from..].find("<si") {
        let i = from + rel;
        // <si> 或 <si ...>，排除 <silly
        let after = xml[i + 3..].chars().next();
        if !matches!(after, Some('>') | Some(' ') | Some('/')) {
            from = i + 3;
            continue;
        }
        let Some(close) = xml[i..].find("</si>").map(|e| i + e + 5) else {
            break;
        };
        let body = &xml[i + 3..close - 5];
        let wrapped = format!("<is>{body}</is>");
        if let Some(runs) = parse_inline(&wrapped) {
            let text = runs_text(&runs);
            if !text.is_empty() {
                out.push((text, runs));
            }
        }
        from = close;
    }
    out
}

#[cfg(test)]
mod sst_tests {
    use super::*;

    #[test]
    fn parses_shared_string_runs() {
        let xml = r#"<sst count="1"><si><r><rPr><b/><color rgb="FFFF0000"/><sz val="14"/><rFont val="黑体"/></rPr><t>加粗</t></r><r><t xml:space="preserve">普通</t></r></si></sst>"#;
        let all = sst_runs(xml);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].0, "加粗普通");
        assert_eq!(all[0].1.len(), 2);
        assert!(all[0].1[0].bold);
        assert_eq!(all[0].1[0].color, Some([255, 0, 0]));
        assert_eq!(all[0].1[0].font.as_deref(), Some("黑体"));
        assert!(!all[0].1[1].bold);
    }

    #[test]
    fn plain_shared_string_is_single_run() {
        let xml = r#"<sst><si><t>纯文本</t></si></sst>"#;
        let all = sst_runs(xml);
        assert_eq!(all[0].0, "纯文本");
        assert!(all[0].1.iter().all(|r| r.is_plain()));
    }
}

/// 把工作表 XML 里指定单元格改成 inlineStr 富文本。
/// 只处理 `<c r="A1" ...>…</c>` 或自闭合的 `<c r="A1" .../>`。
pub fn patch_sheet_rich(
    xml: &str,
    cells: &std::collections::HashMap<(u32, u32), Vec<RichRun>>,
) -> String {
    let mut out = xml.to_string();
    for ((r, c), runs) in cells.iter() {
        if runs.is_empty() {
            continue;
        }
        let name = crate::model::cell_name(*r, *c);
        let pat = format!("<c r=\"{name}\"");
        let Some(i) = out.find(&pat) else { continue };
        // 找该 c 元素的结束
        let Some(gt) = out[i..].find('>').map(|e| i + e) else {
            continue;
        };
        let self_closing = out[i..=gt].ends_with("/>");
        let end = if self_closing {
            gt + 1
        } else {
            match out[gt..].find("</c>") {
                Some(e) => gt + e + 4,
                None => continue,
            }
        };
        let open = &out[i..=gt];
        // 保留原有样式属性 s="…"，去掉 t=… 和内容
        let mut attrs = String::new();
        if let Some(sp) = open.find(" s=\"") {
            let rest = &open[sp + 4..];
            if let Some(q) = rest.find('"') {
                attrs.push_str(" s=\"");
                attrs.push_str(&rest[..q]);
                attrs.push('"');
            }
        }
        let new = format!(
            "<c r=\"{name}\"{attrs} t=\"inlineStr\">{}</c>",
            runs_to_inline(runs)
        );
        out.replace_range(i..end, &new);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_roundtrip() {
        let runs = vec![
            RichRun {
                text: "Hello ".into(),
                bold: true,
                italic: false,
                color: Some([255, 0, 0]),
                size: Some(14.0),
                font: None,
            },
            RichRun::plain("world"),
        ];
        let xml = runs_to_inline(&runs);
        assert!(xml.contains("<b/>"));
        assert!(xml.contains("FFFF0000"));
        let back = parse_inline(&xml).expect("应能解析");
        assert_eq!(back.len(), 2);
        assert_eq!(back[0].text, "Hello ");
        assert!(back[0].bold);
        assert_eq!(back[0].color, Some([255, 0, 0]));
        assert_eq!(back[1].text, "world");
    }

    #[test]
    fn patch_replaces_cell_and_keeps_style() {
        let mut map = std::collections::HashMap::new();
        map.insert((0, 0), vec![RichRun::plain("hi")]);
        let xml = r#"<sheetData><row r="1"><c r="A1" s="3"><v>1</v></c><c r="B1"/></row></sheetData>"#;
        let out = patch_sheet_rich(xml, &map);
        assert!(out.contains(r#"<c r="A1" s="3" t="inlineStr"><is><t xml:space="preserve">hi</t></is>"#));
        assert!(out.contains("<c r=\"B1\"/>"));
    }
}

fn parse_a1(s: &str) -> Option<(u32, u32)> {
    let mut c = 0u32;
    let mut i = 0usize;
    let bytes = s.as_bytes();
    while i < bytes.len() && bytes[i].is_ascii_alphabetic() {
        c = c * 26 + (bytes[i].to_ascii_uppercase() - b'A' + 1) as u32;
        i += 1;
    }
    if i == 0 || i == bytes.len() {
        return None;
    }
    let r = s[i..].parse::<u32>().ok()?;
    if r == 0 {
        return None;
    }
    Some((r - 1, c - 1))
}

/// 从工作表 XML 里取出所有 inlineStr 富文本单元格
pub fn inline_cells(xml: &str) -> std::collections::HashMap<(u32, u32), Vec<RichRun>> {
    let mut out = std::collections::HashMap::new();
    let mut from = 0usize;
    while let Some(i) = xml[from..].find("<c r=\"") {
        let i = from + i;
        let rest = &xml[i + 6..];
        let Some(q) = rest.find('"') else { break };
        let name = &rest[..q];
        let Some((r, c)) = parse_a1(name) else {
            from = i + 6;
            continue;
        };
        let Some(gt) = xml[i..].find('>').map(|e| i + e) else { break };
        let open = &xml[i..=gt];
        if !open.contains("t=\"inlineStr\"") {
            from = gt;
            continue;
        }
        let Some(end) = xml[gt..].find("</c>").map(|e| gt + e + 4) else { break };
        let body = &xml[gt..end];
        if let Some(pos) = body.find("<is") {
            if let Some(runs) = parse_inline(&body[pos..]) {
                if !runs.is_empty() {
                    out.insert((r, c), runs);
                }
            }
        }
        from = end;
    }
    out
}

/// 对字符区间 [start, end) 套格式；区间会切开 run 边界
pub fn apply_range(runs: &mut Vec<RichRun>, start: usize, end: usize, f: &dyn Fn(&mut RichRun)) {
    if start >= end {
        return;
    }
    let mut out: Vec<RichRun> = Vec::new();
    let mut pos = 0usize;
    for run in runs.drain(..) {
        let len = run.text.chars().count();
        let (a, b) = (pos, pos + len);
        pos = b;
        if b <= start || a >= end {
            out.push(run);
            continue;
        }
        let cut_a = start.saturating_sub(a);
        let cut_b = (end.min(b)) - a;
        let chars: Vec<char> = run.text.chars().collect();
        if cut_a > 0 {
            out.push(RichRun {
                text: chars[..cut_a].iter().collect(),
                ..run.clone()
            });
        }
        let mut mid = run.clone();
        mid.text = chars[cut_a..cut_b].iter().collect();
        f(&mut mid);
        if !mid.text.is_empty() {
            out.push(mid);
        }
        if cut_b < chars.len() {
            out.push(RichRun {
                text: chars[cut_b..].iter().collect(),
                ..run
            });
        }
    }
    // 合并相邻同样式的段
    let mut merged: Vec<RichRun> = Vec::new();
    for r in out {
        if let Some(last) = merged.last_mut() {
            let same = last.bold == r.bold
                && last.italic == r.italic
                && last.color == r.color
                && last.size == r.size
                && last.font == r.font;
            if same {
                last.text.push_str(&r.text);
                continue;
            }
        }
        merged.push(r);
    }
    *runs = merged;
}

#[cfg(test)]
mod range_tests {
    use super::*;

    #[test]
    fn formats_only_the_selection() {
        let mut runs = vec![RichRun::plain("abcdef")];
        apply_range(&mut runs, 2, 4, &|r| r.bold = true);
        assert_eq!(runs.len(), 3);
        assert_eq!(runs[0].text, "ab");
        assert_eq!(runs[1].text, "cd");
        assert!(runs[1].bold);
        assert_eq!(runs[2].text, "ef");
        assert!(!runs[2].bold);
    }
}
