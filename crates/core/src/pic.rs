//! 图片（drawing 里的 `<xdr:pic>`）：找出工作表上的图，给出位置与部件路径。
//!
//! 结构：sheet → `<drawing r:id>` → drawingN.xml → 每个 anchor 里的 `<xdr:pic>` →
//! `<a:blip r:embed="rIdN"/>` → drawing rels → `../media/imageN.png`

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;

#[derive(Clone, Debug, PartialEq)]
pub struct Picture {
    /// 落在哪几张格上 (r1, c1, r2, c2)
    pub rect: (u32, u32, u32, u32),
    /// zip 里的部件名，例如 xl/media/image1.jpeg
    pub part: String,
}

fn attr(tag: &str, key: &str) -> Option<String> {
    let pat = format!("{key}=\"");
    let i = tag.find(&pat)? + pat.len();
    let rest = &tag[i..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

/// 每个 anchor 里的 from/to 格坐标
fn cell_of(chunk: &str, which: &str) -> Option<(u32, u32)> {
    let start = chunk.find(&format!("<xdr:{which}"))
        .or_else(|| chunk.find(&format!("<{which}")))
        .or_else(|| chunk.find(&format!(":{which}")))
        .or_else(|| chunk.find(&format!("<{which}>")))?;
    let seg = &chunk[start..];
    let end = seg.find(&format!("</xdr:{which}>"))
        .or_else(|| seg.find(&format!("</{which}>")))
        .unwrap_or(seg.len());
    let body = &seg[..end];
    // <xdr:col>1</xdr:col> —— 是元素文本，不是属性
    let text = |key: &str| -> Option<u32> {
        let i = body.find(&format!("<xdr:{key}>")).or_else(|| body.find(&format!("<{key}>")))?;
        let start = i + body[i..].find('>')? + 1;
        let rest = &body[start..];
        let end = rest.find('<')?;
        rest[..end].trim().parse().ok()
    };
    let c = text("col")?;
    let r = text("row")?;
    Some((r, c))
}

/// 从 drawing XML 里挑出图片（不含图表）
pub fn parse_drawing(xml: &str) -> Vec<(u32, u32, u32, u32, String)> {
    let mut out = Vec::new();
    // 逐个 anchor 扫：带 xdr: 前缀和不带的都要认
    for tag in ["twoCellAnchor", "oneCellAnchor", "absoluteAnchor"] {
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
                // 只要图片，图表（graphicFrame）不算
                let is_pic = chunk.contains("<pic") || chunk.contains("<xdr:pic");
                if is_pic {
                    if let (Some((fr, fc)), Some(rid)) = (
                        cell_of(chunk, "from"),
                        attr(chunk, "r:embed").or_else(|| attr(chunk, "embed")),
                    ) {
                        let (tr, tc) = cell_of(chunk, "to").unwrap_or_else(|| {
                            // oneCellAnchor：按 ext 的 EMU 估跨度（1 px = 9525 EMU）
                            let cx = attr(chunk, "cx")
                                .and_then(|v| v.parse::<f32>().ok())
                                .unwrap_or(1_900_000.0)
                                / 9525.0;
                            let cy = attr(chunk, "cy")
                                .and_then(|v| v.parse::<f32>().ok())
                                .unwrap_or(1_000_000.0)
                                / 9525.0;
                            (
                                fr + (cy / 22.0).round().max(1.0) as u32,
                                fc + (cx / 104.0).round().max(1.0) as u32,
                            )
                        });
                        out.push((fr, fc, tr.max(fr + 1), tc.max(fc + 1), rid));
                    }
                }
                from = end;
                if from >= xml.len() {
                    break;
                }
            }
        }
    }
    out
}

/// 读 drawing 的关系表：rId → 部件名（相对 xl/）
fn rels_map(xml: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    let mut rest = xml;
    while let Some(pos) = rest.find("<Relationship") {
        let end = rest[pos..].find("/>").map(|e| pos + e).unwrap_or(rest.len());
        let tag = &rest[pos..end];
        if let (Some(id), Some(target)) = (attr(tag, "Id"), attr(tag, "Target")) {
            m.insert(id, target);
        }
        rest = &rest[end.min(rest.len())..];
    }
    m
}

/// 读整本文件：表名 → 图片列表
pub fn load(path: &Path) -> HashMap<String, Vec<Picture>> {
    let mut out: HashMap<String, Vec<Picture>> = HashMap::new();
    let Ok(targets) = crate::patch::sheet_targets(path) else {
        return out;
    };
    let Ok(file) = std::fs::File::open(path) else {
        return out;
    };
    let Ok(mut zip) = zip::ZipArchive::new(file) else {
        return out;
    };

    for (sheet_name, part) in targets.iter() {
        let rels_path = part.replace("worksheets/", "worksheets/_rels/") + ".rels";
        let mut rels_xml = String::new();
        {
            let Ok(mut f) = zip.by_name(&rels_path) else { continue };
            if f.read_to_string(&mut rels_xml).is_err() {
                continue;
            }
        }
        let rels = rels_map(&rels_xml);
        // 找 drawing 关系
        let mut drawing_part = None;
        let mut rest = rels_xml.as_str();
        while let Some(pos) = rest.find("<Relationship") {
            let end = rest[pos..].find("/>").map(|e| pos + e).unwrap_or(rest.len());
            let tag = &rest[pos..end];
            if tag.contains("/drawing\"") {
                if let Some(target) = attr(tag, "Target") {
                    // 相对路径要按 rels 所在目录解析（表可能不在 xl/worksheets/）
                    drawing_part = Some(crate::fixups::resolve_target(&rels_path, &target));
                }
            }
            rest = &rest[end.min(rest.len())..];
        }
        let Some(drawing) = drawing_part else { continue };
        let mut drawing_xml = String::new();
        {
            let Ok(mut f) = zip.by_name(&drawing) else { continue };
            if f.read_to_string(&mut drawing_xml).is_err() {
                continue;
            }
        }
        let drawing_rels_path = {
            let dir = drawing.rsplit_once('/').map(|(d, _)| d).unwrap_or("xl");
            let name = drawing.rsplit('/').next().unwrap_or("drawing1.xml");
            format!("{dir}/_rels/{name}.rels")
        };
        let mut draw_rels = HashMap::new();
        if let Ok(mut f) = zip.by_name(&drawing_rels_path) {
            let mut xml = String::new();
            if f.read_to_string(&mut xml).is_ok() {
                // Target 是相对 drawing 所在目录的
                for (k, v) in rels_map(&xml) {
                    draw_rels.insert(k, crate::fixups::resolve_target(&drawing_rels_path, &v));
                }
            }
        }
        let list: Vec<Picture> = parse_drawing(&drawing_xml)
            .into_iter()
            .filter_map(|(r1, c1, r2, c2, rid)| {
                draw_rels.get(&rid).map(|p| Picture {
                    rect: (r1, c1, r2, c2),
                    part: p.clone(),
                })
            })
            .collect();
        if !list.is_empty() {
            out.insert(sheet_name.clone(), list);
        }
    }
    let _ = rels_map;
    out
}

/// 读出一个部件的原始字节（给解码器用）
pub fn read_part(path: &Path, part: &str) -> Option<Vec<u8>> {
    let file = std::fs::File::open(path).ok()?;
    let mut zip = zip::ZipArchive::new(file).ok()?;
    let mut f = zip.by_name(part).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    Some(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_picture_anchor() {
        let xml = r#"<xdr:wsDr xmlns:xdr="x"><xdr:twoCellAnchor><xdr:from><xdr:col>1</xdr:col><xdr:colOff>0</xdr:colOff><xdr:row>2</xdr:row></xdr:from><xdr:to><xdr:col>4</xdr:col><xdr:row>9</xdr:row></xdr:to><xdr:pic><xdr:blipFill><a:blip r:embed="rId1"/></xdr:blipFill></xdr:pic></xdr:twoCellAnchor></xdr:wsDr>"#;
        let v = parse_drawing(xml);
        assert_eq!(v.len(), 1);
        assert_eq!((v[0].0, v[0].1, v[0].2, v[0].3), (2, 1, 9, 4));
        assert_eq!(v[0].4, "rId1");
    }

    #[test]
    fn ignores_charts() {
        let xml = r#"<xdr:wsDr><xdr:twoCellAnchor><xdr:from><xdr:col>0</xdr:col><xdr:row>0</xdr:row></xdr:from><xdr:graphicFrame><c:chart r:id="rId9"/></xdr:graphicFrame></xdr:twoCellAnchor></xdr:wsDr>"#;
        assert!(parse_drawing(xml).is_empty());
    }
}
